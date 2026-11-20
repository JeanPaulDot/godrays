//! Persistent launcher state: saved hosts and stream settings.
//!
//! Everything here maps onto a `kyclient` command-line flag; see `kyclient.rs`
//! for the translation. Secrets (passwords, tokens) are not stored here but in
//! the OS credential store, see `secrets.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 8080;

/// Declares a C-like enum with the kyclient flag value and a display label for
/// each variant, plus helpers to cycle through the values from the UI.
macro_rules! choice_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => ($flag:expr, $label:expr)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        #[allow(dead_code)]
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Value passed to kyclient.
            #[allow(dead_code)]
            pub fn flag(self) -> &'static str {
                match self { $($name::$variant => $flag),+ }
            }

            pub fn label(self) -> &'static str {
                match self { $($name::$variant => $label),+ }
            }

            pub fn step(self, delta: i32) -> Self {
                let all = Self::ALL;
                let idx = all.iter().position(|v| *v == self).unwrap_or(0) as i32;
                let len = all.len() as i32;
                all[(idx + delta).rem_euclid(len) as usize]
            }
        }
    };
}

choice_enum!(Codec {
    H264 => ("h264", "H.264"),
    Hevc => ("hevc", "HEVC (H.265)"),
    Av1 => ("av1", "AV1 (experimental)"),
});

choice_enum!(Protocol {
    Kymux => ("kymux", "Kymux (QUIC)"),
    Websocket => ("websocket", "WebSocket"),
    Rtp => ("rtp", "RTP"),
});

choice_enum!(VideoTransport {
    Reliable => ("reliable", "Reliable"),
    Gopstream => ("gopstream", "GOP stream"),
    Unreliable => ("unreliable", "Unreliable"),
    UnreliableFec => ("unreliable_fec", "Unreliable + FEC"),
});

choice_enum!(AudioTransport {
    Reliable => ("reliable", "Reliable"),
    Unreliable => ("unreliable", "Unreliable"),
    UnreliableFec => ("unreliable_fec", "Unreliable + FEC"),
});

choice_enum!(
    /// An option kyclient leaves to the server unless told otherwise.
    Tri {
        Auto => ("", "Server default"),
        On => ("true", "On"),
        Off => ("false", "Off"),
    }
);

choice_enum!(AuthMode {
    Basic => ("basic", "Username & password"),
    Token => ("token", "Token (JWT)"),
    Oidc => ("oidc", "OIDC (browser sign-in)"),
});

choice_enum!(TlsMode {
    Tofu => ("tofu", "Trust on first use"),
    System => ("system", "System certificates"),
    KnownHosts => ("known_hosts", "Known hosts only"),
    Fingerprint => ("fingerprint", "Pinned fingerprint"),
    Skip => ("skip", "No verification (unsafe)"),
});

choice_enum!(LogLevel {
    Default => ("", "Default"),
    Warn => ("warn", "Warnings"),
    Info => ("info", "Info"),
    Debug => ("debug", "Debug"),
    Trace => ("trace", "Trace"),
});

choice_enum!(Encoder {
    Auto => ("", "Chosen at install"),
    Nvenc => ("nvenc", "NVIDIA (NVENC)"),
    Amf => ("amf", "AMD (AMF)"),
    Qsv => ("qsv", "Intel (Quick Sync)"),
    X264 => ("x264", "Software (x264)"),
});

choice_enum!(Engine {
    Default => ("", "Kyber's default (txproto)"),
    Txproto => ("txproto", "txproto"),
    Libavconv => ("libavconv", "libavconv"),
});

choice_enum!(Preset {
    Speed => ("speed", "Speed"),
    Balanced => ("balanced", "Balanced"),
    Quality => ("quality", "Quality"),
    Wifi => ("wifi", "Lossy network"),
});

/// Bitrate steps offered in the UI, in Mbit/s. 0 means "let the server decide".
pub const BITRATES: &[u32] = &[0, 5, 10, 15, 20, 30, 40, 50, 60, 80, 100, 150];
/// Audio buffer steps in ms. 0 means "kyclient default".
pub const AUDIO_BUFFERS: &[u32] = &[0, 10, 20, 40, 60, 100, 150, 200];
/// Video buffer steps in ms. 0 is kyclient's default: display frames as they arrive.
pub const VIDEO_BUFFERS: &[u32] = &[0, 5, 10, 20, 30, 50, 100];

