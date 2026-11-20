//! The launcher itself: screens, navigation and the stream lifecycle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, vec2, Align2, Color32, Margin, Pos2, Rect, RichText, Sense, Stroke};

use crate::config::{
    step_in, AuthMode, Config, Host, Preset, TlsMode, AUDIO_BUFFERS, BITRATES, DEFAULT_PORT,
    VIDEO_BUFFERS,
};
use crate::kyclient::{self, EndReason, LaunchOptions, Phase, Session, SessionState};
use crate::net::{self, HostStatus, Reachability, ScanEvent};
use crate::pad::{self, Nav, Pad};
use crate::secrets;
use crate::ui::{self, Hint, Row, RowAct, RowKind, Tile, TileIcon};

mod hosting;
mod latency;
mod pairing;

const RELEASES_URL: &str = "https://gitlab.com/kyber/apps/kyber-installer/-/releases";
const WAKE_TIMEOUT: Duration = Duration::from_secs(120);
/// Settings categories: (title, one-line blurb, intro).
const SETTINGS_TABS: [(&str, &str, &str); 6] = [
    (
        "Picture",
        "Codec, bitrate, sound",
        "How the stream looks and sounds when you join another PC. The defaults keep Kyber's lowest-latency setup.",
    ),
    (
        "Network",
        "Transport and recovery",
        "How the stream travels. Leave these alone on a wired network; change them for Wi-Fi or networks that block UDP.",
    ),
    (
        "Controls",
        "Keyboard, mouse, gamepads",
        "What you send to the PC you join.",
    ),
    (
        "Hosting",
        "When others join this PC",
        "Applies when you share this PC from the Host screen. Paired devices need none of the sign-in details here.",
    ),
    (
        "Latency",
        "Measure, compare, overlay",
        "Godrays measures Kyber's real delay, stage by stage, on this PC.",
    ),
    (
        "App",
        "Godrays itself",
        "Window, start-up and where Kyber is installed.",
    ),
];
const SETTINGS_HOSTING: usize = 3;
const SETTINGS_LATENCY: usize = 4;

pub struct StartupArgs {
    /// Name or address of a saved host to connect to right away.
    pub connect: Option<String>,
    /// Render one frame of `screen` to this PNG and exit (used for UI checks).
    pub screenshot: Option<PathBuf>,
    pub screen: Option<String>,
    /// How long to let the UI settle before the screenshot is taken.
    pub screenshot_after_ms: u64,
    /// Use built-in sample hosts and never touch the saved config.
    pub demo: bool,
    /// Start installing Kyber right away if it is missing.
    pub setup: bool,
    /// Start hosting right away.
    pub host: bool,
    /// Run a latency lab at startup: "measure", "fastest" or "other".
    pub lab: Option<String>,
    /// Print the lab results and close once it is done.
    pub exit_after_lab: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Host,
    Setup,
    Hosts,
    Settings,
    Editor,
    Stream,
}

struct Editor {
    draft: Host,
    original: Host,
    secret: String,
    original_secret: String,
    is_new: bool,
    selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextTarget {
    HostName,
    HostAddress,
    HostPort,
    HostUsername,
    HostSecret,
    HostFingerprint,
    HostMac,
    ClientPath,
    HostingUsername,
    HostingPassword,
    HostingPort,
}

struct TextInput {
    target: TextTarget,
    title: String,
    value: String,
    secret: bool,
    /// Selected on-screen key as (row, column).
    key: (usize, usize),
    shift: bool,
}

struct Confirm {
    title: String,
    body: String,
    delete_host: u64,
    yes_selected: bool,
}

struct Toast {
    text: String,
    colour: Color32,
    until: Instant,
}

struct Ended {
    code: Option<i32>,
    log: Vec<String>,
    ran_for: Duration,
    reason: Option<EndReason>,
}

struct StreamView {
    host_id: u64,
    host_name: String,
    command_line: String,
    ended: Option<Ended>,
}

struct Scan {
    events: Receiver<ScanEvent>,
    added: usize,
}

struct PendingWake {
    host_id: u64,
    since: Instant,
}

struct Shot {
    path: PathBuf,
    at: Instant,
    requested: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SRow {
    Preset,
    Codec,
    Bitrate,
    Yuv444,
    SoftwareDecode,
    VideoBuffer,
    Audio,
    AudioBuffer,
    DisplayIndex,
    DisplayCount,
    LabMeasure,
    LabFastest,
    LabOther,
    LiveStats,
    UseHostCodec,
    LabResult(usize),
    Protocol,
    VideoTransport,
    AudioTransport,
    IntraRefresh,
    AutoReconnect,
    Inputs,
    KeyboardGrab,
    Xbox360,
    Clipboard,
    StartFullscreen,
    LauncherFullscreen,
    AutoConnect,
    QuitButton,
    LogLevel,
    ClientPath,
    GetKyber,
    ConfigFile,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ERow {
    Save,
    Name,
    Address,
    Port,
    Auth,
    Username,
    Secret,
    Tls,
    Fingerprint,
    Mac,
    Pair,
    Wake,
    ForgetCert,
    Delete,
}

pub struct App {
    config: Config,
    /// Set when the saved config could not be read; nothing is written then,
    /// so a damaged file is never replaced by an empty one.
    config_error: Option<String>,
    read_only: bool,
    demo_status: HashMap<u64, HostStatus>,

    pad: Pad,
    monitor: net::Monitor,
    client_path: Option<PathBuf>,
    client_version: Arc<Mutex<Option<String>>>,

    screen: Screen,
    host_selected: usize,
    scroll_to_host: bool,
    settings_tab: usize,
    settings_selected: usize,
    editor: Option<Editor>,
    text_input: Option<TextInput>,
    confirm: Option<Confirm>,
    toast: Option<Toast>,

    session: Option<Session>,
    stream: Option<StreamView>,
    pending_wake: Option<PendingWake>,
    scan: Option<Scan>,
    connect_on_start: Option<u64>,
    shot: Option<Shot>,
    fx: ui::Confetti,
    lab: Option<crate::lab::Lab>,
    lab_purpose: latency::Purpose,
    lab_results: Vec<crate::lab::LabResult>,
    /// Ctrl+Alt+Shift+S hides the statistics for the current session.
    stats_visible: bool,
    exit_after_lab: bool,
    discovery: Option<crate::discovery::Discovery>,
    pairing: pairing::PairingState,
    /// For the little entrance animation when the screen changes.
    last_screen: Screen,
    screen_since: f64,
    hosting: hosting::HostingState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: StartupArgs) -> Self {
        ui::apply_theme(&cc.egui_ctx);
        crate::fx::init(cc.gl.as_deref());

        let (mut config, config_error) = if args.demo {
            (Config::first_run(), None)
        } else {
            match Config::load() {
                Ok(config) => (config, None),
                Err(error) => (Config::first_run(), Some(error)),
            }
        };

        let mut demo_status = HashMap::new();
        if args.demo {
            let online = |ms| HostStatus {
                reachability: Reachability::Online(ms),
                mac: None,
            };
            let sample = [
                ("Gaming PC", "192.168.1.20", online(2), ""),
                (
                    "Workstation",
                    "192.168.1.42",
                    HostStatus {
                        reachability: Reachability::Offline,
                        mac: None,
                    },
                    "A4:5E:60:11:22:33",
                ),
                ("Living room", "kyber-livingroom.lan", online(11), ""),
            ];
            for (index, (name, address, status, mac)) in sample.into_iter().enumerate() {
                let id = index as u64 + 1;
                config.hosts.push(Host {
                    id,
                    name: name.into(),
                    address: address.into(),
                    mac: mac.into(),
                    ..Host::default()
                });
                demo_status.insert(id, status);
            }
        }

        let repaint = cc.egui_ctx.clone();
        let monitor = net::Monitor::start(move || repaint.request_repaint());
        monitor.set_paused(args.demo);
        monitor.set_hosts(&config.hosts);

        let connect_on_start = match &args.connect {
            Some(wanted) => config
                .hosts
                .iter()
                .find(|h| {
                    h.name.eq_ignore_ascii_case(wanted) || h.address.eq_ignore_ascii_case(wanted)
                })
                .map(|h| h.id),
            None if config.auto_connect_last => config.last_host().map(|h| h.id),
            None => None,
        };

        let mut app = Self {
            read_only: args.demo || config_error.is_some(),
            config,
            config_error,
            demo_status,
            pad: Pad::new(),
            monitor,
            client_path: None,
            client_version: Arc::default(),
            screen: Screen::Home,
            host_selected: 0,
            scroll_to_host: false,
            settings_tab: 0,
            settings_selected: 0,
            editor: None,
            text_input: None,
            confirm: None,
            toast: None,
            session: None,
            stream: None,
            pending_wake: None,
            scan: None,
            connect_on_start,
            shot: args.screenshot.map(|path| Shot {
                path,
                at: Instant::now() + Duration::from_millis(args.screenshot_after_ms),
                requested: false,
            }),
            hosting: hosting::HostingState::new(args.demo),
            fx: ui::Confetti::default(),
            lab: None,
            lab_purpose: latency::Purpose::Show,
            lab_results: if args.demo {
                crate::lab::demo_results()
            } else {
                crate::lab::load_results()
            },
            stats_visible: true,
            exit_after_lab: args.exit_after_lab,
            discovery: (!args.demo).then(|| {
                let repaint = cc.egui_ctx.clone();
                crate::discovery::Discovery::start(move || repaint.request_repaint())
            }),
            pairing: pairing::PairingState::default(),
            last_screen: Screen::Home,
            screen_since: 0.0,
        };
        app.refresh_client();
        app.hosting.start_on_launch = args.host;
        if let Some(which) = args.lab.as_deref() {
            let plan = match which {
                "fastest" => crate::lab::Plan::FindFastest,
                "other" => crate::lab::Plan::OtherApp { seconds: 20 },
                _ => crate::lab::Plan::Kyber {
                    encoder: app.config.hosting.encoder_or_default(),
                    codec: app
                        .config
                        .hosting
                        .best_codec
                        .unwrap_or(app.config.stream.codec),
                },
            };
            let purpose = if which == "fastest" {
                latency::Purpose::PickEncoder
            } else {
                latency::Purpose::Show
            };
            app.start_lab(plan, purpose, &cc.egui_ctx);
        }
        app.first_run(args.setup, &cc.egui_ctx);
        if args.connect.is_some() && app.connect_on_start.is_none() {
            app.toast_error("No saved host matches the --connect argument.");
        }
        if args.demo {
            app.pairing.demo_found = vec![crate::discovery::Found {
                name: "LIVING-ROOM-PC".into(),
                address: std::net::IpAddr::from([192, 168, 1, 33]),
                agent_port: crate::agent::DEFAULT_PORT,
                kyber_port: 8080,
                fingerprint: String::new(),
                pairing: true,
                hosting: false,
            }];
        }
        app.apply_demo_screen(args.screen.as_deref());
        app
    }

    /// Jumps straight to a screen; only used together with `--screenshot`.
    fn apply_demo_screen(&mut self, screen: Option<&str>) {
        let first = self.config.hosts.first().map(|h| h.id);
        match screen {
            Some("hosts") => self.screen = Screen::Hosts,
            Some("host") => self.open_host(),
            Some("setup") => self.screen = Screen::Setup,
            Some("settings") => self.open_settings(Screen::Home),
            Some("pairing") => {
                self.open_host();
                if let Ok(agent) = crate::agent::Agent::start(0, || {}) {
                    agent.open_pairing();
                    self.pairing.agent = Some(agent);
                    self.pairing.open = true;
                }
            }
            Some(name @ ("paircode" | "paircode-typed")) => {
                self.screen = Screen::Hosts;
                self.begin_pairing(pairing::PairTarget {
                    name: "LIVING-ROOM-PC".into(),
                    address: "192.168.1.33".into(),
                    agent_port: crate::agent::DEFAULT_PORT,
                    existing: None,
                });
                if name == "paircode-typed" {
                    if let Some(entry) = &mut self.pairing.entry {
                        entry.digits = "4821".into();
                        entry.key = 4;
                    }
                }
            }
            Some("network") => {
                self.open_settings(Screen::Home);
                self.settings_tab = 1;
            }
            Some("hosting-settings") => {
                self.open_settings(Screen::Host);
                self.settings_tab = SETTINGS_HOSTING;
            }
            Some("latency") => {
                self.open_settings(Screen::Home);
                self.settings_tab = SETTINGS_LATENCY;
            }
            Some("launcher") => {
                self.open_settings(Screen::Home);
                self.settings_tab = 5;
            }
            Some("editor") => self.open_editor(first),
            Some("input") => {
                self.open_editor(first);
                self.open_text_input(TextTarget::HostAddress);
            }
            Some("confirm") => {
                self.open_editor(first);
                self.act_on_editor(ERow::Delete, RowAct::Accept);
            }
            Some("ended") => {
                self.screen = Screen::Stream;
                self.stream = Some(StreamView {
                    host_id: first.unwrap_or(0),
                    host_name: "Gaming PC".into(),
                    command_line: "kyclient.exe --video-codec hevc --tls-tofu 192.168.1.20".into(),
                    ended: Some(Ended {
                        code: Some(1),
                        log: vec![
                            "INFO  kyclient: Connecting to 192.168.1.20:8080".into(),
                            "ERROR kyclient: Connect failed: Unauthorized".into(),
                        ],
                        ran_for: Duration::from_secs(2),
                        reason: Some(EndReason::WrongSignIn),
                    }),
                });
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ helpers

    fn status(&self, host_id: u64) -> HostStatus {
        match self.demo_status.get(&host_id) {
            Some(status) => *status,
            None => self.monitor.status(host_id),
        }
    }

    fn toast_info(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            colour: ui::ACCENT,
            until: Instant::now() + Duration::from_secs(4),
        });
    }

    fn toast_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            colour: ui::ERR,
            until: Instant::now() + Duration::from_secs(7),
        });
    }

