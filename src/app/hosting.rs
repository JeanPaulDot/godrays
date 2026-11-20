//! The Home screen (Join or Host), the Host screen and the one-click Kyber
//! setup.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use eframe::egui::{self, vec2, Margin, Pos2, RichText, Sense, Stroke};

use super::{centered, format_duration, mask, open_url, App, Screen, TextTarget};
use crate::config::DEFAULT_PORT;
use crate::display::{self, Mode};
use crate::host::{self, Server, ServerState};
use crate::kyber::{self, Setup, SetupStage};
use crate::net;
use crate::pad::Nav;
use crate::secrets;
use crate::ui::{self, HeroIcon, Hint, Row, RowAct, RowKind};

const VIGEM_URL: &str = "https://github.com/nefarius/ViGEmBus/releases";
const MIN_PASSWORD: usize = 8;

pub(super) struct Failure {
    message: String,
    log: Vec<String>,
}

pub(super) struct HostingState {
    demo: bool,
    /// Folder of the installed Kyber server, when there is one.
    pub(super) install_dir: Option<PathBuf>,
    pub(super) server: Option<Server>,
    pub(super) setup: Option<Setup>,
    /// Screen to open once setup finishes or is skipped.
    after_setup: Screen,
    setup_declined: bool,
    /// Screen the settings were opened from, to return to.
    pub(super) settings_from: Screen,
    /// `--host`: start hosting as soon as the launcher is up.
    pub(super) start_on_launch: bool,
    home_selected: usize,
    row_selected: usize,
    /// The password others type to join this PC; loaded on first use.
    password: String,
    password_loaded: bool,
    show_password: bool,
    vigem: bool,
    addresses: Vec<Ipv4Addr>,
    failure: Option<Failure>,
    /// Resolutions the primary display offers, and what it runs at now.
    modes: Vec<Mode>,
    native: Option<Mode>,
    /// The display was switched for hosting and must be put back.
    display_switched: bool,
    /// The server was ready on the previous frame (to celebrate once).
    was_ready: bool,
}

impl HostingState {
    pub(super) fn is_demo(&self) -> bool {
        self.demo
    }