/// Moves `value` to the neighbouring entry of `steps`, clamping at both ends.
pub fn step_in(steps: &[u32], value: u32, delta: i32) -> u32 {
    let idx = steps
        .iter()
        .position(|v| *v >= value)
        .unwrap_or(steps.len() - 1) as i32;
    let next = (idx + delta).clamp(0, steps.len() as i32 - 1);
    steps[next as usize]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    pub codec: Codec,
    /// Mbit/s, 0 = server default.
    pub bitrate_mbps: u32,
    pub protocol: Protocol,
    pub video_transport: VideoTransport,
    pub audio_transport: AudioTransport,
    pub yuv444: bool,
    pub audio: bool,
    pub audio_buffer_ms: u32,
    pub video_buffer_ms: u32,
    pub software_decode: bool,
    pub intra_refresh: Tri,
    pub inputs: bool,
    pub gamepad_xbox360: Tri,
    pub clipboard: Tri,
    pub keyboard_grab: bool,
    pub metrics: bool,
    pub auto_reconnect: bool,
    /// 0 = first display (kyclient default).
    pub display_index: u32,
    /// 0 or 1 = single display; above that, stream this many displays.
    pub display_count: u32,
    pub log_level: LogLevel,
    /// Stream with the codec the host measured fastest, when it says.
    pub use_host_codec: bool,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            bitrate_mbps: 0,
            protocol: Protocol::Kymux,
            video_transport: VideoTransport::Reliable,
            audio_transport: AudioTransport::Reliable,
            yuv444: false,
            audio: true,
            audio_buffer_ms: 0,
            video_buffer_ms: 0,
            software_decode: false,
            intra_refresh: Tri::Auto,
            inputs: true,
            gamepad_xbox360: Tri::Auto,
            clipboard: Tri::Auto,
            keyboard_grab: true,
            metrics: false,
            auto_reconnect: true,
            display_index: 0,
            display_count: 0,
            log_level: LogLevel::Default,
            use_host_codec: true,
        }
    }
}

impl StreamSettings {
    /// Applies a preset. Presets only touch the options that trade latency
    /// against quality or loss resilience; everything else is left alone.
    pub fn apply_preset(&mut self, preset: Preset) {
        match preset {
            Preset::Speed => {
                self.codec = Codec::H264;
                self.bitrate_mbps = 0;
                self.yuv444 = false;
                self.video_transport = VideoTransport::Reliable;
                self.audio_transport = AudioTransport::Reliable;
                self.intra_refresh = Tri::Auto;
            }
            Preset::Balanced => {
                self.codec = Codec::Hevc;
                self.bitrate_mbps = 30;
                self.yuv444 = false;
                self.video_transport = VideoTransport::Reliable;
                self.audio_transport = AudioTransport::Reliable;
                self.intra_refresh = Tri::Auto;
            }
            Preset::Quality => {
                self.codec = Codec::Hevc;
                self.bitrate_mbps = 80;
                self.yuv444 = true;
                self.video_transport = VideoTransport::Reliable;
                self.audio_transport = AudioTransport::Reliable;
                self.intra_refresh = Tri::Auto;
            }
            Preset::Wifi => {
                self.codec = Codec::H264;
                self.bitrate_mbps = 15;
                self.yuv444 = false;
                self.video_transport = VideoTransport::UnreliableFec;
                self.audio_transport = AudioTransport::UnreliableFec;
                self.intra_refresh = Tri::Auto;
            }
        }
        self.video_buffer_ms = 0;
        self.audio_buffer_ms = 0;
        self.software_decode = false;
    }

    /// The preset these settings currently match, if any.
    pub fn matching_preset(&self) -> Option<Preset> {
        Preset::ALL.iter().copied().find(|preset| {
            let mut probe = self.clone();
            probe.apply_preset(*preset);
            probe == *self
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub id: u64,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub auth: AuthMode,
    /// Empty = let kyclient use its built-in default user.
    pub username: String,
    pub tls: TlsMode,
    /// `sha256:AA:BB:...`, used when `tls` is `Fingerprint`.
    pub fingerprint: String,
    /// Learned from the ARP table while the host is online; used for Wake-on-LAN.
    pub mac: String,
    /// Unix seconds of the last stream launch, 0 = never.
    pub last_connected: u64,
    /// The host's Godrays agent, when paired: its port and our device id.
    pub agent_port: u16,
    pub agent_id: String,
    /// The codec that measured fastest on the host, as it last told us.
    pub codec_hint: Option<Codec>,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            address: String::new(),
            port: DEFAULT_PORT,
            auth: AuthMode::Basic,
            username: String::new(),
            tls: TlsMode::Tofu,
            fingerprint: String::new(),
            mac: String::new(),
            last_connected: 0,
            agent_port: 0,
            agent_id: String::new(),
            codec_hint: None,
        }
    }
}

impl Host {
    pub fn new_id() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        // Never 0: that marks a host that has not been saved yet.
        nanos | 1
    }

    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
}

/// How this PC is offered to others when hosting. The password lives in
/// the OS credential store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hosting {
    pub username: String,
    pub port: u16,
    pub encoder: Encoder,
    /// Resolution the primary display runs at while hosting; 0 keeps
    /// the display as it is.
    pub width: u32,
    pub height: u32,
    /// Codec that measured fastest with `encoder` on this PC, if known.
    pub best_codec: Option<Codec>,
    /// Its median capture-to-display time, in milliseconds.
    pub best_latency_ms: Option<f32>,
    /// Port of the Godrays agent (pairing and status).
    pub agent_port: u16,
    /// Give the server's capture and encode processes priority over
    /// games, for the CPU and the GPU.
    pub boost: bool,
    /// Kyber's capture and encode engine.
    pub engine: Engine,
    /// Highest frame rate to stream at, in frames per second; 0 is the
    /// display's highest. Kyber captures at the display's refresh rate, so
    /// this is applied by running the display at no more than this rate
    /// while hosting.
    pub max_fps: u32,
}

