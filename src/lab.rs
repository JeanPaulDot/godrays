//! The latency lab: measures Kyber on this PC by running a private server
//! and a view-only client against each other, and finds the fastest
//! encoder and codec pair that actually works on this graphics card.
//!
//! Two independent measurements come out of a run:
//! * **Pipeline**: Kyber's own per-frame timestamps, captured on the host to
//!   displayed by the client, split by stage (see `metrics.rs`).
//! * **Screen**: the on-screen beacon read twice from one screenshot, the
//!   original and its copy inside the stream window (see `beacon.rs`). This
//!   one also works for other streaming apps.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::beacon;
use crate::config::{AuthMode, Codec, Config, Encoder, Host, Hosting, StreamSettings, TlsMode};
use crate::display::{self, Mode};
use crate::host::{self, Server, ServerState};
use crate::kyclient::{build_args, hide_console};
use crate::logtail::LogTail;
use crate::metrics::{Collector, Summary};
use crate::winproc::JobGuard;

const WARM_UP: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScreenStats {
    pub samples: usize,
    pub median: f32,
    pub p10: f32,
    pub p90: f32,
}

impl ScreenStats {
    pub fn from_samples(mut samples: Vec<f32>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f32| samples[((samples.len() as f32 * q) as usize).min(samples.len() - 1)];
        Some(Self {
            samples: samples.len(),
            median: at(0.5),
            p10: at(0.1),
            p90: at(0.9),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabResult {
    /// What was measured, for example "Kyber AMF + HEVC" or "Other app".
    pub label: String,
    pub encoder: Option<Encoder>,
    pub codec: Option<Codec>,
    /// False when the client never displayed a frame.
    pub played: bool,
    pub pipeline: Option<PipelineStats>,
    pub screen: Option<ScreenStats>,
    /// Unix seconds.
    pub at: u64,
    /// The host display's size during the run, when known.
    #[serde(default)]
    pub screen_size: Option<[u32; 2]>,
}

/// `Summary` without the live-only fields, for saving.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipelineStats {
    pub frames: usize,
    pub encode: f32,
    pub network: f32,
    pub decode: f32,
    pub render: f32,
    pub total: f32,
    pub total_p10: f32,
    pub total_p90: f32,
    pub fps: f32,
}

impl From<Summary> for PipelineStats {
    fn from(s: Summary) -> Self {
        Self {
            frames: s.frames,
            encode: s.encode,
            network: s.network,
            decode: s.decode,
            render: s.render,
            total: s.total,
            total_p10: s.total_p10,
            total_p90: s.total_p90,
            fps: s.fps,
        }
    }
}

impl LabResult {
    /// "AMF + H.264", or the label for other measurements.
    pub fn short_label(&self) -> String {
        let encoder = |e: Encoder| match e {
            Encoder::Nvenc => "NVENC",
            Encoder::Amf => "AMF",
            Encoder::Qsv => "Quick Sync",
            Encoder::X264 => "x264",
            Encoder::Auto => "Auto",
        };
        let codec = |c: Codec| match c {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        };
        let size = match self.screen_size {
            Some([_, h]) => format!(" \u{b7} {h}p"),
            None => String::new(),
        };
        match (self.encoder, self.codec) {
            (Some(e), Some(c)) => format!("{} + {}{size}", encoder(e), codec(c)),
            _ => format!("{}{size}", self.label),
        }
    }

    pub fn headline(&self) -> String {
        match (&self.pipeline, &self.screen, self.played) {
            (_, _, false) => "no picture".to_owned(),
            (Some(p), Some(s), _) => format!(
                "{:.1} ms pipeline \u{b7} {:.0} ms on screen",
                p.total, s.median
            ),
            (Some(p), None, _) => format!("{:.1} ms (p90 {:.1})", p.total, p.total_p90),
            (None, Some(s), _) => format!("{:.0} ms on screen (p90 {:.0})", s.median, s.p90),
            (None, None, _) => "no measurement".to_owned(),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn results_path() -> PathBuf {
    Config::dir().join("latency-results.json")
}

pub fn load_results() -> Vec<LabResult> {
    std::fs::read_to_string(results_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_result(result: &LabResult) {
    let mut all = load_results();
    all.push(result.clone());
    // Keep the history short.
    let excess = all.len().saturating_sub(50);
    all.drain(..excess);
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::create_dir_all(Config::dir());
        let _ = std::fs::write(results_path(), text);
    }
}

/// Sample history for demo mode (measured on a Radeon RX 5700 XT).
pub fn demo_results() -> Vec<LabResult> {
    let at = now();
    let pipeline = |total: f32, encode: f32, render: f32| PipelineStats {
        frames: 1200,
        encode,
        network: 0.5,
        decode: 0.3,
        render,
        total,
        total_p10: total - 1.5,
        total_p90: total + 2.5,
        fps: 164.0,
    };
    let result =
        |encoder: Encoder, codec: Codec, p: PipelineStats, screen: Option<f32>, ago: u64| {
            LabResult {
                label: format!("Kyber {} + {}", encoder.label(), codec.label()),
                encoder: Some(encoder),
                codec: Some(codec),
                played: true,
                pipeline: Some(p),
                screen: screen.map(|median| ScreenStats {
                    samples: 50,
                    median,
                    p10: median - 6.0,
                    p90: median + 3.0,
                }),
                at: at.saturating_sub(ago),
                screen_size: Some([1920, 1080]),
            }
        };
    vec![
        result(
            Encoder::X264,
            Codec::H264,
            pipeline(31.8, 27.6, 2.7),
            None,
            3600,
        ),
        result(
            Encoder::Amf,
            Codec::Hevc,
            pipeline(8.6, 4.7, 2.8),
            None,
            3500,
        ),
        result(
            Encoder::Amf,
            Codec::H264,
            pipeline(9.2, 5.1, 2.9),
            Some(24.0),
            600,
        ),
    ]
}

// ------------------------------------------------------------ shared state

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Running {
        what: String,
        step: usize,
        steps: usize,
    },
    Done(Vec<LabResult>),
    Failed(String),
}

/// What the UI and the lab thread share.
pub struct Shared {
    pub stage: Mutex<Stage>,
    /// The UI shows the beacon while this is set.
    pub beacon: AtomicBool,
    pub cancel: AtomicBool,
    /// Zero point of the beacon's clock.
    pub epoch: Instant,
    /// The hosting settings to test with (priority boost, engine).
    hosting: Mutex<Hosting>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stage: Mutex::new(Stage::Running {
                what: "Starting".into(),
                step: 0,
                steps: 1,
            }),
            beacon: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            epoch: Instant::now(),
            hosting: Mutex::new(Hosting::default()),
        })
    }

    /// The beacon value to draw right now.
    pub fn beacon_value(&self) -> u16 {
        (self.epoch.elapsed().as_millis() & 0xFFFF) as u16
    }

    fn set(&self, stage: Stage) {
        *self.stage.lock().unwrap() = stage;
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// What to run.
#[derive(Clone)]
pub enum Plan {
    /// Kyber on this PC with one encoder and codec, beacon included.
    Kyber { encoder: Encoder, codec: Codec },
    /// Every working encoder and codec pair, pipeline only, fastest first.
    FindFastest,
    /// Only the beacon: the user streams this screen with another app.
    OtherApp { seconds: u64 },
}

pub struct Lab {
    pub shared: Arc<Shared>,
}

impl Lab {
    /// `display`: run at this display mode, as hosting would (restored after).
    pub fn start(
        plan: Plan,
        install_dir: Option<PathBuf>,
        client: Option<PathBuf>,
        settings: StreamSettings,
        display: Option<Mode>,
        hosting: Hosting,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Shared::new();
        *shared.hosting.lock().unwrap() = hosting;
        let worker = shared.clone();
        std::thread::spawn(move || {
            let _display = match plan {
                Plan::OtherApp { .. } => None,
                _ => display.and_then(DisplayGuard::switch),
            };
            let outcome = run_plan(&plan, install_dir, client, &settings, &worker, &wake);
            worker.beacon.store(false, Ordering::SeqCst);
            match outcome {
                Ok(results) => {
                    for result in &results {
                        save_result(result);
                    }
                    worker.set(Stage::Done(results));
                }
                Err(error) => worker.set(Stage::Failed(error)),
            }
            wake();
        });
        Self { shared }
    }

    pub fn stage(&self) -> Stage {
        self.shared.stage.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }
}

/// Encoder and codec pairs worth trying on this PC, hardware first.
pub fn candidates(gpus: &[String]) -> Vec<(Encoder, Codec)> {
    host::encoders_for_gpus(gpus)
        .into_iter()
        .flat_map(|encoder| match encoder {
            Encoder::X264 => vec![(encoder, Codec::H264)],
            _ => vec![(encoder, Codec::Hevc), (encoder, Codec::H264)],
        })
        .collect()
}

fn run_plan(
    plan: &Plan,
    install_dir: Option<PathBuf>,
    client: Option<PathBuf>,
    settings: &StreamSettings,
    shared: &Shared,
    wake: &(dyn Fn() + Send + Sync),
) -> Result<Vec<LabResult>, String> {
    let need_kyber = || -> Result<(PathBuf, PathBuf), String> {
        match (install_dir.clone(), client.clone()) {
            (Some(install), Some(client)) => Ok((install, client)),
            _ => Err("Kyber is not installed.".to_owned()),
        }
    };
    match plan {
        Plan::Kyber { encoder, codec } => {
            let (install, client) = need_kyber()?;
            shared.set(Stage::Running {
                what: format!("{} + {}", encoder.label(), codec.label()),
                step: 1,
                steps: 1,
            });
            wake();
            let result = run_kyber(
                &install, &client, *encoder, *codec, settings, 8, true, shared, wake,
            )?;
            Ok(vec![result])
        }
        Plan::FindFastest => {
            let (install, client) = need_kyber()?;
            let pairs = candidates(&host::gpu_names());
            let mut results = Vec::new();
            for (index, (encoder, codec)) in pairs.iter().enumerate() {
                if shared.cancelled() {
                    break;
                }
                shared.set(Stage::Running {
                    what: format!("{} + {}", encoder.label(), codec.label()),
                    step: index + 1,
                    steps: pairs.len(),
                });
                wake();
                results.push(run_kyber(
                    &install, &client, *encoder, *codec, settings, 5, false, shared, wake,
                )?);
            }
            // Fastest working pair first.
            results.sort_by(|a, b| {
                let key = |r: &LabResult| match (&r.pipeline, r.played) {
                    (Some(p), true) if p.frames > 30 => p.total,
                    _ => f32::MAX,
                };
                key(a).total_cmp(&key(b))
            });
            Ok(results)
        }
        Plan::OtherApp { seconds } => {
            shared.set(Stage::Running {
                what: "Other app".into(),
                step: 1,
                steps: 1,
            });
            shared.beacon.store(true, Ordering::SeqCst);
            wake();
            // Time to bring the other app's stream window up.
            std::thread::sleep(Duration::from_secs(3));
            let samples = sample_screen(Duration::from_secs(*seconds), shared);
            shared.beacon.store(false, Ordering::SeqCst);
            Ok(vec![LabResult {
                label: "Other app".into(),
                encoder: None,
                codec: None,
                played: true,
                pipeline: None,
                screen: ScreenStats::from_samples(samples),
                at: now(),
                screen_size: display::current().map(|m| [m.width, m.height]),
            }])
        }
    }
}

/// Screenshots for `duration`, returning one latency sample per screenshot
/// in which both beacons were read.
fn sample_screen(duration: Duration, shared: &Shared) -> Vec<f32> {
    let until = Instant::now() + duration;
    let mut samples = Vec::new();
    while Instant::now() < until && !shared.cancelled() {
        if let Some(frame) = beacon::capture_screen() {
            if let Some(ms) = beacon::latency(&beacon::read(&frame)) {
                samples.push(ms);
            }
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    samples
}

/// A port free for both TCP and UDP, on IPv4 and IPv6: Kyber uses one
/// number for its HTTPS control channel and its QUIC stream. Picked below
/// Windows' range for temporary ports (49152 and up), where other programs'
/// short-lived UDP sockets would collide.
fn free_port() -> Option<u16> {
    use std::net::{Ipv6Addr, UdpSocket};
    let mut seed = u16::from_le_bytes([(std::process::id() & 0xFF) as u8, (now() & 0xFF) as u8]);
    for _ in 0..200 {
        seed = seed.wrapping_mul(25173).wrapping_add(13849);
        let port = 20_000 + seed % 20_000;
        let free = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
            && TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok()
            && UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
            && UdpSocket::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok();
        if free {
            return Some(port);
        }
    }
    None
}

/// Switches the display for a run and puts it back afterwards.
struct DisplayGuard;

impl DisplayGuard {
    fn switch(mode: Mode) -> Option<Self> {
        if display::current() == Some(mode) {
            return None;
        }
        display::set(mode).ok()?;
        // Let the desktop settle before capturing it.
        std::thread::sleep(Duration::from_millis(1500));
        Some(Self)
    }
}

impl Drop for DisplayGuard {
    fn drop(&mut self) {
        display::restore();
    }
}

struct KillOnDrop(Child, Option<JobGuard>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.1.take();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_kyber(
    install: &Path,
    client: &Path,
    encoder: Encoder,
    codec: Codec,
    settings: &StreamSettings,
    seconds: u64,
    with_beacon: bool,
    shared: &Shared,
    wake: &(dyn Fn() + Send + Sync),
) -> Result<LabResult, String> {
    let label = format!("Kyber {} + {}", encoder.label(), codec.label());
    let mut result = LabResult {
        label,
        encoder: Some(encoder),
        codec: Some(codec),
        played: false,
        pipeline: None,
        screen: None,
        at: now(),
        screen_size: display::current().map(|m| [m.width, m.height]),
    };
    let port = free_port().ok_or("No free network port on this PC.")?;
    let dir = Config::dir().join("lab");
    let _ = std::fs::remove_dir_all(&dir);
    let client_dir = dir.join("client");
    std::fs::create_dir_all(&client_dir).map_err(|e| e.to_string())?;

    let base = shared.hosting.lock().unwrap().clone();
    let hosting = Hosting {
        username: "lab".into(),
        port,
        encoder,
        boost: base.boost,
        engine: base.engine,
        ..Hosting::default()
    };
    // A throwaway sign-in: this server lives for a few seconds, on loopback.
    let password = crate::kyber::generate_password();
    let files = host::prepare_in(&dir.join("host"), install, &hosting, &password)?;
    let server = Server::start(install, &files, port, hosting.boost, || {})?;
    let deadline = Instant::now() + Duration::from_secs(12);
    while server.state() == ServerState::Starting && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if server.state() != ServerState::Ready {
        server.stop_and_wait();
        return Ok(result);
    }

    let lab_host = Host {
        id: 1,
        address: "127.0.0.1".into(),
        port,
        auth: AuthMode::Basic,
        username: hosting.username.clone(),
        tls: TlsMode::Skip,
        ..Host::default()
    };
    let lab_settings = StreamSettings {
        codec,
        // Sound or input looped back into the same PC would feed on itself.
        audio: false,
        inputs: false,
        metrics: true,
        auto_reconnect: false,
        display_index: 0,
        display_count: 0,
        ..settings.clone()
    };
    let mut log = LogTail::from_end(client_dir.join("kyclient.log"));
    let mut metrics = LogTail::from_end(client_dir.join("metrics.json"));
    let mut command = Command::new(client);
    command
        .args(build_args(&lab_host, &lab_settings, Some(&password)))
        .current_dir(&client_dir)
        .env("KYBER_LOG_DIR", &client_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("Could not start the client: {e}"))?;
    let job = JobGuard::attach(&child);
    let pid = child.id();
    let viewer = KillOnDrop(child, job);

    // Wait for the first displayed frame.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !shared.cancelled() {
        if log
            .poll()
            .iter()
            .any(|line| line.contains("Player started"))
        {
            result.played = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut collector = Collector::new();
    if result.played {
        if with_beacon {
            crate::winutil::place_lab_window(pid);
            shared.beacon.store(true, Ordering::SeqCst);
            wake();
        }
        std::thread::sleep(WARM_UP);
        // Drop the warm-up frames, then measure.
        for line in metrics.poll() {
            collector.push_line(&line);
        }
        collector = Collector::new();
        let samples = if with_beacon {
            sample_screen(Duration::from_secs(seconds), shared)
        } else {
            std::thread::sleep(Duration::from_secs(seconds));
            Vec::new()
        };
        shared.beacon.store(false, Ordering::SeqCst);
        for line in metrics.poll() {
            collector.push_line(&line);
        }
        let summary = collector.summary_all();
        // Frames that stop arriving mean "plays briefly, then nothing".
        result.played = summary.frames > 0;
        result.pipeline = (summary.frames > 0).then(|| summary.into());
        result.screen = ScreenStats::from_samples(samples);
    }
    drop(viewer);
    server.stop_and_wait();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_pairs_put_hardware_and_hevc_first() {
        let gpus = vec!["AMD Radeon RX 5700 XT".to_string()];
        assert_eq!(
            candidates(&gpus),
            [
                (Encoder::Amf, Codec::Hevc),
                (Encoder::Amf, Codec::H264),
                (Encoder::X264, Codec::H264)
            ]
        );
        assert_eq!(candidates(&[]), [(Encoder::X264, Codec::H264)]);
    }

    #[test]
    fn screen_statistics_and_headlines() {
        let stats = ScreenStats::from_samples(vec![12.0, 8.0, 10.0, 9.0, 30.0]).unwrap();
        assert_eq!(stats.samples, 5);
        assert_eq!(stats.median, 10.0);
        assert_eq!(ScreenStats::from_samples(vec![]), None);

        let mut result = LabResult {
            label: "Kyber".into(),
            encoder: None,
            codec: None,
            played: true,
            pipeline: None,
            screen: Some(stats),
            at: 0,
            screen_size: None,
        };
        assert_eq!(result.headline(), "10 ms on screen (p90 30)");
        result.played = false;
        assert_eq!(result.headline(), "no picture");
    }
}
