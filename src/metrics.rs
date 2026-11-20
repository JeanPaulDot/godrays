//! Reads the per-frame timings Kyber records with `--metrics true`.
//!
//! Kyber stamps every video frame at each stage: captured on the host
//! ("acquired"), encoding, encoded, sent; then on the client received,
//! decoding, decoded, prepared and displayed. Host stamps are on the host's
//! clock; Kyber also logs ping results with the clock offset, so both sides
//! can be put on one timeline. The client writes everything as JSON lines
//! to `metrics.json` in its working directory.

use std::collections::{BTreeMap, VecDeque};

use serde::Deserialize;

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    event: Option<String>,
    source_id: Option<u32>,
    pts: Option<u64>,
    ts: Option<i64>,
    offset_micros: Option<i64>,
    delay_micros: Option<i64>,
    rtt_micros: Option<i64>,
    packets_lost: Option<i64>,
}

#[derive(Default, Clone, Copy)]
struct Stamps {
    acquired: Option<i64>,
    encoded: Option<i64>,
    sent: Option<i64>,
    received: Option<i64>,
    decoded: Option<i64>,
    displayed: Option<i64>,
}

/// Where the time went for one frame, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTimes {
    /// Captured to encoded, on the host.
    pub encode: f32,
    /// Encoded to received: packaging, sending and the network.
    pub network: f32,
    /// Received to decoded.
    pub decode: f32,
    /// Decoded to displayed: the client's renderer.
    pub render: f32,
    /// Captured to displayed.
    pub total: f32,
    /// Client time it was displayed, in microseconds.
    displayed_at: i64,
}

/// Medians over a window of frames, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub frames: usize,
    pub encode: f32,
    pub network: f32,
    pub decode: f32,
    pub render: f32,
    pub total: f32,
    pub total_p10: f32,
    pub total_p90: f32,
    /// Frames displayed per second.
    pub fps: f32,
    pub skipped: u64,
    pub rtt_ms: Option<f32>,
    pub packets_lost: Option<u64>,
}

impl Summary {
    /// One compact line for the overlay.
    pub fn overlay_text(&self) -> String {
        if self.frames == 0 {
            return "waiting for frames".to_owned();
        }
        format!(
            "{:.1} ms \u{b7} enc {:.1} \u{b7} net {:.1} \u{b7} dec {:.1} \u{b7} out {:.1} \u{b7} {:.0} fps",
            self.total, self.encode, self.network, self.decode, self.render, self.fps
        )
    }
}

const KEEP: usize = 4096;

#[derive(Default)]
pub struct Collector {
    /// server clock minus client clock, from the most precise ping so far.
    offset: i64,
    best_delay: Option<i64>,
    pending: BTreeMap<(u32, u64), Stamps>,
    done: VecDeque<FrameTimes>,
    skipped: u64,
    rtt_us: Option<i64>,
    lost: Option<i64>,
}

fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f32 * q) as usize).min(sorted.len() - 1)]
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one line of `metrics.json`. Unknown or malformed lines are
    /// ignored.
    pub fn push_line(&mut self, line: &str) {
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            return;
        };
        match line.kind.as_deref() {
            Some("network_ping") => {
                if let (Some(offset), Some(delay)) = (line.offset_micros, line.delay_micros) {
                    if self.best_delay.is_none_or(|best| delay < best) {
                        self.best_delay = Some(delay);
                        self.offset = offset;
                    }
                }
            }
            Some("network_local") => {
                self.rtt_us = line.rtt_micros.filter(|v| *v >= 0);
                self.lost = line.packets_lost.filter(|v| *v >= 0);
            }
            Some("video") => self.push_video(&line),
            _ => {}
        }
    }

    fn push_video(&mut self, line: &Line) {
        let (Some(source), Some(pts), Some(ts), Some(event)) =
            (line.source_id, line.pts, line.ts, line.event.as_deref())
        else {
            return;
        };
        if event == "skipped" {
            self.skipped += 1;
            self.pending.remove(&(source, pts));
            return;
        }
        let stamps = self.pending.entry((source, pts)).or_default();
        match event {
            "acquired" => stamps.acquired = Some(ts),
            "encoded" => stamps.encoded = Some(ts),
            "sent" => stamps.sent = Some(ts),
            "received" => stamps.received = Some(ts),
            "decoded" => stamps.decoded = Some(ts),
            "displayed" => stamps.displayed = Some(ts),
            _ => {}
        }
        if event == "displayed" {
            let stamps = self.pending.remove(&(source, pts)).unwrap_or_default();
            if let Some(times) = self.finish(stamps) {
                if self.done.len() == KEEP {
                    self.done.pop_front();
                }
                self.done.push_back(times);
            }
        }
        // Frames that never complete (dropped) must not pile up.
        while self.pending.len() > KEEP {
            let first = *self.pending.keys().next().expect("not empty");
            self.pending.remove(&first);
        }
    }

    fn finish(&self, s: Stamps) -> Option<FrameTimes> {
        // Host stamps onto the client clock.
        let host = |t: Option<i64>| t.map(|t| t - self.offset);
        let (acquired, encoded, sent) = (host(s.acquired)?, host(s.encoded)?, host(s.sent));
        let (received, decoded, displayed) = (s.received?, s.decoded?, s.displayed?);
        let ms = |from: i64, to: i64| (to - from) as f32 / 1000.0;
        let _ = sent;
        let times = FrameTimes {
            encode: ms(acquired, encoded),
            network: ms(encoded, received),
            decode: ms(received, decoded),
            render: ms(decoded, displayed),
            total: ms(acquired, displayed),
            displayed_at: displayed,
        };
        // A clock offset not yet known shows up as nonsense: leave it out.
        (times.total >= 0.0 && times.total < 2000.0).then_some(times)
    }

    /// Medians over the last `window_ms` of displayed frames.
    pub fn summary(&self, window_ms: i64) -> Summary {
        let Some(last) = self.done.back() else {
            return Summary {
                skipped: self.skipped,
                rtt_ms: self.rtt_us.map(|v| v as f32 / 1000.0),
                packets_lost: self.lost.map(|v| v as u64),
                ..Summary::default()
            };
        };
        let since = last.displayed_at - window_ms * 1000;
        let recent: Vec<&FrameTimes> = self
            .done
            .iter()
            .filter(|f| f.displayed_at >= since)
            .collect();
        let column =
            |pick: fn(&FrameTimes) -> f32| -> Vec<f32> { recent.iter().map(|f| pick(f)).collect() };
        let mut totals = column(|f| f.total);
        let total = median(&mut totals);
        let span_s = (last.displayed_at - recent[0].displayed_at) as f32 / 1_000_000.0;
        Summary {
            frames: recent.len(),
            encode: median(&mut column(|f| f.encode)),
            network: median(&mut column(|f| f.network)),
            decode: median(&mut column(|f| f.decode)),
            render: median(&mut column(|f| f.render)),
            total,
            total_p10: percentile(&totals, 0.1),
            total_p90: percentile(&totals, 0.9),
            fps: if span_s > 0.0 {
                (recent.len() - 1) as f32 / span_s
            } else {
                0.0
            },
            skipped: self.skipped,
            rtt_ms: self.rtt_us.map(|v| v as f32 / 1000.0),
            packets_lost: self.lost.map(|v| v as u64),
        }
    }

    /// Summary over everything collected.
    pub fn summary_all(&self) -> Summary {
        self.summary(i64::MAX / 2_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(event: &str, pts: u64, ts: i64) -> String {
        format!(
            r#"{{"log_type":"raw_metric","type":"video","event":"{event}","source_id":7,"pts":{pts},"ts":{ts}}}"#
        )
    }

    #[test]
    fn stage_times_come_out_on_one_clock() {
        let mut c = Collector::new();
        // A poor ping first, then a precise one: the precise one wins.
        c.push_line(r#"{"log_type":"raw_metric","type":"network_ping","offset_micros":5000,"delay_micros":900000}"#);
        c.push_line(r#"{"log_type":"raw_metric","type":"network_ping","offset_micros":1000,"delay_micros":120}"#);
        // Host clock runs 1 ms ahead of the client: stamps 1000 us higher.
        for frame in 0..10u64 {
            let base = 1_000_000 + frame as i64 * 6_000;
            c.push_line(&video("acquired", frame, base + 1000));
            c.push_line(&video("encoding", frame, base + 1100));
            c.push_line(&video("encoded", frame, base + 1000 + 4000));
            c.push_line(&video("sent", frame, base + 1000 + 4100));
            c.push_line(&video("received", frame, base + 4500));
            c.push_line(&video("decoded", frame, base + 4800));
            c.push_line(&video("displayed", frame, base + 7500));
        }
        let s = c.summary_all();
        assert_eq!(s.frames, 10);
        assert!((s.encode - 4.0).abs() < 0.01, "{s:?}");
        assert!((s.network - 0.5).abs() < 0.01, "{s:?}");
        assert!((s.decode - 0.3).abs() < 0.01, "{s:?}");
        assert!((s.render - 2.7).abs() < 0.01, "{s:?}");
        assert!((s.total - 7.5).abs() < 0.01, "{s:?}");
        assert!((s.fps - 166.7).abs() < 1.0, "{s:?}");
        assert!(s.overlay_text().starts_with("7.5 ms"));
    }

    #[test]
    fn skipped_frames_and_noise_are_handled() {
        let mut c = Collector::new();
        c.push_line("not json");
        c.push_line(r#"{"log_type":"raw_metric","type":"network_local","rtt_micros":850,"packets_lost":2,"dropped_packets":-1}"#);
        c.push_line(&video("acquired", 1, 100));
        c.push_line(&video("skipped", 1, 200));
        let s = c.summary_all();
        assert_eq!(s.frames, 0);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.overlay_text(), "waiting for frames");
        assert_eq!(c.summary_all().rtt_ms, Some(0.85));
        assert_eq!(c.summary_all().packets_lost, Some(2));
    }
}

/// Checks the reader against a real `metrics.json`, when one is given:
///   GODRAYS_METRICS_SAMPLE=path/to/metrics.json cargo test real_metrics
#[cfg(test)]
mod real {
    #[test]
    fn real_metrics_file_gives_plausible_numbers() {
        let Some(path) = std::env::var_os("GODRAYS_METRICS_SAMPLE") else {
            return;
        };
        let mut collector = super::Collector::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            collector.push_line(line);
        }
        let s = collector.summary_all();
        println!("{s:?}\n{}", s.overlay_text());
        assert!(s.frames > 100);
        assert!(s.total > 0.0 && s.total < 200.0);
    }
}