    pub(super) fn new(demo: bool) -> Self {
        Self {
            demo,
            install_dir: None,
            server: None,
            setup: None,
            after_setup: Screen::Home,
            setup_declined: false,
            settings_from: Screen::Hosts,
            start_on_launch: false,
            home_selected: 0,
            row_selected: 0,
            password: String::new(),
            password_loaded: false,
            show_password: false,
            vigem: false,
            addresses: Vec::new(),
            failure: None,
            modes: Vec::new(),
            native: None,
            display_switched: false,
            was_ready: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HRow {
    Username,
    Password,
    ShowPassword,
    Resolution,
    Encoder,
    FindEncoder,
    Port,
    Boost,
    Gamepads,
    Paired,
    Unpair,
}

impl App {
    pub(super) fn hosting_password(&mut self) -> String {
        self.ensure_host_credentials();
        self.hosting.password.clone()
    }

    pub(super) fn refresh_install(&mut self) {
        self.hosting.install_dir = kyber::install_dir(self.client_path.as_deref());
    }

    pub(super) fn open_settings(&mut self, from: Screen) {
        self.hosting.settings_from = from;
        self.screen = Screen::Settings;
        if self.hosting.install_dir.is_some() || self.hosting.demo {
            self.load_host_details();
        }
    }

    /// Called once at startup: offer (or, with `--setup`, begin) the Kyber
    /// install when it is missing.
    pub(super) fn first_run(&mut self, start_now: bool, ctx: &egui::Context) {
        let missing = self.client_path.is_none() || self.hosting.install_dir.is_none();
        if self.hosting.demo || !cfg!(windows) || !missing {
            return;
        }
        self.open_setup(Screen::Home);
        if start_now {
            self.start_setup(ctx);
        }
    }

    // ---------------------------------------------------------------- home

    fn open_join(&mut self) {
        if self.client_path.is_none() && cfg!(windows) && !self.hosting.setup_declined {
            self.open_setup(Screen::Hosts);
        } else {
            self.screen = Screen::Hosts;
        }
    }

    pub(super) fn open_host(&mut self) {
        if self.hosting.install_dir.is_none() && !self.hosting.demo {
            self.open_setup(Screen::Host);
            return;
        }
        self.load_host_details();
        self.screen = Screen::Host;
    }

    /// What the Host screen and the hosting settings show.
    pub(super) fn load_host_details(&mut self) {
        self.ensure_host_credentials();
        self.hosting.vigem = !self.hosting.demo && kyber::vigem_installed();
        self.hosting.addresses = if self.hosting.demo {
            vec![Ipv4Addr::new(192, 168, 1, 5)]
        } else {
            net::local_ipv4s()
        };
        // While the display is switched for hosting, "native" stays what it
        // was before.
        if !self.hosting.display_switched {
            self.hosting.native = display::current();
            self.hosting.modes = display::pick_modes(&display::all_modes());
        }
    }

    pub(super) fn home_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav]) {
        let mut activate = None;
        for nav in navs {
            match nav {
                Nav::Left => self.hosting.home_selected = 0,
                Nav::Right => self.hosting.home_selected = 1,
                Nav::Accept => activate = Some(self.hosting.home_selected),
                Nav::Settings => self.open_settings(Screen::Home),
                _ => {}
            }
        }

        let saved = self.config.hosts.len();
        let join_status = match (self.client_path.is_some(), saved) {
            (false, _) => ("Kyber is not installed yet".to_owned(), ui::WARN),
            (true, 0) => ("No saved hosts yet".to_owned(), ui::DIM),
            (true, 1) => ("1 saved host".to_owned(), ui::DIM),
            (true, n) => (format!("{n} saved hosts"), ui::DIM),
        };
        let server_state = self.hosting.server.as_ref().map(Server::state);
        let players = self.hosting.server.as_ref().map_or(0, Server::clients);
        let host_status = match &server_state {
            Some(ServerState::Ready) if players == 1 => ("1 player connected".to_owned(), ui::OK),
            Some(ServerState::Ready) if players > 1 => {
                (format!("{players} players connected"), ui::OK)
            }
            Some(ServerState::Ready) => ("Hosting now".to_owned(), ui::OK),
            Some(_) => ("Starting...".to_owned(), ui::WARN),
            None if self.hosting.install_dir.is_none() && !self.hosting.demo => {
                ("Kyber is not installed yet".to_owned(), ui::WARN)
            }
            None => ("Not hosting".to_owned(), ui::DIM),
        };
        let live = server_state == Some(ServerState::Ready);

        let width = ui::HERO.x * 2.0 + ui::HERO_GAP;
        let left = ((ui.available_width() - width) * 0.5).max(0.0);
        ui.add_space(((ui.available_height() - ui::HERO.y - 150.0) * 0.4).max(8.0));
        ui.vertical_centered(|ui| {
            ui::caps(ui, "Game streaming \u{b7} built on Kyber", ui::LIME);
            ui::headline(ui, "Let there be *light*.", 62.0);
        });
        ui.add_space(30.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = ui::HERO_GAP;
            ui.add_space(left);
            let cards = [
                (
                    HeroIcon::Join,
                    "Join",
                    "Play on another PC",
                    &join_status,
                    false,
                ),
                (HeroIcon::Host, "Host", "Share this PC", &host_status, live),
            ];
            for (index, (icon, title, subtitle, status, live)) in cards.into_iter().enumerate() {
                let selected = self.hosting.home_selected == index;
                let response = ui::hero_card(
                    ui, icon, title, subtitle, &status.0, status.1, selected, live,
                );
                if response.clicked() {
                    self.hosting.home_selected = index;
                    activate = Some(index);
                }
            }
        });

        match activate {
            Some(0) => self.open_join(),
            Some(_) => self.open_host(),
            None => {}
        }
    }

    // ---------------------------------------------------------------- host

    /// Loads the hosting password, creating one the first time.
    fn ensure_host_credentials(&mut self) {
        if self.hosting.password_loaded {
            return;
        }
        if self.hosting.demo {
            self.hosting.password = "k7mq-x2hd-9fwn".to_owned();
            self.hosting.password_loaded = true;
            return;
        }
        match secrets::get_named(secrets::HOSTING) {
            Ok(Some(password)) => {
                self.hosting.password = password;
                self.hosting.password_loaded = true;
            }
            Ok(None) => {
                let password = kyber::generate_password();
                match secrets::set_named(secrets::HOSTING, &password) {
                    Ok(()) => {
                        self.hosting.password = password;
                        self.hosting.password_loaded = true;
                    }
                    Err(error) => self
                        .toast_error(format!("The hosting password could not be stored: {error}")),
                }
            }
            Err(error) => {
                self.toast_error(format!("The hosting password could not be read: {error}"))
            }
        }
        if self.config.hosting.username.trim().is_empty() {
            self.config.hosting.username = "kyber".to_owned();
        }
    }

    /// The display mode chosen for hosting, if it is not "leave as it is".
    fn hosting_mode(&self) -> Option<Mode> {
        let hosting = &self.config.hosting;
        self.hosting
            .modes
            .iter()
            .copied()
            .find(|m| m.width == hosting.width && m.height == hosting.height)
    }