    fn save(&mut self) {
        if self.read_only {
            return;
        }
        if let Err(error) = self.config.save() {
            self.toast_error(format!("Could not save settings: {error}"));
        }
    }

    fn hosts_changed(&mut self) {
        self.monitor.set_hosts(&self.config.hosts);
        self.save();
    }

    fn refresh_client(&mut self) {
        self.client_path = kyclient::locate(&self.config.kyclient_path);
        self.refresh_install();
        *self.client_version.lock().unwrap() = None;
        if let Some(path) = self.client_path.clone() {
            let slot = self.client_version.clone();
            std::thread::spawn(move || {
                let version = kyclient::query_version(&path);
                *slot.lock().unwrap() = version;
            });
        }
    }

    fn needs_secret(host: &Host) -> bool {
        match host.auth {
            AuthMode::Basic => !host.username.is_empty(),
            AuthMode::Token => true,
            AuthMode::Oidc => false,
        }
    }

    // ---------------------------------------------------------- streaming

    /// Connects to a host, waking it first if it is asleep and we know how.
    fn connect(&mut self, host_id: u64, ctx: &egui::Context) {
        if self.session.is_some() {
            return;
        }
        let Some(host) = self.config.host(host_id).cloned() else {
            return;
        };
        if self.client_path.is_none() {
            self.toast_error(
                "The Kyber client was not found. Install Kyber or set its location in Settings > App.",
            );
            return;
        }
        if self.status(host_id).reachability == Reachability::Offline {
            if let Some(mac) = net::parse_mac(&host.mac) {
                match net::wake_on_lan(mac) {
                    Ok(()) => {
                        self.pending_wake = Some(PendingWake {
                            host_id,
                            since: Instant::now(),
                        });
                        self.toast_info(format!(
                            "Waking {}. The stream starts as soon as it answers.",
                            host.display_name()
                        ));
                    }
                    Err(error) => self.toast_error(format!("Wake-on-LAN failed: {error}")),
                }
                return;
            }
        }
        self.launch(host_id, ctx);
    }

