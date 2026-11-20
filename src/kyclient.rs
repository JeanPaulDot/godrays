//! Everything that touches the Kyber client binary: finding it, turning the
//! launcher's settings into its command line, and running a stream session.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{
    AudioTransport, AuthMode, Codec, Host, LogLevel, Protocol, StreamSettings, TlsMode, Tri,
    VideoTransport, DEFAULT_PORT,
};
use crate::logtail::LogTail;
use crate::metrics::{Collector, Summary};

const LOG_LINES: usize = 400;

/// File names the client has shipped under.
const BINARY_NAMES: &[&str] = if cfg!(windows) {
    &["kyclient.exe", "client.exe"]
} else {
    &["kyclient", "run_kyclient.sh"]
};

/// Finds the client: explicit setting first, then the usual install spots.
pub fn locate(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
        // A folder is accepted too: look for the binary inside it.
        if path.is_dir() {
            return BINARY_NAMES
                .iter()
                .map(|n| path.join(n))
                .find(|p| p.is_file());
        }
        return None;
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("KYCLIENT_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        dirs.push(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("kyber"));
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            dirs.push(PathBuf::from(&base).join("Kyber"));
            dirs.push(PathBuf::from(&base).join("Programs").join("Kyber"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }

    dirs.iter()
        .flat_map(|dir| BINARY_NAMES.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Asks the client for its version. Returns `None` if it prints nothing
/// (packaged Windows builds have no console) or does not answer in time.
pub fn query_version(path: &Path) -> Option<String> {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let version = out.lines().next()?.trim();
    // clap prints "<app name> <version>"; keep the last word.
    let version = version.rsplit(' ').next().unwrap_or(version);
    (!version.is_empty()).then(|| version.to_owned())
}

/// Removes a host from kyclient's trust-on-first-use store, so the next
/// connection accepts its (new) certificate.
pub fn forget_known_host(path: &Path, address: &str) -> Result<(), String> {
    if address.is_empty() {
        return Err("This host has no address.".into());
    }
    let mut command = Command::new(path);
    command
        .args(["--tls-tofu-remove", address])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(format!(
                    "The client had no trusted certificate for {address}."
                ))
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The client did not answer.".into());
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_console(_command: &mut Command) {}

/// Translates a host and the stream settings into kyclient arguments.
///
/// Only options that differ from kyclient's own defaults are emitted, so the
/// command line stays short and older client builds are not handed flags they
/// do not know.
pub fn build_args(host: &Host, s: &StreamSettings, secret: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut flag = |name: &str, value: Option<String>| {
        args.push(format!("--{name}"));
        args.extend(value);
    };

    if s.protocol != Protocol::Kymux {
        flag("protocol", Some(s.protocol.flag().into()));
    }
    if s.protocol != Protocol::Rtp {
        if s.video_transport != VideoTransport::Reliable {
            flag("kymux-video", Some(s.video_transport.flag().into()));
        }
        if s.audio_transport != AudioTransport::Reliable {
            flag("kymux-audio", Some(s.audio_transport.flag().into()));
        }
    }
    if s.codec != Codec::H264 {
        flag("video-codec", Some(s.codec.flag().into()));
    }
    if s.yuv444 {
        flag("444", None);
    }
    if s.bitrate_mbps > 0 {
        flag("bitrate", Some(format!("{}M", s.bitrate_mbps)));
    }
    if !s.audio {
        flag("audio", Some("false".into()));
    } else if s.audio_buffer_ms > 0 {
        flag("audio-buffer", Some(s.audio_buffer_ms.to_string()));
    }
    if s.video_buffer_ms > 0 {
        flag("video-buffer", Some(s.video_buffer_ms.to_string()));
    }
    if !s.inputs {
        flag("inputs", Some("false".into()));
    }
    if s.software_decode {
        flag("software-decode", None);
    }
    for (name, value) in [
        ("intra-refresh", s.intra_refresh),
        ("gamepad-force-xbox360", s.gamepad_xbox360),
        ("clipboard", s.clipboard),
    ] {
        if value != Tri::Auto {
            flag(name, Some(value.flag().into()));
        }
    }
    if !s.keyboard_grab {
        flag("keyboard-grab", Some("false".into()));
    }
    if s.metrics {
        flag("metrics", Some("true".into()));
    }
    if !s.auto_reconnect {
        flag("auto-reconnect", Some("false".into()));
    }
    // kyclient rejects --display-idx together with --display-count.
    if s.display_count > 1 {
        flag("display-count", Some(s.display_count.to_string()));
    } else if s.display_index > 0 {
        flag("display-idx", Some(s.display_index.to_string()));
    }
    if s.log_level != LogLevel::Default {
        flag("log-level", Some(s.log_level.flag().into()));
    }

    if host.port != DEFAULT_PORT && host.port != 0 {
        flag("port", Some(host.port.to_string()));
    }
    match host.tls {
        TlsMode::Tofu => flag("tls-tofu", None),
        TlsMode::System => {}
        TlsMode::KnownHosts => flag("tls-known-hosts", None),
        TlsMode::Fingerprint => flag("tls-fingerprint", Some(host.fingerprint.trim().into())),
        TlsMode::Skip => flag("tls-skip-verification", None),
    }
    match host.auth {
        // No username: kyclient falls back to its built-in default user.
        AuthMode::Basic if !host.username.is_empty() => {
            flag("auth-username", Some(host.username.clone()));
            flag("auth-password", Some(secret.unwrap_or_default().into()));
        }
        AuthMode::Basic => {}
        AuthMode::Token => flag("auth-token", Some(secret.unwrap_or_default().into())),
        AuthMode::Oidc => flag("auth-oidc", None),
    }

    args.push(host.address.trim().to_owned());
    args
}

/// The command line with secrets masked, for display and logs.
pub fn redact(args: &[String]) -> String {
    let mut out = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for arg in args {
        if mask_next {
            out.push("****".to_owned());
            mask_next = false;
            continue;
        }
        mask_next = matches!(arg.as_str(), "--auth-password" | "--auth-token");
        if arg.contains(' ') {
            out.push(format!("\"{arg}\""));
        } else {
            out.push(arg.clone());
        }
    }
    out.join(" ")
}

/// Why a host cannot be launched yet, if anything is missing.
pub fn validate(host: &Host, secret: Option<&str>) -> Result<(), String> {
    if host.address.trim().is_empty() {
        return Err("This host has no address.".into());
    }
    if host.tls == TlsMode::Fingerprint && host.fingerprint.trim().is_empty() {
        return Err("TLS is set to a pinned fingerprint, but none is entered.".into());
    }
    let has_secret = !secret.unwrap_or_default().is_empty();
    if host.auth == AuthMode::Token && !has_secret {
        return Err("This host uses token sign-in, but no token is saved.".into());
    }
    if host.auth == AuthMode::Basic && !host.username.is_empty() && !has_secret {
        return Err(format!(
            "No password is saved for \"{}\". Edit the host to enter it.",
            host.username
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Running,
    /// Exit code, if the process reported one.
    Exited(Option<i32>),
}

/// Output lines of a child process, shared with the threads reading them.
pub(crate) type LogBuffer = Arc<Mutex<VecDeque<String>>>;

/// The last `lines` lines of a log buffer.
pub(crate) fn log_tail(buffer: &LogBuffer, lines: usize) -> Vec<String> {
    let log = buffer.lock().unwrap();
    log.iter()
        .skip(log.len().saturating_sub(lines))
        .cloned()
        .collect()
}

struct Shared {
    state: Mutex<SessionState>,
    log: LogBuffer,
    stop: AtomicBool,
    stopped_by_user: AtomicBool,
    phase: Mutex<Phase>,
    /// Latency statistics over the last two seconds, when metrics are on.
    stats: Mutex<Summary>,
    end_reason: Mutex<Option<EndReason>>,
}

/// A running (or just finished) kyclient process.
pub struct Session {
    shared: Arc<Shared>,
    /// The client's process id, to bring its window back to the front.
    pub pid: u32,
    pub command_line: String,
    pub started: Instant,
}

pub struct LaunchOptions {
    pub start_fullscreen: bool,
    /// Folder the client writes its log to; the launcher follows it.
    pub log_dir: PathBuf,
    /// Where the host is, to tell "host stopped" from a network hiccup.
    pub host_address: String,
    pub host_port: u16,
}

/// How far a session has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Streaming,
    Reconnecting,
}

/// Why a session ended, when the client's log tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    HostStopped,
    WrongSignIn,
}

impl Session {
    /// Starts kyclient. `wake` is called (from a background thread) whenever
    /// the session produces output or ends, so the UI can repaint.
    pub fn start(
        binary: &Path,
        host: &Host,
        settings: &StreamSettings,
        secret: Option<&str>,
        options: LaunchOptions,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::start_with_env(binary, host, settings, secret, options, &[], wake)
    }

    /// [`Session::start`] with extra environment variables for the client.
    fn start_with_env(
        binary: &Path,
        host: &Host,
        settings: &StreamSettings,
        secret: Option<&str>,
        options: LaunchOptions,
        extra_env: &[(&str, &str)],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let args = build_args(host, settings, secret);
        let command_line = format!(
            "{} {}",
            binary.file_name().unwrap_or_default().to_string_lossy(),
            redact(&args)
        );

        let mut command = Command::new(binary);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The packaged client prints nothing; its log file is how we follow it.
        let _ = std::fs::create_dir_all(&options.log_dir);
        command.env("KYBER_LOG_DIR", &options.log_dir);
        command.envs(extra_env.iter().copied());
        let tail = LogTail::from_end(options.log_dir.join("kyclient.log"));
        // With --metrics the client writes metrics.json to its working
        // directory, so that is our (writable) log folder. It finds its own
        // plugins next to its executable either way.
        command.current_dir(&options.log_dir);
        let metrics_path = options.log_dir.join("metrics.json");
        let _ = std::fs::remove_file(&metrics_path);
        let metrics = settings
            .metrics
            .then(|| LogTail::from_end(metrics_path.clone()));
        hide_console(&mut command);

        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", binary.display()))?;

        let pid = child.id();
        let shared = Arc::new(Shared {
            state: Mutex::new(SessionState::Running),
            log: LogBuffer::default(),
            stop: AtomicBool::new(false),
            stopped_by_user: AtomicBool::new(false),
            phase: Mutex::new(Phase::Connecting),
            stats: Mutex::new(Summary::default()),
            end_reason: Mutex::new(None),
        });
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);

        if let Some(out) = child.stdout.take() {
            spawn_log_reader(out, shared.log.clone(), wake.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_log_reader(err, shared.log.clone(), wake.clone());
        }

        let watcher_shared = shared.clone();
        std::thread::spawn(move || watch(child, watcher_shared, options, tail, metrics, wake));

        Ok(Self {
            shared,
            pid,
            command_line,
            started: Instant::now(),
        })
    }

    pub fn state(&self) -> SessionState {
        self.shared.state.lock().unwrap().clone()
    }

    /// Live latency statistics (empty when metrics are off).
    pub fn stats(&self) -> Summary {
        *self.shared.stats.lock().unwrap()
    }

    pub fn phase(&self) -> Phase {
        *self.shared.phase.lock().unwrap()
    }

    pub fn end_reason(&self) -> Option<EndReason> {
        *self.shared.end_reason.lock().unwrap()
    }

    /// Asks the watcher thread to end the stream.
    pub fn stop(&self) {
        self.shared.stopped_by_user.store(true, Ordering::SeqCst);
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// True when the stream was ended from the launcher or the pad combo,
    /// as opposed to kyclient exiting on its own.
    pub fn stopped_by_user(&self) -> bool {
        self.shared.stopped_by_user.load(Ordering::SeqCst)
    }

    pub fn log_tail(&self, lines: usize) -> Vec<String> {
        let log = self.shared.log.lock().unwrap();
        log.iter()
            .skip(log.len().saturating_sub(lines))
            .cloned()
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the launcher ends the stream it started.
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

pub(crate) fn spawn_log_reader(
    stream: impl Read + Send + 'static,
    buffer: LogBuffer,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = strip_ansi(String::from_utf8_lossy(&raw).trim_end());
            if line.is_empty() {
                continue;
            }
            let mut log = buffer.lock().unwrap();
            if log.len() == LOG_LINES {
                log.pop_front();
            }
            log.push_back(line);
            drop(log);
            wake();
        }
    });
}

/// Removes terminal colour sequences from a log line.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI sequence: ESC [ ... final byte in @..~
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The fullscreen shortcut is sent once video is playing; if the log never
/// says so, it is sent anyway after this long.
const FULLSCREEN_FALLBACK: Duration = Duration::from_secs(8);

/// Updates the session from one line of the client's log.
fn interpret(line: &str, shared: &Shared, options: &LaunchOptions) {
    if line.contains("Player started") || line.contains("Successfully reconnected") {
        *shared.phase.lock().unwrap() = Phase::Streaming;
    } else if line.contains("Reconnecting...") || line.contains("Data plane closed") {
        *shared.phase.lock().unwrap() = Phase::Reconnecting;
        // The client keeps retrying on its own. If the host no longer
        // answers at all, it stopped sharing: end the session now instead
        // of leaving a frozen picture on screen.
        if crate::net::probe(&options.host_address, options.host_port)
            == crate::net::Reachability::Offline
        {
            *shared.end_reason.lock().unwrap() = Some(EndReason::HostStopped);
            shared.stop.store(true, Ordering::SeqCst);
        }
    } else if line.contains("401 Unauthorized") || line.contains("connect failed: Unauthorized") {
        *shared.end_reason.lock().unwrap() = Some(EndReason::WrongSignIn);
    }
}

fn follow_log(tail: &mut LogTail, shared: &Shared, options: &LaunchOptions) -> bool {
    let lines = tail.poll();
    if lines.is_empty() {
        return false;
    }
    for line in &lines {
        interpret(line, shared, options);
    }
    let mut log = shared.log.lock().unwrap();
    for line in lines {
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }
    true
}

fn watch(
    mut child: Child,
    shared: Arc<Shared>,
    options: LaunchOptions,
    mut tail: LogTail,
    mut metrics: Option<LogTail>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let started = Instant::now();
    let mut collector = Collector::new();
    let mut next_stats = Instant::now();
    let mut fullscreen = options
        .start_fullscreen
        .then(|| crate::winutil::FullscreenRequest::new(child.id()));
    let mut combo = crate::winutil::QuitCombo::default();
    crate::hotkey::arm();
    let mut next_log_check = Instant::now();

    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(_) => break None,
        }
        if combo.held_long_enough() || crate::hotkey::quit_pressed() {
            shared.stopped_by_user.store(true, Ordering::SeqCst);
            shared.stop.store(true, Ordering::SeqCst);
        }
        if Instant::now() >= next_log_check {
            next_log_check = Instant::now() + Duration::from_millis(200);
            if follow_log(&mut tail, &shared, &options) {
                wake();
            }
        }
        if let Some(metrics) = &mut metrics {
            if Instant::now() >= next_stats {
                next_stats = Instant::now() + Duration::from_millis(500);
                for line in metrics.poll() {
                    collector.push_line(&line);
                }
                *shared.stats.lock().unwrap() = collector.summary(2000);
            }
        }
        if shared.stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            break child.wait().ok().and_then(|s| s.code());
        }
        if let Some(request) = &mut fullscreen {
            let video_up = *shared.phase.lock().unwrap() == Phase::Streaming;
            if (video_up || started.elapsed() > FULLSCREEN_FALLBACK) && request.poll() {
                fullscreen = None;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    // Whatever the client wrote on its way out.
    follow_log(&mut tail, &shared, &options);
    // About 270 MB an hour: not kept once the stream is over.
    if metrics.is_some() {
        let _ = std::fs::remove_file(options.log_dir.join("metrics.json"));
    }
    *shared.state.lock().unwrap() = SessionState::Exited(code);
    wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host {
            id: 1,
            name: "PC".into(),
            address: "192.168.1.20".into(),
            ..Host::default()
        }
    }

    #[test]
    fn defaults_produce_a_minimal_command_line() {
        let args = build_args(&host(), &StreamSettings::default(), None);
        assert_eq!(args, ["--tls-tofu", "192.168.1.20"]);
    }

    #[test]
    fn every_non_default_setting_becomes_a_flag() {
        let settings = StreamSettings {
            codec: Codec::Hevc,
            bitrate_mbps: 30,
            protocol: Protocol::Kymux,
            video_transport: VideoTransport::UnreliableFec,
            audio_transport: AudioTransport::Unreliable,
            yuv444: true,
            audio: true,
            audio_buffer_ms: 20,
            video_buffer_ms: 5,
            software_decode: true,
            intra_refresh: Tri::Off,
            inputs: false,
            gamepad_xbox360: Tri::On,
            clipboard: Tri::Off,
            keyboard_grab: false,
            metrics: true,
            auto_reconnect: false,
            display_index: 2,
            display_count: 0,
            log_level: LogLevel::Debug,
            use_host_codec: false,
        };
        let mut h = host();
        h.port = 9000;
        h.tls = TlsMode::KnownHosts;
        h.username = "alice".into();
        let args = build_args(&h, &settings, Some("s3cret pw"));
        let line = args.join(" ");
        for expected in [
            "--kymux-video unreliable_fec",
            "--kymux-audio unreliable",
            "--video-codec hevc",
            "--444",
            "--bitrate 30M",
            "--audio-buffer 20",
            "--video-buffer 5",
            "--inputs false",
            "--software-decode",
            "--intra-refresh false",
            "--gamepad-force-xbox360 true",
            "--clipboard false",
            "--keyboard-grab false",
            "--metrics true",
            "--auto-reconnect false",
            "--display-idx 2",
            "--log-level debug",
            "--port 9000",
            "--tls-known-hosts",
            "--auth-username alice --auth-password s3cret pw",
        ] {
            assert!(line.contains(expected), "missing `{expected}` in `{line}`");
        }
        assert_eq!(args.last().unwrap(), "192.168.1.20");
        assert!(!line.contains("--protocol"));
    }

    #[test]
    fn display_count_wins_over_display_index() {
        let settings = StreamSettings {
            display_index: 1,
            display_count: 2,
            ..StreamSettings::default()
        };
        let line = build_args(&host(), &settings, None).join(" ");
        assert!(line.contains("--display-count 2"));
        assert!(!line.contains("--display-idx"));
    }

    #[test]
    fn muted_audio_drops_the_buffer_flag_and_rtp_drops_kymux_flags() {
        let settings = StreamSettings {
            audio: false,
            audio_buffer_ms: 40,
            protocol: Protocol::Rtp,
            video_transport: VideoTransport::Gopstream,
            ..StreamSettings::default()
        };
        let line = build_args(&host(), &settings, None).join(" ");
        assert!(line.contains("--audio false"));
        assert!(!line.contains("--audio-buffer"));
        assert!(line.contains("--protocol rtp"));
        assert!(!line.contains("--kymux-video"));
    }

    #[test]
    fn auth_modes_map_to_the_right_flags() {
        let mut h = host();
        h.auth = AuthMode::Token;
        let line = build_args(&h, &StreamSettings::default(), Some("abc.def")).join(" ");
        assert!(line.contains("--auth-token abc.def"));

        h.auth = AuthMode::Oidc;
        let line = build_args(&h, &StreamSettings::default(), None).join(" ");
        assert!(line.contains("--auth-oidc"));
        assert!(!line.contains("--auth-token"));

        // Basic without a username relies on kyclient's default user.
        h.auth = AuthMode::Basic;
        let line = build_args(&h, &StreamSettings::default(), Some("ignored")).join(" ");
        assert!(!line.contains("--auth-"));
    }

    #[test]
    fn secrets_are_masked_for_display() {
        let mut h = host();
        h.username = "Default user".into();
        let args = build_args(&h, &StreamSettings::default(), Some("hunter2"));
        let shown = redact(&args);
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("--auth-username \"Default user\" --auth-password ****"));
    }

    #[test]
    fn incomplete_hosts_are_rejected_before_launch() {
        let mut h = host();
        assert!(validate(&h, None).is_ok());
        h.tls = TlsMode::Fingerprint;
        assert!(validate(&h, None).is_err());
        h.fingerprint = "sha256:AB".into();
        assert!(validate(&h, None).is_ok());
        h.username = "alice".into();
        assert!(validate(&h, None).is_err());
        assert!(validate(&h, Some("pw")).is_ok());
        h.auth = AuthMode::Token;
        assert!(validate(&h, None).is_err());
        h.address.clear();
        assert!(validate(&h, Some("t")).is_err());
    }

    #[test]
    fn ansi_colours_are_stripped_from_log_lines() {
        assert_eq!(strip_ansi("\u{1b}[31mERROR\u{1b}[0m boom"), "ERROR boom");
    }
}

/// End-to-end checks against the stand-in client in `examples/`. They run
/// only when FAKE_KYCLIENT points at the built example:
///   cargo build --example fake_kyclient
///   FAKE_KYCLIENT=target/debug/examples/fake_kyclient.exe cargo test
#[cfg(test)]
mod process_tests {
    use super::*;

    fn test_options() -> LaunchOptions {
        let log_dir = std::env::temp_dir().join(format!(
            "godrays-session-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&log_dir);
        LaunchOptions {
            start_fullscreen: false,
            log_dir,
            host_address: "127.0.0.1".into(),
            // Nothing listens here: a "reconnecting" client means the host is gone.
            host_port: 9,
        }
    }

    fn fake() -> Option<PathBuf> {
        std::env::var_os("FAKE_KYCLIENT").map(PathBuf::from)
    }

    fn wait_for_exit(session: &Session) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let SessionState::Exited(code) = session.state() {
                return code;
            }
            assert!(Instant::now() < deadline, "session did not end");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn host() -> Host {
        Host {
            id: 7,
            address: "127.0.0.1".into(),
            ..Host::default()
        }
    }

    fn start(fake: &Path) -> Session {
        let options = test_options();
        Session::start(
            fake,
            &host(),
            &StreamSettings::default(),
            None,
            options,
            || {},
        )
        .unwrap()
    }

    #[test]
    fn version_is_read_from_the_client() {
        let Some(fake) = fake() else { return };
        assert_eq!(query_version(&fake).as_deref(), Some("0.0.0-fake"));
    }

    #[test]
    fn a_session_runs_captures_output_and_can_be_stopped() {
        let Some(fake) = fake() else { return };
        // The fake client "streams" for a second by default; stop it early.
        let session = start(&fake);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(session.state(), SessionState::Running);
        let log = session.log_tail(10).join("\n");
        assert!(log.contains("connecting to 127.0.0.1"), "log was: {log}");
        // Colour codes are stripped.
        assert!(log.contains("WARN  fake"), "log was: {log}");

        let stopped_at = Instant::now();
        session.stop();
        wait_for_exit(&session);
        assert!(stopped_at.elapsed() < Duration::from_millis(500));
        assert!(session.stopped_by_user());
    }

    #[test]
    fn a_session_that_ends_by_itself_reports_its_exit_code() {
        let Some(fake) = fake() else { return };
        let session = start(&fake);
        assert_eq!(wait_for_exit(&session), Some(0));
        assert!(!session.stopped_by_user());
    }

    fn start_with(fake: &Path, env: &[(&str, &str)]) -> Session {
        Session::start_with_env(
            fake,
            &host(),
            &StreamSettings::default(),
            None,
            test_options(),
            env,
            || {},
        )
        .unwrap()
    }

    #[test]
    fn live_stats_come_from_the_metrics_file() {
        let Some(fake) = fake() else { return };
        let settings = StreamSettings {
            metrics: true,
            ..StreamSettings::default()
        };
        let session = Session::start_with_env(
            &fake,
            &host(),
            &settings,
            None,
            test_options(),
            &[
                ("FAKE_KYCLIENT_PLAYS", "1"),
                ("FAKE_KYCLIENT_SLEEP_MS", "3000"),
            ],
            || {},
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.stats().frames == 0 {
            assert!(Instant::now() < deadline, "no stats arrived");
            std::thread::sleep(Duration::from_millis(50));
        }
        let stats = session.stats();
        assert!((stats.total - 8.0).abs() < 0.01, "{stats:?}");
        assert!((stats.encode - 4.0).abs() < 0.01, "{stats:?}");
        session.stop();
        wait_for_exit(&session);
    }

    #[test]
    fn the_client_log_drives_the_session_phase() {
        let Some(fake) = fake() else { return };
        let session = start_with(&fake, &[("FAKE_KYCLIENT_PLAYS", "1")]);
        assert_eq!(session.phase(), Phase::Connecting);
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.phase() != Phase::Streaming {
            assert!(Instant::now() < deadline, "never reached Streaming");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(session.log_tail(5).join("\n").contains("Player started"));
        wait_for_exit(&session);
        assert_eq!(session.end_reason(), None);
    }

    #[test]
    fn a_session_ends_by_itself_when_the_host_is_gone() {
        let Some(fake) = fake() else { return };
        // The stand-in would run for 20 s; it reports "reconnecting" and
        // the host port answers nobody, so the session is ended early.
        let session = start_with(
            &fake,
            &[
                ("FAKE_KYCLIENT_RECONNECTS", "1"),
                ("FAKE_KYCLIENT_SLEEP_MS", "20000"),
            ],
        );
        let started = Instant::now();
        wait_for_exit(&session);
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(session.end_reason(), Some(EndReason::HostStopped));
        assert!(!session.stopped_by_user());
    }

    #[test]
    fn a_refused_sign_in_is_recognised() {
        let Some(fake) = fake() else { return };
        let session = start_with(&fake, &[("FAKE_KYCLIENT_UNAUTHORIZED", "1")]);
        assert_eq!(wait_for_exit(&session), Some(1));
        assert_eq!(session.end_reason(), Some(EndReason::WrongSignIn));
    }
}