    fn restore_display(&mut self) {
        if std::mem::take(&mut self.hosting.display_switched) {
            display::restore();
        }
    }

    pub(super) fn start_hosting(&mut self, ctx: &egui::Context) {
        if self.hosting.server.is_some() {
            return;
        }
        if self.hosting.demo {
            self.toast_info("Hosting is switched off in demo mode.");
            return;
        }
        if self.lab.is_some() {
            self.toast_info("Wait for the latency test to finish.");
            return;
        }
        let Some(install_dir) = self.hosting.install_dir.clone() else {
            self.open_setup(Screen::Host);
            return;
        };
        self.ensure_host_credentials();

        // The server reads the display size when it starts, so the switch
        // comes first.
        if let Some(mode) = self.hosting_mode() {
            if Some(mode) != self.hosting.native {
                match display::set(mode) {
                    Ok(()) => self.hosting.display_switched = true,
                    Err(error) => {
                        self.toast_error(format!("{error} Hosting at the current resolution."))
                    }
                }
            }
        }

        let repaint = ctx.clone();
        let port = self.config.hosting.port;
        let boost = self.config.hosting.boost;
        let started = host::prepare(&install_dir, &self.config.hosting, &self.hosting.password)
            .and_then(|files| {
                Server::start(&install_dir, &files, port, boost, move || {
                    repaint.request_repaint()
                })
            });
        match started {
            Ok(server) => {
                self.hosting.server = Some(server);
                self.hosting.failure = None;
                self.hosting.was_ready = false;
            }
            Err(message) => {
                self.restore_display();
                self.hosting.failure = Some(Failure {
                    message,
                    log: Vec::new(),
                })
            }
        }
    }

    /// Ends hosting (closing the launcher). Connected clients are
    /// disconnected by the server itself before it exits.
    pub(super) fn stop_hosting_and_wait(&mut self) {
        if let Some(server) = self.hosting.server.take() {
            server.stop_and_wait();
        }
        self.restore_display();
    }

    pub(super) fn poll_hosting(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.hosting.start_on_launch) {
            self.open_host();
            if self.screen == Screen::Host {
                self.start_hosting(ctx);
            }
        }

        let state = self.hosting.server.as_ref().map(Server::state);
        let ready = state == Some(ServerState::Ready);
        if ready && !self.hosting.was_ready {
            let screen = ctx.screen_rect();
            self.fx
                .burst(ctx, Pos2::new(screen.center().x, screen.top() + 150.0));
        }
        self.hosting.was_ready = ready;

        if let Some(ServerState::Exited(code)) = state {
            let (requested, log) = self
                .hosting
                .server
                .as_ref()
                .map(|server| (server.stop_requested(), server.log_tail(12)))
                .unwrap_or_default();
            self.hosting.server = None;
            self.restore_display();
            if requested {
                self.toast_info("Hosting stopped. Everyone connected was disconnected.");
            } else {
                let code = code.map_or("no exit code".to_owned(), |c| format!("exit code {c}"));
                self.hosting.failure = Some(Failure {
                    message: format!("The Kyber server stopped by itself ({code})."),
                    log,
                });
                self.toast_error("Hosting stopped unexpectedly.");
            }
        }