    fn launch(&mut self, host_id: u64, ctx: &egui::Context) {
        let (Some(host), Some(binary)) =
            (self.config.host(host_id).cloned(), self.client_path.clone())
        else {
            return;
        };
        let secret = if Self::needs_secret(&host) && !self.read_only {
            match secrets::get(host.id) {
                Ok(secret) => secret,
                Err(error) => {
                    self.toast_error(format!("Could not read the saved sign-in: {error}"));
                    return;
                }
            }
        } else {
            None
        };
        if let Err(problem) = kyclient::validate(&host, secret.as_deref()) {
            self.toast_error(problem);
            return;
        }

        let repaint = ctx.clone();
        // Live statistics need Kyber's per-frame metrics.
        let mut settings = self.config.stream.clone();
        if !self.config.hide_stats {
            settings.metrics = true;
        }
        if settings.use_host_codec {
            if let Some(codec) = host.codec_hint {
                settings.codec = codec;
            }
        }
        let options = LaunchOptions {
            start_fullscreen: self.config.start_fullscreen,
            log_dir: Config::dir().join("client-log"),
            host_address: host.address.trim().to_owned(),
            host_port: host.port,
        };
        match Session::start(
            &binary,
            &host,
            &settings,
            secret.as_deref(),
            options,
            move || repaint.request_repaint(),
        ) {
            Ok(session) => {
                self.stream = Some(StreamView {
                    host_id,
                    host_name: host.display_name().to_owned(),
                    command_line: session.command_line.clone(),
                    ended: None,
                });
                self.session = Some(session);
                self.screen = Screen::Stream;
                self.pending_wake = None;
                self.monitor.set_paused(true);
                if let Some(saved) = self.config.host_mut(host_id) {
                    saved.last_connected = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(1);
                }
                self.save();
            }
            Err(error) => self.toast_error(error),
        }
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        self.poll_hosting(ctx);
        self.poll_lab(ctx);
        self.poll_agent(ctx);
        self.poll_pairing(ctx);
        self.refresh_statuses();
        if let Some(host_id) = self.connect_on_start.take() {
            self.connect(host_id, ctx);
        }

        // Stream finished?
        let finished = self
            .session
            .as_ref()
            .and_then(|session| match session.state() {
                SessionState::Running => None,
                SessionState::Exited(code) => Some((
                    code,
                    session.stopped_by_user(),
                    session.end_reason(),
                    session.started.elapsed(),
                    session.log_tail(14),
                )),
            });
        if let Some((code, by_user, reason, ran_for, log)) = finished {
            self.session = None;
            self.monitor.set_paused(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            let lasted = format_duration(ran_for);
            if reason == Some(EndReason::HostStopped) {
                self.screen = Screen::Hosts;
                self.stream = None;
                self.toast_info(format!(
                    "The host stopped sharing. Stream ended after {lasted}."
                ));
            } else if by_user || code == Some(0) {
                self.screen = Screen::Hosts;
                self.stream = None;
                self.toast_info(format!("Stream ended after {lasted}."));
            } else if let Some(view) = &mut self.stream {
                view.ended = Some(Ended {
                    code,
                    log,
                    ran_for,
                    reason,
                });
            }
        }

        // LAN scan results.
        let mut scan_done = None;
        if let Some(scan) = &mut self.scan {
            let mut new_hosts = Vec::new();
            while let Ok(event) = scan.events.try_recv() {
                match event {
                    ScanEvent::Found(found) => {
                        let address = found.ip.to_string();
                        let known = self
                            .config
                            .hosts
                            .iter()
                            .chain(new_hosts.iter())
                            .any(|h: &Host| h.address.trim() == address);
                        if !known {
                            new_hosts.push(Host {
                                id: Host::new_id() + new_hosts.len() as u64 * 2,
                                name: format!("Kyber host {address}"),
                                address,
                                port: found.port,
                                ..Host::default()
                            });
                        }
                    }
                    ScanEvent::Done { probed } => scan_done = Some(probed),
                }
            }
            scan.added += new_hosts.len();
            if !new_hosts.is_empty() {
                self.config.hosts.extend(new_hosts);
                self.hosts_changed();
            }
        }
        if let Some(probed) = scan_done {
            let added = self.scan.take().map(|s| s.added).unwrap_or(0);
            match (probed, added) {
                (0, _) => self.toast_error("No local network to search was found."),
                (_, 0) => self.toast_info(format!(
                    "No new Kyber hosts among {probed} addresses. Add the host by address instead."
                )),
                (_, n) => self.toast_info(format!("Found {n} new Kyber host(s).")),
            }
        }

        // Host we are waiting for after a wake packet.
        if let Some(wake) = &self.pending_wake {
            let host_id = wake.host_id;
            if matches!(self.status(host_id).reachability, Reachability::Online(_)) {
                self.pending_wake = None;
                self.launch(host_id, ctx);
            } else if wake.since.elapsed() > WAKE_TIMEOUT {
                self.pending_wake = None;
                self.toast_error("The host did not come online after the wake-up packet.");
            }
        }

        // Remember MAC addresses seen while hosts are online, for Wake-on-LAN.
        let mut learned = false;
        for index in 0..self.config.hosts.len() {
            if !self.config.hosts[index].mac.is_empty() {
                continue;
            }
            if let Some(mac) = self.monitor.status(self.config.hosts[index].id).mac {
                self.config.hosts[index].mac = net::format_mac(mac);
                learned = true;
            }
        }
        if learned {
            self.save();
        }
    }

    // -------------------------------------------------------- hosts screen

    fn hosts_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav], ctx: &egui::Context) {
        let host_count = self.config.hosts.len();
        let discovered = self.discovered_hosts();
        let found_count = discovered.len();
        let total = host_count + found_count + 2;
        self.host_selected = self.host_selected.min(total - 1);

        let available = ui.available_width() - 48.0;
        let columns = (((available + ui::TILE_GAP) / (ui::TILE.x + ui::TILE_GAP)).floor() as usize)
            .clamp(1, total);
        let header_width = columns as f32 * ui::TILE.x + (columns - 1) as f32 * ui::TILE_GAP;
        ui.add_space(10.0);
        centered(ui, header_width, |ui| {
            let label = match host_count {
                1 => "Join \u{b7} 1 saved host".to_owned(),
                n => format!("Join \u{b7} {n} saved hosts"),
            };
            ui::caps(ui, &label, ui::LIME);
            ui::headline(ui, "Pick a *host*.", 46.0);
        });
        ui.add_space(14.0);
        self.banners(ui);

        let mut selected = self.host_selected;
        let mut moved = std::mem::take(&mut self.scroll_to_host);
        let mut activate = None;
        let mut edit = None;
        for nav in navs {
            let before = selected;
            match nav {
                Nav::Left if selected % columns > 0 => selected -= 1,
                Nav::Right if selected % columns + 1 < columns && selected + 1 < total => {
                    selected += 1
                }
                Nav::Up if selected >= columns => selected -= columns,
                Nav::Down if selected + columns < total => selected += columns,
                // Last row is shorter: land on its final tile.
                Nav::Down if selected / columns < (total - 1) / columns => selected = total - 1,
                Nav::Accept => activate = Some(selected),
                Nav::Edit => edit = Some(selected),
                Nav::Settings => self.open_settings(Screen::Hosts),
                Nav::Back if self.pending_wake.is_some() => self.pending_wake = None,
                Nav::Back => self.screen = Screen::Home,
                _ => {}
            }
            moved |= before != selected;
        }

        // Describe the tiles before drawing so `self` is free while painting.
        struct Card {
            title: String,
            subtitle: String,
            status: Option<(String, Color32)>,
            icon: u8,
        }
        let mut cards: Vec<Card> = Vec::with_capacity(total);
        for host in &self.config.hosts {
            let waking = self
                .pending_wake
                .as_ref()
                .is_some_and(|w| w.host_id == host.id);
            let live = self.pairing.live.get(&host.id);
            let status = if waking {
                ("Waking up...".to_owned(), ui::WARN)
            } else {
                match self.status(host.id).reachability {
                    Reachability::Online(ms) if live.is_some_and(|l| l.hosting) => {
                        (format!("Hosting \u{b7} {ms} ms"), ui::OK)
                    }
                    Reachability::Online(ms) => (format!("Online \u{b7} {ms} ms"), ui::OK),
                    Reachability::Offline if net::parse_mac(&host.mac).is_some() => {
                        ("Offline \u{b7} can be woken".to_owned(), ui::DIM)
                    }
                    Reachability::Offline => ("Offline".to_owned(), ui::DIM),
                    Reachability::Unknown => ("Checking...".to_owned(), ui::DIM),
                }
            };
            let subtitle = if host.port == DEFAULT_PORT {
                host.address.clone()
            } else {
                format!("{}:{}", host.address, host.port)
            };
            cards.push(Card {
                title: host.display_name().to_owned(),
                subtitle,
                status: Some(status),
                icon: 0,
            });
        }
        for found in &discovered {
            let status = if found.pairing {
                ("Pairing open \u{b7} press A".to_owned(), ui::LIME)
            } else if found.hosting {
                ("Hosting \u{b7} pair to join".to_owned(), ui::LAVENDER)
            } else {
                ("On your network".to_owned(), ui::DIM)
            };
            cards.push(Card {
                title: found.name.clone(),
                subtitle: found.address.to_string(),
                status: Some(status),
                icon: 3,
            });
        }
        cards.push(Card {
            title: "Add host".into(),
            subtitle: "Enter an address".into(),
            status: None,
            icon: 1,
        });
        cards.push(Card {
            title: if self.scan.is_some() {
                "Searching...".into()
            } else {
                "Find hosts".into()
            },
            subtitle: "Search this network".into(),
            status: None,
            icon: 2,
        });

        let grid_width = columns as f32 * ui::TILE.x + (columns - 1) as f32 * ui::TILE_GAP;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let left = ((ui.available_width() - grid_width) * 0.5).max(0.0);
                ui.spacing_mut().item_spacing = vec2(ui::TILE_GAP, ui::TILE_GAP);
                ui.add_space(20.0);
                for (row, chunk) in cards.chunks(columns).enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_space(left);
                        for (column, card) in chunk.iter().enumerate() {
                            let index = row * columns + column;
                            let tile = Tile {
                                icon: match card.icon {
                                    0 | 3 => TileIcon::Monitor,
                                    1 => TileIcon::Plus,
                                    _ => TileIcon::Search,
                                },
                                title: &card.title,
                                subtitle: &card.subtitle,
                                status: card
                                    .status
                                    .as_ref()
                                    .map(|(text, colour)| (text.as_str(), *colour)),
                                outlined: card.icon != 0,
                                number: index + 1,
                            };
                            let is_selected = index == selected;
                            let response = ui::tile(ui, &tile, is_selected, is_selected && moved);
                            if response.clicked() {
                                selected = index;
                                activate = Some(index);
                            }
                            if response.secondary_clicked() {
                                selected = index;
                                edit = Some(index);
                            }
                        }
                    });
                }
                ui.add_space(12.0);
            });

        self.host_selected = selected;
        if let Some(index) = edit.filter(|i| *i < host_count) {
            let id = self.config.hosts[index].id;
            self.open_editor(Some(id));
        } else if let Some(index) = activate {
            if index < host_count {
                let id = self.config.hosts[index].id;
                self.connect(id, ctx);
            } else if index < host_count + found_count {
                let found = &discovered[index - host_count];
                self.begin_pairing(pairing::PairTarget {
                    name: found.name.clone(),
                    address: found.address.to_string(),
                    agent_port: found.agent_port,
                    existing: None,
                });
            } else if index == host_count + found_count {
                self.open_editor(None);
            } else {
                self.start_scan(ctx);
            }
        }
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        let mut messages: Vec<(String, Color32)> = Vec::new();
        if let Some(error) = &self.config_error {
            messages.push((
                format!(
                    "Saved settings were not loaded ({error}). Changes made now are not saved."
                ),
                ui::ERR,
            ));
        }
        if self.client_path.is_none() {
            messages.push((
                "The Kyber client (kyclient) is not installed or was not found. Install Kyber, or set the client location in Settings > App.".to_owned(),
                ui::WARN,
            ));
        }
        for (text, colour) in messages {
            centered(ui, 900.0, |ui| {
                egui::Frame::new()
                    .fill(ui::PANEL)
                    .stroke(Stroke::new(1.0, colour))
                    .corner_radius(10.0)
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(text).color(colour).size(15.0));
                    });
            });
            ui.add_space(12.0);
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scan.is_some() {
            return;
        }
        let repaint = ctx.clone();
        self.scan = Some(Scan {
            events: net::scan_lan(DEFAULT_PORT, move || repaint.request_repaint()),
            added: 0,
        });
        self.toast_info("Searching this network for Kyber hosts on port 8080...");
    }

    // ----------------------------------------------------- settings screen

    fn settings_rows(&self) -> Vec<Row<SRow>> {
        let s = &self.config.stream;
        let c = &self.config;
        let toggle = |id, label: &str, on: bool| Row::new(id, RowKind::Toggle(on), label, "");
        let choice = |id, label: &str, value: &str| Row::new(id, RowKind::Choice, label, value);
        let ms = |value: u32, zero: &str| {
            if value == 0 {
                zero.to_owned()
            } else {
                format!("{value} ms")
            }
        };
        let rows = match self.settings_tab {
            0 => vec![
                choice(
                    SRow::Preset,
                    "Preset",
                    s.matching_preset().map(Preset::label).unwrap_or("Custom"),
                )
                .help("Starting points. Speed keeps Kyber's low-latency defaults (H.264, no buffering); Balanced and Quality trade bandwidth for picture quality; Lossy network adds error correction."),
                choice(SRow::Codec, "Video codec", s.codec.label())
                    .help("H.264 decodes fastest on most hardware. HEVC gives a better picture at the same bitrate. Kyber's own docs call AV1 highly experimental."),
                choice(
                    SRow::Bitrate,
                    "Bitrate",
                    &if s.bitrate_mbps == 0 {
                        "Auto (server default)".to_owned()
                    } else {
                        format!("{} Mbit/s", s.bitrate_mbps)
                    },
                )
                .help("Auto leaves the bitrate to the host. While streaming, Ctrl+B+Up / Ctrl+B+Down adjusts it live."),
                toggle(SRow::Yuv444, "Full colour (4:4:4)", s.yuv444)
                    .help("Sharper text and fine detail, at the cost of bandwidth. The host encoder must support it."),
                toggle(SRow::SoftwareDecode, "Software decoding", s.software_decode)
                    .help("Decode video on the CPU. Slower; only useful when hardware decoding crashes or shows artefacts."),
                choice(SRow::VideoBuffer, "Video buffer", &ms(s.video_buffer_ms, "None (lowest latency)"))
                    .help("Extra delay before frames are shown. Leave at none unless the picture stutters from network jitter."),
                toggle(SRow::Audio, "Audio", s.audio),
                choice(SRow::AudioBuffer, "Audio buffer", &ms(s.audio_buffer_ms, "Client default"))
                    .help("Larger buffers hide crackling on unstable links but delay the sound."),
                choice(
                    SRow::DisplayIndex,
                    "Display",
                    &if s.display_count > 1 {
                        "All selected below".to_owned()
                    } else {
                        format!("Display {}", s.display_index + 1)
                    },
                )
                .help("Which of the host's displays to stream."),
                choice(
                    SRow::DisplayCount,
                    "Displays to stream",
                    &if s.display_count > 1 {
                        format!("{} displays", s.display_count)
                    } else {
                        "One".to_owned()
                    },
                )
                .help("Stream several host displays at once, each in its own window."),
            ],
            1 => vec![
                toggle(SRow::UseHostCodec, "Use the host's fastest codec", s.use_host_codec)
                    .help("When a paired host has measured which codec is fastest on its graphics card, stream with that one instead of the codec chosen above."),
                choice(SRow::Protocol, "Protocol", s.protocol.label())
                    .help("Kymux over QUIC is Kyber's native transport and the default. WebSocket and RTP are alternatives for networks where QUIC (UDP) is blocked."),
                choice(SRow::VideoTransport, "Video delivery", s.video_transport.label())
                    .help("Reliable (default) resends lost data. The unreliable modes skip resending so a lost packet never holds up newer frames; FEC adds redundancy to recover from loss."),
                choice(SRow::AudioTransport, "Audio delivery", s.audio_transport.label())
                    .help("Same choices as video, applied to the audio stream."),
                choice(SRow::IntraRefresh, "Intra refresh", s.intra_refresh.label())
                    .help("Spreads keyframe data across frames to avoid bitrate spikes. Kyber supports it with Kymux and NVIDIA encoding."),
                toggle(SRow::AutoReconnect, "Reconnect automatically", s.auto_reconnect)
                    .help("Let the client reconnect by itself when the connection drops."),
            ],
            2 => vec![
                toggle(SRow::Inputs, "Send keyboard, mouse and gamepads", s.inputs)
                    .help("Turn off for a view-only stream. Gamepads need the ViGEmBus driver on Windows hosts."),
                toggle(SRow::KeyboardGrab, "Capture system shortcuts", s.keyboard_grab)
                    .help("Send Alt+Tab, the Windows key and similar shortcuts to the host instead of this PC."),
                choice(SRow::Xbox360, "Present gamepads as Xbox 360", s.gamepad_xbox360.label())
                    .help("Makes every controller appear as an Xbox 360 pad on a Windows host, for games that only understand XInput."),
                choice(SRow::Clipboard, "Shared clipboard", s.clipboard.label()),
            ],
            SETTINGS_LATENCY => self.latency_rows(),
            _ => vec![
                toggle(SRow::StartFullscreen, "Start streams fullscreen", c.start_fullscreen)
                    .help("kyclient has no fullscreen option, so the launcher presses Ctrl+Alt+F for you once the stream window opens (Windows only)."),
                toggle(SRow::LauncherFullscreen, "Launcher fullscreen", c.launcher_fullscreen)
                    .help("Also toggled with F11 or the Start button."),
                toggle(SRow::AutoConnect, "Connect to the last host at startup", c.auto_connect_last),
                toggle(SRow::QuitButton, "Leave button over the stream", !c.hide_quit_button)
                    .help("A small always-on-top button in the top-right corner of the screen while you are streaming: one click ends the stream. Ctrl+Alt+Shift+Q and holding Start + Back + LB + RB work as well."),
                choice(SRow::LogLevel, "Client log level", s.log_level.label())
                    .help("How much kyclient writes to its log. Raise it when diagnosing a failed connection."),
                Row::new(
                    SRow::ClientPath,
                    RowKind::Text,
                    "Kyber client location",
                    match (&self.client_path, c.kyclient_path.is_empty()) {
                        (Some(path), _) => path.display().to_string(),
                        (None, true) => "Not found".to_owned(),
                        (None, false) => format!("Not found: {}", c.kyclient_path),
                    },
                )
                .help("Path to kyclient.exe or its folder. Leave empty to search the usual install locations."),
                Row::new(SRow::GetKyber, RowKind::Action, "Get Kyber", "Open the download page")
                    .help("Opens the official Kyber installer releases in your browser. The installer contains both the client and the host."),
                Row::new(
                    SRow::ConfigFile,
                    RowKind::Info,
                    "Settings file",
                    Config::path().display().to_string(),
                ),
            ],
        };
        rows.into_iter()
            .map(|row| match srow_note(row.id) {
                "" => row,
                note => row.note(note),
            })
            .collect()
    }

    fn act_on_setting(&mut self, row: SRow, act: RowAct, ctx: &egui::Context) {
        let delta = if act == RowAct::Prev { -1 } else { 1 };
        let s = &mut self.config.stream;
        match row {
            SRow::Preset => {
                let presets = Preset::ALL;
                let next = match s.matching_preset() {
                    Some(current) => current.step(delta),
                    None if delta > 0 => presets[0],
                    None => presets[presets.len() - 1],
                };
                s.apply_preset(next);
            }
            SRow::Codec => s.codec = s.codec.step(delta),
            SRow::Bitrate => s.bitrate_mbps = step_in(BITRATES, s.bitrate_mbps, delta),
            SRow::Yuv444 => s.yuv444 = !s.yuv444,
            SRow::SoftwareDecode => s.software_decode = !s.software_decode,
            SRow::VideoBuffer => {
                s.video_buffer_ms = step_in(VIDEO_BUFFERS, s.video_buffer_ms, delta)
            }
            SRow::Audio => s.audio = !s.audio,
            SRow::AudioBuffer => {
                s.audio_buffer_ms = step_in(AUDIO_BUFFERS, s.audio_buffer_ms, delta)
            }
            SRow::DisplayIndex => {
                s.display_index = (s.display_index as i32 + delta).clamp(0, 7) as u32;
                s.display_count = 0;
            }
            SRow::DisplayCount => {
                let count = (s.display_count.max(1) as i32 + delta).clamp(1, 4) as u32;
                s.display_count = if count > 1 { count } else { 0 };
            }
            SRow::UseHostCodec => s.use_host_codec = !s.use_host_codec,
            SRow::LabMeasure
            | SRow::LabFastest
            | SRow::LabOther
            | SRow::LiveStats
            | SRow::LabResult(_) => {
                self.act_on_latency(row, act, ctx);
                return;
            }
            SRow::Protocol => s.protocol = s.protocol.step(delta),
            SRow::VideoTransport => s.video_transport = s.video_transport.step(delta),
            SRow::AudioTransport => s.audio_transport = s.audio_transport.step(delta),
            SRow::IntraRefresh => s.intra_refresh = s.intra_refresh.step(delta),
            SRow::AutoReconnect => s.auto_reconnect = !s.auto_reconnect,
            SRow::Inputs => s.inputs = !s.inputs,
            SRow::KeyboardGrab => s.keyboard_grab = !s.keyboard_grab,
            SRow::Xbox360 => s.gamepad_xbox360 = s.gamepad_xbox360.step(delta),
            SRow::Clipboard => s.clipboard = s.clipboard.step(delta),
            SRow::LogLevel => s.log_level = s.log_level.step(delta),
            SRow::StartFullscreen => self.config.start_fullscreen = !self.config.start_fullscreen,
            SRow::LauncherFullscreen => self.set_fullscreen(!self.config.launcher_fullscreen, ctx),
            SRow::AutoConnect => self.config.auto_connect_last = !self.config.auto_connect_last,
            SRow::QuitButton => self.config.hide_quit_button = !self.config.hide_quit_button,
            SRow::ClientPath => {
                if act == RowAct::Accept {
                    self.open_text_input(TextTarget::ClientPath);
                }
                return;
            }
            SRow::GetKyber => {
                if act == RowAct::Accept {
                    match open_url(RELEASES_URL) {
                        Ok(()) => {
                            self.toast_info("Opened the Kyber download page in your browser.")
                        }
                        Err(error) => {
                            self.toast_error(format!("Could not open the browser: {error}"))
                        }
                    }
                }
                return;
            }
            SRow::ConfigFile => return,
        }
        self.save();
    }

    fn set_fullscreen(&mut self, on: bool, ctx: &egui::Context) {
        self.config.launcher_fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        self.save();
    }

    fn settings_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav], ctx: &egui::Context) {
        let mut switch_to = None;
        for nav in navs {
            match nav {
                Nav::PrevTab if self.settings_tab > 0 => switch_to = Some(self.settings_tab - 1),
                Nav::NextTab if self.settings_tab + 1 < SETTINGS_TABS.len() => {
                    switch_to = Some(self.settings_tab + 1)
                }
                Nav::Back | Nav::Settings => {
                    self.screen = self.hosting.settings_from;
                    return;
                }
                _ => {}
            }
        }

        let mut setting = None;
        let mut host_setting = None;
        centered(ui, 1120.0, |ui| {
            ui.add_space(10.0);
            ui::caps(ui, "Saved as you go \u{b7} LB / RB switches section", ui::DIM);
            ui.add_space(10.0);
            let nav_width = 280.0;
            let gap = 30.0;
            let height = ui.available_height() - 8.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(
                    vec2(nav_width, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for (index, (title, blurb, _)) in SETTINGS_TABS.iter().enumerate() {
                            if ui::nav_item(ui, nav_width, index, title, blurb, index == self.settings_tab)
                                .clicked()
                            {
                                switch_to = Some(index);
                            }
                        }
                    },
                );
                let width = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(width, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let (title, _, intro) = SETTINGS_TABS[self.settings_tab];
                        ui::headline(ui, &format!("*{title}*"), 40.0);
                        ui.label(RichText::new(intro).color(ui::DIM).size(15.0));
                        ui.add_space(10.0);
                        let list_height = (ui.available_height() - 52.0).max(120.0);
                        let mut selected = self.settings_selected;
                        let help;
                        if self.settings_tab == SETTINGS_HOSTING {
                            let rows = self.host_settings_rows();
                            ui.allocate_ui(vec2(width - 14.0, list_height), |ui| {
                                host_setting = ui::row_list(ui, &rows, &mut selected, navs);
                            });
                            help = rows.get(selected).map(|r| r.help).unwrap_or_default();
                        } else {
                            let rows = self.settings_rows();
                            ui.allocate_ui(vec2(width - 14.0, list_height), |ui| {
                                setting = ui::row_list(ui, &rows, &mut selected, navs);
                            });
                            help = rows.get(selected).map(|r| r.help).unwrap_or_default();
                        }
                        self.settings_selected = selected;
                        help_line(ui, help);
                    },
                );
            });
        });
        if let Some((row, act)) = setting {
            self.act_on_setting(row, act, ctx);
        }
        if let Some((row, act)) = host_setting {
            self.act_on_host_row(row, act, ctx);
        }
        if let Some(tab) = switch_to {
            if tab != self.settings_tab {
                self.settings_tab = tab;
                self.settings_selected = 0;
            }
        }
    }

    // --------------------------------------------------------- host editor

    fn open_editor(&mut self, host_id: Option<u64>) {
        let existing = host_id.and_then(|id| self.config.host(id).cloned());
        let is_new = existing.is_none();
        let draft = existing.unwrap_or_default();
        let secret = if !is_new && !self.read_only && draft.auth != AuthMode::Oidc {
            secrets::get(draft.id).ok().flatten().unwrap_or_default()
        } else {
            String::new()
        };
        self.editor = Some(Editor {
            original: draft.clone(),
            draft,
            original_secret: secret.clone(),
            secret,
            is_new,
            // New hosts start on the address row, existing ones on Save.
            selected: if is_new { 2 } else { 0 },
        });
        self.screen = Screen::Editor;
    }

    fn editor_rows(&self) -> Vec<Row<ERow>> {
        let Some(editor) = &self.editor else {
            return Vec::new();
        };
        let h = &editor.draft;
        let text = |id, label: &str, value: &str, empty: &str| {
            Row::new(
                id,
                RowKind::Text,
                label,
                if value.is_empty() { empty } else { value },
            )
            .placeholder(value.is_empty())
        };
        let mut rows = vec![
            Row::new(
                ERow::Save,
                RowKind::Action,
                if editor.is_new { "Add this host" } else { "Save changes" },
                "",
            ),
            text(ERow::Name, "Name", &h.name, "Same as address"),
            text(ERow::Address, "Address", &h.address, "Required")
                .help("IP address or host name of the PC running the Kyber server."),
            text(ERow::Port, "Port", &h.port.to_string(), "")
                .help("Kyber uses one port, 8080 by default, for both TCP and UDP. Open it in the host's firewall."),
            Row::new(ERow::Auth, RowKind::Choice, "Sign-in", h.auth.label())
                .help("How the host checks who you are. It must match what was chosen when the Kyber server was installed."),
        ];
        match h.auth {
            AuthMode::Basic => {
                rows.push(
                    text(
                        ERow::Username,
                        "Username",
                        &h.username,
                        "Kyber default user",
                    )
                    .help("Leave empty to use the default account of a stock Kyber install."),
                );
                if !h.username.is_empty() {
                    rows.push(
                        text(ERow::Secret, "Password", &mask(&editor.secret), "Not set")
                            .help("Kept in the system credential store, not in the settings file. kyclient receives it on its command line."),
                    );
                }
            }
            AuthMode::Token => rows.push(
                text(ERow::Secret, "Token", &mask(&editor.secret), "Not set")
                    .help("A JWT accepted by the host. Kept in the system credential store."),
            ),
            AuthMode::Oidc => {}
        }
        rows.push(
            Row::new(ERow::Tls, RowKind::Choice, "Certificate check", h.tls.label()).help(
                match h.tls {
                    TlsMode::Tofu => "Like SSH: trust the certificate seen on the first connection, refuse if it later changes. Right for the self-signed certificate a default install creates.",
                    TlsMode::System => "Require a certificate signed by an authority this PC already trusts.",
                    TlsMode::KnownHosts => "Only connect to hosts whose certificate is already trusted; never add new ones.",
                    TlsMode::Fingerprint => "Only accept the exact certificate whose fingerprint is entered below.",
                    TlsMode::Skip => "Accept any certificate. Anyone on the network path can read or alter the stream; for testing only.",
                },
            ),
        );
        if h.tls == TlsMode::Fingerprint {
            rows.push(text(
                ERow::Fingerprint,
                "Fingerprint",
                &h.fingerprint,
                "sha256:AA:BB:...",
            ));
        }
        rows.push(
            text(ERow::Mac, "MAC address", &h.mac, "Learned automatically")
                .help("Used to wake a sleeping host (Wake-on-LAN). Filled in by itself the first time the host is seen online on this network."),
        );
        if !editor.is_new {
            rows.push(
                Row::new(
                    ERow::Pair,
                    RowKind::Action,
                    "Pair with a code",
                    if h.agent_id.is_empty() {
                        "Not paired"
                    } else {
                        "Paired"
                    },
                )
                .help("On the host: Host > Show a code. Entering it here fills in the sign-in and pins the host's certificate, so nothing can impersonate it."),
            );
            if net::parse_mac(&h.mac).is_some() {
                rows.push(Row::new(
                    ERow::Wake,
                    RowKind::Action,
                    "Wake this host now",
                    "",
                ));
            }
            if matches!(h.tls, TlsMode::Tofu | TlsMode::KnownHosts) {
                rows.push(
                    Row::new(ERow::ForgetCert, RowKind::Action, "Forget trusted certificate", "")
                        .help("Use after reinstalling the host, when the client refuses a changed certificate. The next connection trusts the new one."),
                );
            }
            rows.push(Row::new(
                ERow::Delete,
                RowKind::Danger,
                "Remove this host",
                "",
            ));
        }
        rows
    }

    fn act_on_editor(&mut self, row: ERow, act: RowAct) {
        let delta = if act == RowAct::Prev { -1 } else { 1 };
        let accept = act == RowAct::Accept;
        let Some(editor) = &mut self.editor else {
            return;
        };
        match row {
            ERow::Auth => {
                editor.draft.auth = editor.draft.auth.step(delta);
                // A password is not a token: do not carry the secret across.
                editor.secret.clear();
            }
            ERow::Tls => editor.draft.tls = editor.draft.tls.step(delta),
            _ if !accept => {}
            ERow::Save => self.save_editor(),
            ERow::Name => self.open_text_input(TextTarget::HostName),
            ERow::Address => self.open_text_input(TextTarget::HostAddress),
            ERow::Port => self.open_text_input(TextTarget::HostPort),
            ERow::Username => self.open_text_input(TextTarget::HostUsername),
            ERow::Secret => self.open_text_input(TextTarget::HostSecret),
            ERow::Fingerprint => self.open_text_input(TextTarget::HostFingerprint),
            ERow::Mac => self.open_text_input(TextTarget::HostMac),
            ERow::Pair => {
                let draft = editor.draft.clone();
                if draft.address.trim().is_empty() {
                    self.toast_error("Enter the host's address first.");
                    return;
                }
                self.begin_pairing(pairing::PairTarget {
                    name: draft.display_name().to_owned(),
                    address: draft.address.trim().to_owned(),
                    agent_port: if draft.agent_port > 0 {
                        draft.agent_port
                    } else {
                        crate::agent::DEFAULT_PORT
                    },
                    existing: Some(draft.id),
                });
            }
            ERow::Wake => match net::parse_mac(&editor.draft.mac) {
                Some(mac) => match net::wake_on_lan(mac) {
                    Ok(()) => self.toast_info("Wake-up packet sent."),
                    Err(error) => self.toast_error(format!("Wake-on-LAN failed: {error}")),
                },
                None => self.toast_error("The MAC address is not valid."),
            },
            ERow::ForgetCert => {
                let address = editor.draft.address.trim().to_owned();
                match &self.client_path {
                    Some(path) => match kyclient::forget_known_host(path, &address) {
                        Ok(()) => self.toast_info(format!("Asked the client to forget {address}.")),
                        Err(error) => self.toast_error(error),
                    },
                    None => self.toast_error("The Kyber client was not found."),
                }
            }
            ERow::Delete => {
                self.confirm = Some(Confirm {
                    title: format!("Remove {}?", editor.draft.display_name()),
                    body: "Its saved sign-in is deleted too.".into(),
                    delete_host: editor.draft.id,
                    yes_selected: false,
                });
            }
        }
    }

    fn save_editor(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        editor.draft.address = editor.draft.address.trim().to_owned();
        editor.draft.name = editor.draft.name.trim().to_owned();
        if editor.draft.address.is_empty() {
            self.toast_error("Enter the host's address first.");
            return;
        }
        let Some(editor) = self.editor.take() else {
            return;
        };
        let mut host = editor.draft;
        if editor.is_new {
            host.id = Host::new_id();
            self.config.hosts.push(host.clone());
        } else if let Some(saved) = self.config.host_mut(host.id) {
            // Keep what changed in the background while the editor was open.
            host.last_connected = saved.last_connected;
            *saved = host.clone();
        }
        if !self.read_only && (editor.secret != editor.original_secret || editor.is_new) {
            let stored = if Self::needs_secret(&host) {
                secrets::set(host.id, &editor.secret)
            } else {
                secrets::delete(host.id)
            };
            if let Err(error) = stored {
                self.toast_error(format!("The sign-in could not be stored: {error}"));
            }
        }
        self.host_selected = self
            .config
            .hosts
            .iter()
            .position(|h| h.id == host.id)
            .unwrap_or(0);
        self.scroll_to_host = true;
        self.hosts_changed();
        self.screen = Screen::Hosts;
    }

    fn delete_host(&mut self, host_id: u64) {
        self.config.hosts.retain(|h| h.id != host_id);
        if !self.read_only {
            Self::forget_agent_secret(host_id);
        }
        if !self.read_only {
            if let Err(error) = secrets::delete(host_id) {
                self.toast_error(format!("The saved sign-in could not be removed: {error}"));
            }
        }
        self.editor = None;
        self.screen = Screen::Hosts;
        self.hosts_changed();
    }

    fn editor_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav]) {
        if navs.contains(&Nav::Back) {
            let changed = self
                .editor
                .as_ref()
                .is_some_and(|e| e.draft != e.original || e.secret != e.original_secret);
            self.editor = None;
            self.screen = Screen::Hosts;
            if changed {
                self.toast_info("Changes discarded.");
            }
            return;
        }

        let rows = self.editor_rows();
        let mut selected = self.editor.as_ref().map(|e| e.selected).unwrap_or(0);
        let mut action = None;
        let title = match &self.editor {
            Some(editor) if editor.is_new => "Add a *host*.".to_owned(),
            Some(editor) => format!("Edit *{}*.", plain(editor.original.display_name())),
            None => String::new(),
        };
        centered(ui, 860.0, |ui| {
            ui.add_space(10.0);
            ui::caps(
                ui,
                "Host \u{b7} where to connect and how to sign in",
                ui::LIME,
            );
            ui::headline(ui, &title, 46.0);
            ui.add_space(4.0);
            let list_height = (ui.available_height() - 78.0).max(120.0);
            ui.allocate_ui(vec2(ui.available_width(), list_height), |ui| {
                action = ui::row_list(ui, &rows, &mut selected, navs);
            });
            help_line(ui, rows.get(selected).map(|r| r.help).unwrap_or_default());
        });
        if let Some(editor) = &mut self.editor {
            editor.selected = selected;
        }
        if let Some((row, act)) = action {
            self.act_on_editor(row, act);
        }
    }

    // ---------------------------------------------------------- text entry

    fn open_text_input(&mut self, target: TextTarget) {
        let draft = self.editor.as_ref().map(|e| &e.draft);
        let field = |get: fn(&Host) -> String| draft.map(get).unwrap_or_default();
        let (title, value, secret) = match target {
            TextTarget::HostName => ("Name", field(|h| h.name.clone()), false),
            TextTarget::HostAddress => ("Address", field(|h| h.address.clone()), false),
            TextTarget::HostPort => ("Port", field(|h| h.port.to_string()), false),
            TextTarget::HostUsername => ("Username", field(|h| h.username.clone()), false),
            TextTarget::HostSecret => (
                "Password or token",
                self.editor
                    .as_ref()
                    .map(|e| e.secret.clone())
                    .unwrap_or_default(),
                true,
            ),
            TextTarget::HostFingerprint => (
                "Certificate fingerprint",
                field(|h| h.fingerprint.clone()),
                false,
            ),
            TextTarget::HostMac => ("MAC address", field(|h| h.mac.clone()), false),
            TextTarget::ClientPath => (
                "Kyber client location",
                self.config.kyclient_path.clone(),
                false,
            ),
            TextTarget::HostingUsername | TextTarget::HostingPassword | TextTarget::HostingPort => {
                self.hosting_text_field(target)
            }
        };
        self.text_input = Some(TextInput {
            target,
            title: title.to_owned(),
            value,
            secret,
            key: (1, 0),
            shift: false,
        });
    }

    /// Stores the entered text. Returns false (keeping the dialog open) when
    /// the value is not acceptable.
    fn apply_text(&mut self, target: TextTarget, value: &str) -> bool {
        if matches!(
            target,
            TextTarget::HostingUsername | TextTarget::HostingPassword | TextTarget::HostingPort
        ) {
            return self.apply_hosting_text(target, value);
        }
        if target == TextTarget::ClientPath {
            self.config.kyclient_path = value.trim().trim_matches('"').to_owned();
            self.refresh_client();
            self.save();
            match &self.client_path {
                Some(path) => self.toast_info(format!("Using {}.", path.display())),
                None => self.toast_error("No Kyber client was found there."),
            }
            return true;
        }
        let Some(editor) = &mut self.editor else {
            return true;
        };
        let host = &mut editor.draft;
        match target {
            TextTarget::HostName => host.name = value.trim().to_owned(),
            TextTarget::HostAddress => {
                let value = value.trim();
                // Accept "host:port" pasted as one piece (not for IPv6 literals).
                match value.rsplit_once(':') {
                    Some((address, port))
                        if !address.contains(':') && port.parse::<u16>().is_ok_and(|p| p > 0) =>
                    {
                        host.address = address.to_owned();
                        host.port = port.parse().unwrap_or(DEFAULT_PORT);
                    }
                    _ => host.address = value.to_owned(),
                }
            }
            TextTarget::HostPort => match value.trim().parse::<u16>() {
                Ok(port) if port > 0 => host.port = port,
                _ => {
                    self.toast_error("The port must be a number from 1 to 65535.");
                    return false;
                }
            },
            TextTarget::HostUsername => host.username = value.trim().to_owned(),
            TextTarget::HostSecret => editor.secret = value.to_owned(),
            TextTarget::HostFingerprint => host.fingerprint = value.trim().to_owned(),
            TextTarget::HostMac => {
                if value.trim().is_empty() {
                    host.mac.clear();
                } else if let Some(mac) = net::parse_mac(value) {
                    host.mac = net::format_mac(mac);
                } else {
                    self.toast_error("A MAC address looks like AA:BB:CC:DD:EE:FF.");
                    return false;
                }
            }
            TextTarget::ClientPath
            | TextTarget::HostingUsername
            | TextTarget::HostingPassword
            | TextTarget::HostingPort => {}
        }
        true
    }

    fn text_modal(&mut self, ctx: &egui::Context, pad_navs: &[Nav], key_navs: &[Nav]) {
        let Some(mut input) = self.text_input.take() else {
            return;
        };
        let mut done = key_navs.contains(&Nav::Accept);
        let mut cancel = key_navs.contains(&Nav::Back);
        let mut press: Option<(usize, usize)> = None;

        let (mut row, mut column) = input.key;
        for nav in pad_navs {
            match nav {
                Nav::Left if column > 0 => column -= 1,
                Nav::Right if column + 1 < osk_columns(row) => column += 1,
                Nav::Up if row > 0 => {
                    column = remap_column(column, row, row - 1);
                    row -= 1;
                }
                Nav::Down if row + 1 < OSK_ROWS => {
                    column = remap_column(column, row, row + 1);
                    row += 1;
                }
                Nav::Accept => press = Some((row, column)),
                Nav::Edit => {
                    input.value.pop();
                }
                Nav::Settings => input.shift = !input.shift,
                Nav::Menu => done = true,
                Nav::Back => cancel = true,
                _ => {}
            }
        }
        input.key = (row, column);

        egui::Modal::new(egui::Id::new("text-input"))
            .frame(
                egui::Frame::new()
                    .fill(ui::PANEL)
                    .stroke(Stroke::new(1.0, ui::LINE))
                    .corner_radius(16.0)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(ui::caps_job(&input.title, 13.0, ui::LIME));
                ui.add_space(6.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut input.value)
                        .password(input.secret)
                        .font(ui::font(20.0))
                        .margin(Margin::symmetric(10, 8))
                        .desired_width(OSK_KEY.x * 10.0 + 6.0 * 9.0 - 20.0),
                );
                field.request_focus();
                ui.add_space(10.0);

                for r in 0..OSK_ROWS {
                    ui.horizontal(|ui| {
                        for c in 0..osk_columns(r) {
                            let label = osk_label(r, c, input.shift);
                            let width = if r == OSK_ROWS - 1 {
                                (OSK_KEY.x * 10.0 + 6.0 * 9.0 - 6.0 * 3.0) / 4.0
                            } else {
                                OSK_KEY.x
                            };
                            let (rect, response) =
                                ui.allocate_exact_size(vec2(width, OSK_KEY.y), Sense::click());
                            let is_selected = input.key == (r, c);
                            let active = r == OSK_ROWS - 1 && c == 0 && input.shift;
                            let fill = if is_selected {
                                ui::ACCENT
                            } else if active || response.hovered() {
                                ui::PANEL_HI
                            } else {
                                ui::BG
                            };
                            ui.painter().rect_filled(rect, 8.0, fill);
                            let ink = if is_selected {
                                Color32::from_rgb(0x0B, 0x0E, 0x14)
                            } else {
                                ui::TEXT
                            };
                            ui.painter().text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                label,
                                ui::font(17.0),
                                ink,
                            );
                            if response.clicked() {
                                press = Some((r, c));
                            }
                        }
                    });
                }
                ui.add_space(8.0);
                ui::hints(
                    ui,
                    &[
                        Hint::new("A", "", "Type"),
                        Hint::new("X", "", "Delete"),
                        Hint::new("Y", "", "Shift"),
                        Hint::new("Start", "Enter", "Done"),
                        Hint::new("B", "Esc", "Cancel"),
                    ],
                    self.pad.connected() > 0,
                );
            });

        if let Some((r, c)) = press {
            if r == OSK_ROWS - 1 {
                match c {
                    0 => input.shift = !input.shift,
                    1 => input.value.push(' '),
                    2 => {
                        input.value.pop();
                    }
                    _ => done = true,
                }
            } else {
                input.value.push_str(&osk_label(r, c, input.shift));
            }
        }

        if cancel {
            return;
        }
        if done && self.apply_text(input.target, &input.value) {
            return;
        }
        self.text_input = Some(input);
    }

    fn confirm_modal(&mut self, ctx: &egui::Context, navs: &[Nav]) {
        let Some(mut confirm) = self.confirm.take() else {
            return;
        };
        let mut decision = None;
        for nav in navs {
            match nav {
                Nav::Left => confirm.yes_selected = true,
                Nav::Right => confirm.yes_selected = false,
                Nav::Accept => decision = Some(confirm.yes_selected),
                Nav::Back => decision = Some(false),
                _ => {}
            }
        }
        egui::Modal::new(egui::Id::new("confirm"))
            .frame(
                egui::Frame::new()
                    .fill(ui::PANEL)
                    .stroke(Stroke::new(1.0, ui::LINE))
                    .corner_radius(16.0)
                    .inner_margin(Margin::same(26)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.label(ui::headline_job(&confirm.title, 28.0, ui::TEXT, ui::LIME));
                ui.label(RichText::new(&confirm.body).color(ui::DIM));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui::button(ui, "Remove", confirm.yes_selected, true).clicked() {
                        decision = Some(true);
                    }
                    if ui::button(ui, "Keep", !confirm.yes_selected, false).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => self.delete_host(confirm.delete_host),
            Some(false) => {}
            None => self.confirm = Some(confirm),
        }
    }

    // ------------------------------------------------------- stream screen

    fn stream_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav], ctx: &egui::Context) {
        let Some(view) = &self.stream else {
            self.screen = Screen::Hosts;
            return;
        };
        let host_id = view.host_id;
        let mut disconnect = false;
        let mut retry = false;
        let mut back = false;

        let running = self.session.is_some();
        for nav in navs {
            match nav {
                Nav::Back if running => disconnect = true,
                Nav::Back => back = true,
                Nav::Accept if !running => retry = true,
                _ => {}
            }
        }

        let log = match (&self.session, &view.ended) {
            (Some(session), _) => session.log_tail(8),
            (None, Some(ended)) => ended.log.clone(),
            (None, None) => Vec::new(),
        };
        let phase = self.session.as_ref().map(Session::phase);
        let show_leave_button = !self.config.hide_quit_button;

        centered(ui, 900.0, |ui| {
            ui.add_space(ui.available_height() * 0.07);
            if let Some(ended) = &view.ended {
                let (headline, detail) = match ended.reason {
                    Some(EndReason::WrongSignIn) => (
                        format!("*{}* said no.", plain(&view.host_name)),
                        "Edit the host (X or E on its tile) and enter the name and password shown on the hosting PC's Host screen.".to_owned(),
                    ),
                    _ => {
                        let code = match ended.code {
                            Some(code) => format!("exit code {code}"),
                            None => "no exit code".to_owned(),
                        };
                        (
                            format!("Lost the light from *{}*.", plain(&view.host_name)),
                            format!(
                                "kyclient ended after {} ({code}).",
                                format_duration(ended.ran_for)
                            ),
                        )
                    }
                };
                ui::caps(ui, "Stream ended", ui::ERR);
                ui::headline(ui, &headline, 46.0);
                ui.label(RichText::new(detail).color(ui::DIM).size(16.0));
            } else {
                let name = plain(&view.host_name);
                let (label, headline, colour) = match phase {
                    Some(Phase::Streaming) => ("Live", format!("Streaming *{name}*."), ui::OK),
                    Some(Phase::Reconnecting) => (
                        "Connection lost",
                        format!("Finding *{name}* again..."),
                        ui::WARN,
                    ),
                    _ => ("Connecting", format!("Reaching *{name}*..."), ui::LAVENDER),
                };
                ui::caps(ui, label, colour);
                ui::headline(ui, &headline, 46.0);
                if let Some(stats) = self.session.as_ref().map(Session::stats) {
                    if stats.frames > 0 {
                        ui.label(
                            RichText::new(stats.overlay_text())
                                .font(ui::mono(14.0))
                                .color(ui::LIME),
                        );
                    }
                }
                let elapsed = self
                    .session
                    .as_ref()
                    .map(|s| s.started.elapsed())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format_duration(elapsed))
                        .size(20.0)
                        .color(ui::DIM),
                );
                ui.add_space(8.0);

                ui.label(
                    RichText::new("To leave the stream")
                        .size(16.0)
                        .color(ui::TEXT),
                );
                let mut ways = vec![("Ctrl + Alt + Shift + Q", "from the keyboard, any time")];
                if cfg!(windows) {
                    ways.push(("Start + Back + LB + RB", "held for a second on a gamepad"));
                }
                if show_leave_button {
                    ways.push(("Leave button", "top-right corner of the screen"));
                }
                for (keys, what) in ways {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(keys).color(ui::ACCENT3).size(15.0));
                        ui.label(RichText::new(what).color(ui::DIM).size(15.0));
                    });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "In the stream: Ctrl+Alt+F fullscreen \u{b7} Ctrl+M mute \u{b7} Ctrl+Up/Down volume \u{b7} Ctrl+B+Up/Down bitrate \u{b7} Ctrl+Alt+R restart",
                    )
                    .color(ui::DIM)
                    .size(14.0),
                );
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if running {
                    disconnect |= ui::button(ui, "Leave stream", true, true).clicked();
                } else {
                    retry |= ui::button(ui, "Try again", true, false).clicked();
                    back |= ui::button(ui, "Back to hosts", false, false).clicked();
                }
            });

            ui.add_space(18.0);
            ui.label(
                RichText::new(&view.command_line)
                    .font(ui::mono(12.5))
                    .color(ui::DIM),
            );
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(ui::PANEL.gamma_multiply(0.8))
                .corner_radius(14.0)
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if log.is_empty() {
                        ui.label(
                            RichText::new(if running {
                                "Waiting for the client to report in..."
                            } else {
                                "The client wrote nothing to its log."
                            })
                            .color(ui::DIM)
                            .size(14.0),
                        );
                    }
                    for line in &log {
                        let colour = if line.starts_with("ERROR") {
                            ui::ERR
                        } else if line.starts_with("WARN") {
                            ui::WARN
                        } else {
                            ui::DIM
                        };
                        let font = ui::mono(12.0);
                        let line = ui::elide(ui.painter(), line, &font, ui.available_width());
                        ui.label(RichText::new(line).font(font).color(colour));
                    }
                });
        });

        if disconnect {
            if let Some(session) = &self.session {
                session.stop();
            }
        }
        if back {
            self.stream = None;
            self.screen = Screen::Hosts;
        }
        if retry {
            self.stream = None;
            self.screen = Screen::Hosts;
            self.connect(host_id, ctx);
        }
    }

    /// Seconds since the current screen was opened.
    fn screen_age(&mut self, ctx: &egui::Context) -> f32 {
        let now = ctx.input(|input| input.time);
        if self.screen != self.last_screen {
            self.last_screen = self.screen;
            self.screen_since = now;
        }
        (now - self.screen_since) as f32
    }

    // --------------------------------------------------------------- chrome

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .exact_height(64.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(Margin::symmetric(28, 0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let section = match self.screen {
                        Screen::Home => "",
                        Screen::Host => "Host",
                        Screen::Setup => "Setup",
                        Screen::Hosts => "Join",
                        Screen::Settings => "Settings",
                        Screen::Editor => "Join \u{b7} Edit host",
                        Screen::Stream => "Stream",
                    };
                    ui::caps(ui, section, ui::DIM);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match (
                            &self.client_path,
                            self.client_version.lock().unwrap().as_deref(),
                        ) {
                            (Some(_), Some(version)) => {
                                ui::chip(ui, &format!("Kyber {version}"), ui::DIM)
                            }
                            (Some(_), None) => ui::chip(ui, "Kyber ready", ui::DIM),
                            (None, _) => ui::chip(ui, "Kyber not installed", ui::WARN),
                        }
                        match (self.pad.connected(), &self.pad.active_name) {
                            (0, _) => ui::chip(ui, "No controller", ui::DIM),
                            (_, Some(name)) => ui::chip(ui, name, ui::ACCENT3),
                            (1, None) => ui::chip(ui, "Controller ready", ui::ACCENT3),
                            (n, None) => ui::chip(ui, &format!("{n} controllers"), ui::ACCENT3),
                        }
                        if let Some(server) = &self.hosting.server {
                            let label = match server.clients() {
                                0 => "Hosting".to_owned(),
                                1 => "Hosting \u{b7} 1 player".to_owned(),
                                n => format!("Hosting \u{b7} {n} players"),
                            };
                            ui::chip_live(ui, &label, ui::OK, true);
                        }
                    });
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let on_host = self.host_selected < self.config.hosts.len();
        let hints: Vec<Hint> = match self.screen {
            Screen::Home => hosting::home_hints(),
            Screen::Host => hosting::host_hints(),
            Screen::Setup => self.setup_hints(),
            Screen::Hosts if on_host => vec![
                Hint::new("A", "Enter", "Connect"),
                Hint::new("X", "E", "Edit"),
                Hint::new("Y", "S", "Settings"),
                Hint::new("Start", "F11", "Fullscreen"),
            ],
            Screen::Hosts => vec![
                Hint::new("A", "Enter", "Select"),
                Hint::new("Y", "S", "Settings"),
                Hint::new("Start", "F11", "Fullscreen"),
            ],
            Screen::Settings => vec![
                Hint::new("A", "Enter", "Change"),
                Hint::new("D-pad", "Arrows", "Adjust"),
                Hint::new("LB / RB", "PgUp / PgDn", "Section"),
                Hint::new("B", "Esc", "Back"),
            ],
            Screen::Editor => vec![
                Hint::new("A", "Enter", "Select"),
                Hint::new("D-pad", "Arrows", "Adjust"),
                Hint::new("B", "Esc", "Cancel"),
            ],
            Screen::Stream if self.session.is_some() => vec![Hint::new("", "Esc", "Disconnect")],
            Screen::Stream => vec![
                Hint::new("A", "Enter", "Try again"),
                Hint::new("B", "Esc", "Back"),
            ],
        };
        // While a stream runs the pad belongs to the stream, so show keys.
        let use_pad = self.pad.connected() > 0 && self.session.is_none();
        egui::TopBottomPanel::bottom("footer")
            .exact_height(54.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(Margin::symmetric(28, 0)))
            .show(ctx, |ui| ui::hints(ui, &hints, use_pad));
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else {
            return;
        };
        if Instant::now() >= toast.until {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("toast"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -66.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ui::PANEL_HI)
                    .stroke(Stroke::new(1.5, toast.colour))
                    .corner_radius(20.0)
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.set_max_width(720.0);
                        ui.label(
                            RichText::new(&toast.text)
                                .font(ui::bold(15.0))
                                .color(ui::TEXT),
                        );
                    });
            });
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else {
            return;
        };
        if !shot.requested && Instant::now() >= shot.at {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [width, height] = image.size;
            if let Err(error) = image::save_buffer(
                &shot.path,
                image.as_raw(),
                width as u32,
                height as u32,
                image::ColorType::Rgba8,
            ) {
                eprintln!("screenshot failed: {error}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background(ctx);
        if crate::hotkey::stats_pressed() {
            self.stats_visible = !self.stats_visible;
        }
        if self.lab.is_some() {
            self.lab_frame(ctx);
            self.handle_screenshot(ctx);
            return;
        }

        let streaming = self.session.is_some();
        let focused = ctx.input(|input| input.focused);
        let text_open = self.text_input.is_some();
        let confirm_open = self.confirm.is_some();
        let entry_open = self.pairing.entry.is_some();

        // While a stream runs the controller belongs to it, not to us.
        let pad_navs = self.pad.poll(focused && !streaming);
        let key_navs = pad::keyboard(ctx, text_open || entry_open);
        let all_navs: Vec<Nav> = pad_navs.iter().chain(key_navs.iter()).copied().collect();
        let screen_navs: &[Nav] = if text_open || confirm_open || entry_open {
            &[]
        } else {
            &all_navs
        };

        let toggle_fullscreen = ctx.input(|input| input.key_pressed(egui::Key::F11))
            || (matches!(self.screen, Screen::Hosts | Screen::Home)
                && screen_navs.contains(&Nav::Menu));
        if toggle_fullscreen {
            self.set_fullscreen(!self.config.launcher_fullscreen, ctx);
        }

        ui::set_motion(ctx, focused && !streaming);
        // Full light on Home; dimmer where there is text to read.
        let light = if self.screen == Screen::Home {
            1.0
        } else {
            0.55
        };
        ui::background(ctx, light);
        let age = self.screen_age(ctx);
        self.top_bar(ctx);
        self.footer(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ctx, |ui| {
                ui::enter(ui, age);
                match self.screen {
                    Screen::Home => self.home_screen(ui, screen_navs),
                    Screen::Host => self.host_screen(ui, screen_navs, ctx),
                    Screen::Setup => self.setup_screen(ui, screen_navs, ctx),
                    Screen::Hosts => self.hosts_screen(ui, screen_navs, ctx),
                    Screen::Settings => self.settings_screen(ui, screen_navs, ctx),
                    Screen::Editor => self.editor_screen(ui, screen_navs),
                    Screen::Stream => self.stream_screen(ui, screen_navs, ctx),
                }
            });

        // Dialogs only react to input from the frame after they opened, so
        // the key press that opened one is not also applied to it.
        if text_open {
            self.text_modal(ctx, &pad_navs, &key_navs);
        } else if self.text_input.is_some() {
            self.text_modal(ctx, &[], &[]);
        } else if confirm_open {
            self.confirm_modal(ctx, &all_navs);
        } else if self.confirm.is_some() {
            self.confirm_modal(ctx, &[]);
        }

        if entry_open {
            self.code_entry_modal(ctx, &pad_navs, &key_navs, true);
        } else if self.pairing.entry.is_some() {
            self.code_entry_modal(ctx, &[], &[], false);
        }
        self.draw_toast(ctx);
        self.fx.paint(ctx);
        self.stream_overlay(ctx);
        self.handle_screenshot(ctx);

        // Gamepad input does not wake egui, so poll: fast while the launcher
        // is in use, slowly while a stream is running or we are in the
        // background, to stay out of the stream's way.
        let delay = if streaming {
            500
        } else if !focused {
            400
        } else if self.pad.connected() > 0 {
            16
        } else {
            // Keeps the backdrop and the cards moving.
            33
        };
        ctx.request_repaint_after(Duration::from_millis(delay));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Hosting lasts only while the launcher is open.
        self.stop_hosting_and_wait();
        // Closing the launcher ends the stream it started.
        if let Some(session) = &self.session {
            session.stop();
            let deadline = Instant::now() + Duration::from_secs(2);
            while session.state() == SessionState::Running && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

// ------------------------------------------------------------- free helpers

/// Lays `add` out in a horizontally centred column of at most `max_width`.
fn centered(ui: &mut egui::Ui, max_width: f32, add: impl FnOnce(&mut egui::Ui)) {
    let available = ui.available_rect_before_wrap();
    let width = (available.width() - 48.0).min(max_width).max(200.0);
    let rect = Rect::from_min_size(
        Pos2::new(available.center().x - width * 0.5, available.top()),
        vec2(width, available.height()),
    );
    // The scope advances the parent past what the column used.
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add);
}

fn help_line(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).color(ui::DIM).size(14.5));
}

/// A name safe to put in a headline (asterisks mark the accent word).
fn plain(text: &str) -> String {
    text.replace('*', "")
}

fn mask(secret: &str) -> String {
    "\u{2022}".repeat(secret.chars().count().min(12))
}

fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn open_url(url: &str) -> Result<(), String> {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command.spawn().map(|_| ()).map_err(|e| e.to_string())
}

// On-screen keyboard: four character rows and one row of actions.
const OSK_ROWS: usize = 5;
const OSK_KEY: egui::Vec2 = vec2(52.0, 46.0);
const OSK_PLAIN: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl-", "zxcvbnm.:_"];
const OSK_SHIFT: [&str; 4] = ["!@#$%^&*()", "QWERTYUIOP", "ASDFGHJKL+", "ZXCVBNM,/\\"];
const OSK_ACTIONS: [&str; 4] = ["Shift", "Space", "Delete", "Done"];

fn osk_columns(row: usize) -> usize {
    if row == OSK_ROWS - 1 {
        OSK_ACTIONS.len()
    } else {
        10
    }
}

fn osk_label(row: usize, column: usize, shift: bool) -> String {
    if row == OSK_ROWS - 1 {
        return OSK_ACTIONS[column].to_owned();
    }
    let layout = if shift { OSK_SHIFT } else { OSK_PLAIN };
    layout[row]
        .chars()
        .nth(column)
        .map(String::from)
        .unwrap_or_default()
}

/// Keeps the horizontal position when moving between the 10-key rows and the
/// 4-button action row.
fn remap_column(column: usize, from_row: usize, to_row: usize) -> usize {
    let (from, to) = (osk_columns(from_row), osk_columns(to_row));
    if from == to {
        column
    } else {
        ((column * 2 + 1) * to / (from * 2)).min(to - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_rows_all_have_ten_keys() {
        for row in OSK_PLAIN.iter().chain(OSK_SHIFT.iter()) {
            assert_eq!(row.chars().count(), 10, "row {row}");
        }
    }

    #[test]
    fn moving_between_key_rows_and_action_row_keeps_position() {
        // 10 keys over 4 actions: left keys map to Shift, right keys to Done.
        assert_eq!(remap_column(0, 3, 4), 0);
        assert_eq!(remap_column(4, 3, 4), 1);
        assert_eq!(remap_column(5, 3, 4), 2);
        assert_eq!(remap_column(9, 3, 4), 3);
        // And back up, landing in the middle of the button's span.
        assert_eq!(remap_column(0, 4, 3), 1);
        assert_eq!(remap_column(3, 4, 3), 8);
        assert_eq!(remap_column(6, 1, 2), 6);
    }

    #[test]
    fn durations_are_readable() {
        assert_eq!(format_duration(Duration::from_secs(5)), "0:05");
        assert_eq!(format_duration(Duration::from_secs(725)), "12:05");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1:02:05");
    }
}

/// The short line under a setting's name.
fn srow_note(row: SRow) -> &'static str {
    match row {
        SRow::Preset => "A starting point for everything below",
        SRow::Codec => "H.264 decodes fastest almost everywhere",
        SRow::Bitrate => "Auto lets the host decide",
        SRow::Yuv444 => "Sharper text, more bandwidth",
        SRow::SoftwareDecode => "Only if hardware decoding misbehaves",
        SRow::VideoBuffer => "None is fastest; add some for jittery Wi-Fi",
        SRow::Audio => "Stream the host's sound",
        SRow::AudioBuffer => "Bigger hides crackling, delays sound",
        SRow::DisplayIndex => "Which of the host's screens",
        SRow::DisplayCount => "Several screens, one window each",
        SRow::UseHostCodec => "Uses what the host measured fastest",
        SRow::Protocol => "Kymux over QUIC unless UDP is blocked",
        SRow::VideoTransport => "Unreliable never waits for lost packets",
        SRow::AudioTransport => "Same choices, for the sound",
        SRow::IntraRefresh => "Smooths bitrate spikes on NVIDIA hosts",
        SRow::AutoReconnect => "Rejoin by itself after a drop",
        SRow::Inputs => "Off for a view-only stream",
        SRow::KeyboardGrab => "Alt+Tab and the Windows key go to the host",
        SRow::Xbox360 => "For games that only know XInput",
        SRow::Clipboard => "Copy here, paste there",
        SRow::StartFullscreen => "Presses Ctrl+Alt+F for you",
        SRow::LauncherFullscreen => "Also F11 or Start",
        SRow::AutoConnect => "Straight into your last host",
        SRow::QuitButton => "One click to leave a stream",
        SRow::LogLevel => "Raise it to diagnose a failed connection",
        SRow::ClientPath => "Found automatically when left empty",
        SRow::GetKyber => "Official installer, client and host",
        _ => "",
    }
}