/// Frame-rate caps offered for hosting; 0 is the display's highest rate.
pub const FRAME_RATES: &[u32] = &[60, 90, 120, 144, 0];

impl Hosting {
    /// The encoder to test with: the chosen one, or software when the
    /// installer's choice is unknown.
    pub fn encoder_or_default(&self) -> Encoder {
        match self.encoder {
            Encoder::Auto => Encoder::X264,
            chosen => chosen,
        }
    }
}

impl Default for Hosting {
    fn default() -> Self {
        Self {
            username: "kyber".into(),
            port: DEFAULT_PORT,
            encoder: Encoder::Auto,
            width: 0,
            height: 0,
            best_codec: None,
            best_latency_ms: None,
            agent_port: 8081,
            boost: true,
            engine: Engine::Default,
            max_fps: 60,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Explicit path to the kyclient executable. Empty = auto-detect.
    pub kyclient_path: String,
    /// Ask kyclient to go fullscreen once its window is up.
    pub start_fullscreen: bool,
    /// Run the launcher itself fullscreen.
    pub launcher_fullscreen: bool,
    /// Connect to the most recently used host as soon as the launcher starts.
    pub auto_connect_last: bool,
    /// Do not show the small always-on-top quit button over a stream.
    pub hide_quit_button: bool,
    /// Do not show live latency statistics over a stream.
    pub hide_stats: bool,
    pub stream: StreamSettings,
    pub hosting: Hosting,
    pub hosts: Vec<Host>,
    /// Devices paired with this PC (their secrets are in the credential store).
    pub paired_devices: Vec<PairedDevice>,
}

impl Config {
    pub fn first_run() -> Self {
        Self {
            start_fullscreen: true,
            ..Self::default()
        }
    }

    pub fn dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("GODRAYS_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        directories::ProjectDirs::from("", "", "godrays")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Where the settings lived before the project was renamed.
    fn old_dir() -> Option<PathBuf> {
        if std::env::var_os("GODRAYS_CONFIG_DIR").is_some() {
            return None;
        }
        directories::ProjectDirs::from("", "", "kyber-launcher")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Copies settings saved under the old name, once. The old file is left
    /// in place, so nothing is lost if the copy turns out to be wrong.
    fn migrate_from_old_name() {
        let new = Self::path();
        if new.exists() {
            return;
        }
        if let Some(old) = Self::old_dir().map(|dir| dir.join("config.toml")) {
            if old.is_file() {
                if let Some(parent) = new.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&old, &new);
            }
        }
    }

    /// Loads the config. A missing file is a first run; an unreadable one is
    /// reported so the caller can avoid overwriting it blindly.
    pub fn load() -> Result<Self, String> {
        Self::migrate_from_old_name();
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| format!("{} could not be parsed: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::first_run()),
            Err(e) => Err(format!("{} could not be read: {e}", path.display())),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Write to a sibling file first so a crash mid-write cannot truncate
        // the existing config.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn host(&self, id: u64) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }

    pub fn host_mut(&mut self, id: u64) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.id == id)
    }

    pub fn last_host(&self) -> Option<&Host> {
        self.hosts
            .iter()
            .filter(|h| h.last_connected > 0)
            .max_by_key(|h| h.last_connected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_through_toml() {
        let mut config = Config::first_run();
        config.stream.apply_preset(Preset::Quality);
        config.hosts.push(Host {
            id: 42,
            name: "Gaming PC".into(),
            address: "192.168.1.20".into(),
            ..Host::default()
        });
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.hosts, config.hosts);
        assert_eq!(back.stream, config.stream);
        assert!(back.start_fullscreen);
    }

    #[test]
    fn unknown_and_missing_keys_are_tolerated() {
        let config: Config = toml::from_str("[stream]\ncodec = \"hevc\"\n").unwrap();
        assert_eq!(config.stream.codec, Codec::Hevc);
        assert!(config.stream.audio);
    }

    #[test]
    fn presets_are_recognised() {
        for preset in Preset::ALL {
            let mut s = StreamSettings::default();
            s.apply_preset(*preset);
            assert_eq!(s.matching_preset(), Some(*preset));
        }
        let mut s = StreamSettings::default();
        s.apply_preset(Preset::Speed);
        s.bitrate_mbps = 40;
        assert_eq!(s.matching_preset(), None);
    }

    #[test]
    fn stepping_clamps_and_wraps() {
        assert_eq!(step_in(BITRATES, 0, -1), 0);
        assert_eq!(step_in(BITRATES, 0, 1), 5);
        assert_eq!(step_in(BITRATES, 150, 1), 150);
        // A hand-edited value between two steps snaps to a neighbour.
        assert_eq!(step_in(BITRATES, 12, 1), 20);
        assert_eq!(Codec::H264.step(-1), Codec::Av1);
        assert_eq!(Codec::Av1.step(1), Codec::H264);
    }
}