        let stage = self.hosting.setup.as_ref().map(Setup::stage);
        if stage == Some(SetupStage::Done) {
            self.hosting.setup = None;
            self.refresh_client();
            self.toast_info(format!("Kyber {} is installed.", kyber::VERSION));
            let screen = ctx.screen_rect();
            self.fx.burst(ctx, screen.center());
            match self.hosting.after_setup {
                Screen::Host => self.open_host(),
                other => self.screen = other,
            }
        }
    }

    fn resolution_label(&self) -> String {
        match self.hosting_mode() {
            Some(mode) => mode.label(),
            None => match self.hosting.native {
                Some(native) => {
                    format!("Keep as it is ({} \u{d7} {})", native.width, native.height)
                }
                None => "Keep as it is".to_owned(),
            },
        }
    }

    /// Settings > Hosting: what a host rarely needs to touch.
    pub(super) fn host_settings_rows(&self) -> Vec<Row<HRow>> {
        let hosting = &self.config.hosting;
        let test_status = match (
            &self.config.hosting.best_codec,
            self.config.hosting.best_latency_ms,
        ) {
            (Some(codec), Some(ms)) => format!(
                "{} + {}: {ms:.1} ms",
                self.config.hosting.encoder.label(),
                codec.label()
            ),
            _ => "Not measured yet".to_owned(),
        };
        vec![
            Row::new(HRow::Username, RowKind::Text, "Name to sign in with", hosting.username.as_str())
                .note("For devices without Godrays, such as a browser")
                .help("Paired devices get this sign-in automatically. You only type it on devices that do not run Godrays: Kyber's web client (https://this-pc:8080/webclient/) or the plain Kyber client."),
            Row::new(
                HRow::Password,
                RowKind::Text,
                "Password",
                if self.hosting.show_password {
                    self.hosting.password.clone()
                } else {
                    mask(&self.hosting.password)
                },
            )
            .note("Changing it un-pairs every device")
            .help("Created for you at random. Kept in the Windows credential store; the server only gets its hash. A new password shuts out everyone who had the old one, paired devices included."),
            Row::new(
                HRow::ShowPassword,
                RowKind::Toggle(self.hosting.show_password),
                "Show password",
                "",
            )
            .note("To read it out to someone"),
            Row::new(HRow::Resolution, RowKind::Choice, "Resolution while hosting", self.resolution_label())
                .note("Switches the screen while you share it")
                .help("Kyber streams this PC's main display exactly as it is. The display switches when hosting starts and goes back when it stops; each entry uses the highest refresh rate the display offers."),
            Row::new(HRow::Encoder, RowKind::Choice, "Video encoder", hosting.encoder.label())
                .note("Your graphics card's encoder is fastest")
                .help("Hardware encoders are far faster than software, but must work with your drivers. If players connect and see no picture, change this, or let the test below pick."),
            Row::new(HRow::FindEncoder, RowKind::Action, "Find the fastest encoder", test_status)
                .note("Measures each one on this PC, about a minute")
                .help("Streams to this same PC with each encoder and codec your graphics card offers, measures Kyber's capture-to-display time for each, and selects the fastest one that really shows a picture."),
            Row::new(HRow::Boost, RowKind::Toggle(hosting.boost), "Stream ahead of games", "")
                .note("Top GPU priority for capture and encode")
                .help("Gives Kyber's capture and encode the highest GPU priority Windows allows, and high CPU priority. Measured with the GPU at full load: encoding 20% faster, worst-case delay 12 ms lower. Turn off only if a game stutters while hosting."),
            Row::new(HRow::Port, RowKind::Text, "Port", hosting.port.to_string())
                .note("One port for TCP and UDP")
                .help("Kyber uses this one port for TCP and UDP. Windows may ask to allow it through the firewall the first time."),
            if self.hosting.vigem {
                Row::new(HRow::Gamepads, RowKind::Info, "Gamepad driver (ViGEmBus)", "Installed")
                    .note("Players' controllers appear on this PC")
            } else {
                Row::new(HRow::Gamepads, RowKind::Action, "Gamepad driver (ViGEmBus)", "Not installed")
                    .note("Needed for players' controllers")
                    .help("Kyber needs this driver on the hosting PC so the other side's controllers show up as gamepads here. Opens the download page.")
            },
            Row::new(
                HRow::Paired,
                RowKind::Info,
                "Paired devices",
                match self.config.paired_devices.len() {
                    0 => "None yet".to_owned(),
                    1 => self.config.paired_devices[0].name.clone(),
                    n => format!("{n} devices"),
                },
            )
            .note("Joined with a code from the Host screen"),
            Row::new(HRow::Unpair, RowKind::Danger, "Un-pair all devices", "")
                .note("They will need a new code to join")
                .help("Forgets every paired device. Note that a device that was paired still knows the sign-in; to shut it out completely, change the password above."),
        ]
    }

    pub(super) fn act_on_host_row(&mut self, row: HRow, act: RowAct, ctx: &egui::Context) {
        let accept = act == RowAct::Accept;
        let delta: i32 = if act == RowAct::Prev { -1 } else { 1 };
        match row {
            HRow::Unpair if accept => self.unpair_all(),
            HRow::Username if accept => self.open_text_input(TextTarget::HostingUsername),
            HRow::Password if accept => self.open_text_input(TextTarget::HostingPassword),
            HRow::Port if accept => self.open_text_input(TextTarget::HostingPort),
            HRow::ShowPassword => self.hosting.show_password = !self.hosting.show_password,
            HRow::Resolution => {
                // Position 0 is "keep as it is"; the display's modes follow.
                let count = self.hosting.modes.len() as i32 + 1;
                let current = self
                    .hosting
                    .modes
                    .iter()
                    .position(|m| {
                        m.width == self.config.hosting.width
                            && m.height == self.config.hosting.height
                    })
                    .map_or(0, |index| index as i32 + 1);
                let next = (current + delta).rem_euclid(count);
                let (width, height) = match next {
                    0 => (0, 0),
                    index => {
                        let mode = self.hosting.modes[index as usize - 1];
                        (mode.width, mode.height)
                    }
                };
                self.config.hosting.width = width;
                self.config.hosting.height = height;
                self.save();
                self.note_restart_needed();
            }
            HRow::Encoder => {
                self.config.hosting.encoder = self.config.hosting.encoder.step(delta);
                self.save();
                self.note_restart_needed();
            }
            HRow::FindEncoder if accept => self.start_lab(
                crate::lab::Plan::FindFastest,
                super::latency::Purpose::PickEncoder,
                ctx,
            ),
            HRow::Boost => {
                self.config.hosting.boost = !self.config.hosting.boost;
                self.save();
                self.note_restart_needed();
            }
            HRow::Gamepads if accept && !self.hosting.vigem => match open_url(VIGEM_URL) {
                Ok(()) => self.toast_info("Opened the ViGEmBus download page in your browser."),
                Err(error) => self.toast_error(format!("Could not open the browser: {error}")),
            },
            _ => {}
        }
    }

    fn note_restart_needed(&mut self) {
        if self.hosting.server.is_some() {
            self.toast_info("Stop and start hosting to apply the change.");
        }
    }

    fn unpair_all(&mut self) {
        if self.config.paired_devices.is_empty() {
            self.toast_info("No device is paired.");
            return;
        }
        if !self.hosting.demo {
            for device in &self.config.paired_devices {
                crate::agent::forget_device(&device.id);
            }
        }
        self.config.paired_devices.clear();
        self.save();
        self.toast_info("Every device is un-paired.");
    }

    fn toggle_hosting(&mut self, ctx: &egui::Context) {
        match &self.hosting.server {
            Some(server) => server.stop(),
            None => self.start_hosting(ctx),
        }
    }

    fn open_hosting_settings(&mut self) {
        self.open_settings(Screen::Host);
        self.settings_tab = super::SETTINGS_HOSTING;
        self.settings_selected = 0;
    }

    /// Host: two panels (share, invite), the facts, and a way to the
    /// hosting settings. Three things to focus: 0 share, 1 invite, 2 settings.
    pub(super) fn host_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav], ctx: &egui::Context) {
        let mut focus = self.hosting.row_selected.min(2);
        let mut activate = None;
        for nav in navs {
            match nav {
                Nav::Back if self.pairing.open => {
                    if let Some(agent) = &self.pairing.agent {
                        agent.close_pairing();
                    }
                    self.pairing.open = false;
                }
                Nav::Back => {
                    self.screen = Screen::Home;
                    return;
                }
                Nav::Left | Nav::Up => focus = focus.saturating_sub(1),
                Nav::Right | Nav::Down => focus = (focus + 1).min(2),
                Nav::Accept => activate = Some(focus),
                Nav::Settings => activate = Some(2),
                _ => {}
            }
        }

        let state = self.hosting.server.as_ref().map(Server::state);
        let players = self.hosting.server.as_ref().map_or(0, Server::clients);
        let since = self
            .hosting
            .server
            .as_ref()
            .map(|s| s.started.elapsed())
            .unwrap_or_default();
        let boosted = self
            .hosting
            .server
            .as_ref()
            .is_some_and(|s| s.boosted().1 > 0);
        let pairing = self.pairing.agent.as_ref().and_then(crate::agent::Agent::pairing);
        let this_pc = super::pairing::computer_name();
        let address = self
            .hosting
            .addresses
            .first()
            .map(|ip| match self.config.hosting.port {
                DEFAULT_PORT => ip.to_string(),
                port => format!("{ip}:{port}"),
            })
            .unwrap_or_else(|| "No network".to_owned());
        let encoder = match (self.config.hosting.best_codec, self.config.hosting.best_latency_ms) {
            (Some(codec), Some(ms)) => format!("{} + {} \u{b7} {ms:.1} ms", short_encoder(self.config.hosting.encoder), codec.label()),
            _ => self.config.hosting.encoder.label().to_owned(),
        };
        let paired = self.config.paired_devices.len();

        centered(ui, 980.0, |ui| {
            ui.add_space(10.0);
            let (caps, colour) = match &state {
                None => ("Not sharing".to_owned(), ui::DIM),
                Some(ServerState::Ready) => (
                    format!("Live \u{b7} {}", format_duration(since)),
                    ui::OK,
                ),
                Some(_) => ("Starting".to_owned(), ui::WARN),
            };
            ui::caps(ui, &caps, colour);
            let headline = match state {
                Some(ServerState::Ready) => "You're *live*.",
                Some(_) => "Warming *up*...",
                None => "Share this *PC*.",
            };
            ui::headline(ui, headline, 46.0);
            ui.add_space(14.0);

            let gap = 22.0;
            let width = ui.available_width();
            let half = (width - gap) / 2.0;
            let panel = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.allocate_ui_with_layout(
                    vec2(half, 300.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(ui::PANEL.gamma_multiply(0.94))
                            .stroke(Stroke::new(1.0, ui::LINE))
                            .corner_radius(10.0)
                            .inner_margin(Margin::same(22))
                            .show(ui, |ui| {
                                ui.set_width(half - 44.0);
                                ui.set_min_height(256.0);
                                ui.spacing_mut().item_spacing.y = 4.0;
                                add(ui);
                            });
                    },
                );
            };
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                // ------------------------------------------------ share
                panel(ui, &mut |ui| {
                    ui.label(ui::caps_job("01 \u{b7} Share this PC", 12.0, ui::DIM));
                    ui.add_space(4.0);
                    let (big, big_colour, line) = match &state {
                        None => (
                            "Off".to_owned(),
                            ui::DIM,
                            "Turn it on and paired devices can join from their Join screen.".to_owned(),
                        ),
                        Some(ServerState::Ready) => (
                            match players {
                                0 => "Waiting".to_owned(),
                                1 => "1 player".to_owned(),
                                n => format!("{n} players"),
                            },
                            ui::LIME,
                            if boosted {
                                "Streaming this screen, sound and controls, ahead of games.".to_owned()
                            } else {
                                "Streaming this screen, sound and controls.".to_owned()
                            },
                        ),
                        Some(_) => (
                            "Starting".to_owned(),
                            ui::WARN,
                            "The Kyber server is getting ready.".to_owned(),
                        ),
                    };
                    ui.label(ui::wordmark_job(&big, 52.0, big_colour));
                    ui.label(RichText::new(line).color(ui::DIM).size(15.0));
                    ui.add_space(14.0);
                    let card = if state.is_some() {
                        ui::action_card(ui, half - 44.0, "Stop hosting", "Disconnects everyone first", ui::ActionKind::Danger, focus == 0)
                    } else {
                        ui::action_card(ui, half - 44.0, "Start hosting", "Share this screen, sound and controls", ui::ActionKind::Primary, focus == 0)
                    };
                    if card.clicked() {
                        focus = 0;
                        activate = Some(0);
                    }
                });
                // ------------------------------------------------ invite
                panel(ui, &mut |ui| {
                    ui.label(ui::caps_job("02 \u{b7} Invite a device", 12.0, ui::DIM));
                    ui.add_space(4.0);
                    match &pairing {
                        Some((code, left)) => {
                            ui.add_space(6.0);
                            ui::code_boxes(ui, code, 6, vec2(52.0, 68.0), false, 0.0);
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("On the other PC: Join, pick {this_pc}, type this code."))
                                    .color(ui::TEXT)
                                    .size(15.0),
                            );
                            ui.label(ui::caps_job(
                                &format!("Expires in {}:{:02} \u{b7} works once", left.as_secs() / 60, left.as_secs() % 60),
                                11.5,
                                ui::LIME,
                            ));
                            ui.add_space(8.0);
                            if ui::action_card(ui, half - 44.0, "Hide the code", "B also hides it", ui::ActionKind::Plain, focus == 1).clicked() {
                                focus = 1;
                                activate = Some(1);
                            }
                        }
                        None => {
                            ui.label(ui::wordmark_job(
                                &match paired {
                                    0 => "No one yet".to_owned(),
                                    1 => "1 device".to_owned(),
                                    n => format!("{n} devices"),
                                },
                                52.0,
                                ui::TEXT,
                            ));
                            ui.label(
                                RichText::new("Pair a PC once with a 6-digit code: after that it joins with one press, nothing to type.")
                                    .color(ui::DIM)
                                    .size(15.0),
                            );
                            ui.add_space(14.0);
                            if ui::action_card(ui, half - 44.0, "Show a code", "6 digits, valid for 2 minutes", ui::ActionKind::Primary, focus == 1).clicked() {
                                focus = 1;
                                activate = Some(1);
                            }
                        }
                    }
                });
            });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let fact_width = (width - 36.0) / 4.0;
                ui::fact(ui, fact_width, "This PC", &format!("{this_pc} \u{b7} {address}"), ui::TEXT);
                ui::fact(ui, fact_width, "Encoder", &encoder, ui::TEXT);
                ui::fact(
                    ui,
                    fact_width,
                    "Priority",
                    if self.config.hosting.boost { "Ahead of games" } else { "Normal" },
                    if self.config.hosting.boost { ui::LIME } else { ui::DIM },
                );
                ui::fact(
                    ui,
                    fact_width,
                    "Gamepads",
                    if self.hosting.vigem || self.hosting.demo { "Ready" } else { "Driver missing" },
                    if self.hosting.vigem || self.hosting.demo { ui::LIME } else { ui::WARN },
                );
            });
            ui.add_space(14.0);
            if ui::action_card(
                ui,
                width,
                "Hosting settings",
                "Sign-in for browsers, resolution, encoder, port, priority",
                ui::ActionKind::Plain,
                focus == 2,
            )
            .clicked()
            {
                focus = 2;
                activate = Some(2);
            }

            if let Some(failure) = &self.hosting.failure {
                ui.add_space(10.0);
                egui::Frame::new()
                    .fill(ui::PANEL)
                    .stroke(Stroke::new(1.0, ui::ERR))
                    .corner_radius(10.0)
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&failure.message).color(ui::ERR).size(15.0));
                        for line in failure.log.iter().rev().take(3).rev() {
                            ui.label(RichText::new(line).font(ui::mono(12.0)).color(ui::DIM));
                        }
                    });
            }
        });

        self.hosting.row_selected = focus;
        match activate {
            Some(0) => self.toggle_hosting(ctx),
            Some(1) if self.pairing.open => {
                if let Some(agent) = &self.pairing.agent {
                    agent.close_pairing();
                }
                self.pairing.open = false;
            }
            Some(1) => self.open_host_pairing(ctx),
            Some(_) => self.open_hosting_settings(),
            None => {}
        }
    }

    // ---------------------------------------------------- hosting text fields

    pub(super) fn hosting_text_field(&self, target: TextTarget) -> (&'static str, String, bool) {
        match target {
            TextTarget::HostingPassword => {
                ("Hosting password", self.hosting.password.clone(), false)
            }
            TextTarget::HostingPort => {
                ("Hosting port", self.config.hosting.port.to_string(), false)
            }
            _ => (
                "Name to sign in with",
                self.config.hosting.username.clone(),
                false,
            ),
        }
    }

    pub(super) fn apply_hosting_text(&mut self, target: TextTarget, value: &str) -> bool {
        match target {
            TextTarget::HostingUsername => {
                let name = value.trim();
                // The name travels in an HTTP Basic header, where ':' separates
                // it from the password.
                if name.is_empty() || name.contains(':') {
                    self.toast_error("Enter a name without ':'.");
                    return false;
                }
                self.config.hosting.username = name.to_owned();
            }
            TextTarget::HostingPassword => {
                if value.chars().count() < MIN_PASSWORD {
                    self.toast_error(format!(
                        "Use at least {MIN_PASSWORD} characters: this password protects full control of this PC."
                    ));
                    return false;
                }
                if !self.hosting.demo {
                    if let Err(error) = secrets::set_named(secrets::HOSTING, value) {
                        self.toast_error(format!("The password could not be stored: {error}"));
                        return false;
                    }
                }
                let changed = self.hosting.password != value;
                self.hosting.password = value.to_owned();
                self.hosting.password_loaded = true;
                // A new password shuts out everyone who had the old one:
                // paired devices too, so they pair again.
                if changed && !self.config.paired_devices.is_empty() {
                    if !self.hosting.demo {
                        for device in &self.config.paired_devices {
                            crate::agent::forget_device(&device.id);
                        }
                    }
                    self.config.paired_devices.clear();
                    self.toast_info("Password changed. Paired devices need to pair again.");
                }
            }
            TextTarget::HostingPort => match value.trim().parse::<u16>() {
                Ok(port) if port > 0 => self.config.hosting.port = port,
                _ => {
                    self.toast_error("The port must be a number from 1 to 65535.");
                    return false;
                }
            },
            _ => return true,
        }
        self.save();
        self.note_restart_needed();
        true
    }

    // --------------------------------------------------------------- setup

    fn open_setup(&mut self, then: Screen) {
        self.hosting.after_setup = then;
        self.screen = Screen::Setup;
    }

    fn setup_running(&self) -> bool {
        matches!(
            self.hosting.setup.as_ref().map(Setup::stage),
            Some(SetupStage::Downloading { .. } | SetupStage::Installing)
        )
    }

    pub(super) fn setup_hints(&self) -> Vec<Hint> {
        if self.setup_running() {
            Vec::new()
        } else {
            vec![
                Hint::new("A", "Enter", "Install Kyber"),
                Hint::new("B", "Esc", "Not now"),
            ]
        }
    }

    pub(super) fn start_setup(&mut self, ctx: &egui::Context) {
        if self.setup_running() || self.hosting.demo {
            return;
        }
        self.ensure_host_credentials();
        // The installer only takes plain values. If the user picked something
        // else, the installed Windows service gets a random sign-in instead;
        // hosting from the launcher uses the launcher's own either way.
        let username = Some(self.config.hosting.username.clone())
            .filter(|name| kyber::is_safe_credential(name))
            .unwrap_or_else(|| "kyber".to_owned());
        let password = Some(self.hosting.password.clone())
            .filter(|password| kyber::is_safe_credential(password))
            .unwrap_or_else(kyber::generate_password);
        let repaint = ctx.clone();
        self.hosting.setup = Some(Setup::start(username, password, move || {
            repaint.request_repaint()
        }));
    }

    pub(super) fn setup_screen(&mut self, ui: &mut egui::Ui, navs: &[Nav], ctx: &egui::Context) {
        let stage = self.hosting.setup.as_ref().map(Setup::stage);
        let running = self.setup_running();
        let mut install = false;
        let mut skip = false;
        if !running {
            install = navs.contains(&Nav::Accept);
            skip = navs.contains(&Nav::Back);
        }

        centered(ui, 760.0, |ui| {
            ui.add_space(ui.available_height() * 0.12);
            ui::caps(ui, "First start", ui::LIME);
            ui::headline(ui, "Let's get you *set up*.", 52.0);
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "Godrays needs Kyber {} on this PC, both to join another PC and to host. It can install it for you:",
                    kyber::VERSION
                ))
                .size(16.0),
            );
            for line in [
                format!(
                    "Downloads the official installer ({} MB) from gitlab.com/kyber and checks it against a known checksum.",
                    kyber::INSTALLER_BYTES / 1_000_000
                ),
                "Installs the Kyber client and server. Windows asks for permission once.".to_owned(),
                "Nothing is shared afterwards: hosting stays off until you start it on the Host screen.".to_owned(),
            ] {
                ui.horizontal_top(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, ui::ACCENT2);
                    ui.label(RichText::new(line).color(ui::DIM).size(15.0));
                });
            }
            ui.add_space(16.0);

            match &stage {
                Some(SetupStage::Downloading { received, total }) => {
                    let fraction = if *total > 0 {
                        *received as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui::progress_bar(ui, fraction);
                    ui.label(
                        RichText::new(format!(
                            "Downloading Kyber... {} of {} MB",
                            received / 1_000_000,
                            total / 1_000_000
                        ))
                        .color(ui::DIM),
                    );
                }
                Some(SetupStage::Installing) => {
                    ui::progress_bar(ui, 1.0);
                    ui.label(
                        RichText::new(
                            "Installing. Approve the Windows permission prompt to continue.",
                        )
                        .color(ui::WARN),
                    );
                }
                Some(SetupStage::Failed(error)) => {
                    ui.label(RichText::new(error).color(ui::ERR).size(16.0));
                    ui.add_space(8.0);
                }
                _ => {}
            }

            if !running {
                ui.horizontal(|ui| {
                    let label = if matches!(stage, Some(SetupStage::Failed(_))) {
                        "Try again"
                    } else {
                        "Install Kyber"
                    };
                    install |= ui::button(ui, label, true, false).clicked();
                    skip |= ui::button(ui, "Not now", false, false).clicked();
                });
            }
        });

        if install {
            self.start_setup(ctx);
        } else if skip {
            self.hosting.setup = None;
            self.hosting.setup_declined = true;
            self.screen = match self.hosting.after_setup {
                // Joining can still be looked at without Kyber; hosting cannot.
                Screen::Hosts => Screen::Hosts,
                _ => Screen::Home,
            };
        }
    }
}

/// Keyboard and pad hints for the Home and Host screens.
pub(super) fn home_hints() -> Vec<Hint> {
    vec![
        Hint::new("A", "Enter", "Open"),
        Hint::new("Y", "S", "Settings"),
        Hint::new("Start", "F11", "Fullscreen"),
    ]
}

pub(super) fn host_hints() -> Vec<Hint> {
    vec![
        Hint::new("A", "Enter", "Select"),
        Hint::new("D-pad", "Arrows", "Adjust"),
        Hint::new("B", "Esc", "Back"),
    ]
}

fn short_encoder(encoder: crate::config::Encoder) -> &'static str {
    use crate::config::Encoder;
    match encoder {
        Encoder::Nvenc => "NVENC",
        Encoder::Amf => "AMF",
        Encoder::Qsv => "Quick Sync",
        Encoder::X264 => "x264",
        Encoder::Auto => "Auto",
    }
}
