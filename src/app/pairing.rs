//! Pairing and discovery in the app: the host's invite code, hosts
//! found on the network, pairing from the client, and status refreshes for
//! paired hosts.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{self, vec2, Margin, Pos2, RichText, Stroke};

use super::{App, Screen};
use crate::agent::{self, Agent, Event, HostInfo, HostStatus, Welcome};
use crate::config::{AuthMode, Host, PairedDevice, TlsMode};
use crate::discovery::{Advert, Found};
use crate::host::{Server, ServerState};
use crate::pad::Nav;
use crate::secrets;
use crate::ui;

const STATUS_EVERY: Duration = Duration::from_secs(30);

/// Who we are about to pair with.
#[derive(Clone)]
pub(super) struct PairTarget {
    pub name: String,
    pub address: String,
    pub agent_port: u16,
    /// Update this saved host instead of adding a new one.
    pub existing: Option<u64>,
}

/// Typing the host's 6-digit code.
pub(super) struct CodeEntry {
    pub target: PairTarget,
    pub digits: String,
    /// Selected keypad key, for gamepads.
    pub key: usize,
    pub error: Option<String>,
    /// When the last wrong code was entered, for the shake.
    shake_at: Option<f64>,
}

const CODE_LEN: usize = 6;

pub(super) struct PairJob {
    target: PairTarget,
    result: Receiver<Result<(Welcome, IpAddr), String>>,
}

#[derive(Default)]
pub(super) struct PairingState {
    pub agent: Option<Agent>,
    agent_failed: bool,
    /// The host side is showing a code.
    pub open: bool,
    /// The client side's code entry, while open.
    pub entry: Option<CodeEntry>,
    job: Option<PairJob>,
    /// Latest status per saved host id, from paired hosts.
    pub live: HashMap<u64, HostStatus>,
    statuses: Option<Receiver<(u64, Result<HostStatus, String>)>>,
    last_refresh: Option<Instant>,
    fingerprint: Option<String>,
    /// Sample hosts "found on the network" in demo mode.
    pub demo_found: Vec<Found>,
}

pub(super) fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "This PC".to_owned())
}

fn agent_secret_name(host_id: u64) -> String {
    format!("agent-{host_id}")
}

impl App {
    /// This PC's Kyber certificate fingerprint, computed once.
    pub(super) fn own_fingerprint(&mut self) -> String {
        if self.pairing.fingerprint.is_none() {
            self.pairing.fingerprint = self
                .hosting
                .install_dir
                .as_ref()
                .and_then(|dir| agent::fingerprint_of_pem(&dir.join("kybertest_cert.pem")));
        }
        self.pairing.fingerprint.clone().unwrap_or_default()
    }

    /// Hosts found on the network that are not saved yet.
    pub(super) fn discovered_hosts(&mut self) -> Vec<Found> {
        let own = self.own_fingerprint();
        let Some(discovery) = &self.discovery else {
            return self.pairing.demo_found.clone();
        };
        discovery
            .list(&own)
            .into_iter()
            .filter(|found| {
                !self.config.hosts.iter().any(|host| {
                    (!host.fingerprint.is_empty() && host.fingerprint == found.fingerprint)
                        || host.address.trim() == found.address.to_string()
                })
            })
            .collect()
    }

    // ------------------------------------------------------------ host side

    fn host_info(&mut self) -> HostInfo {
        let fingerprint = self.own_fingerprint();
        let password = self.hosting_password();
        let server = self.hosting.server.as_ref();
        HostInfo {
            host_name: computer_name(),
            kyber_port: self.config.hosting.port,
            username: self.config.hosting.username.clone(),
            password,
            fingerprint,
            hosting: server.map(Server::state) == Some(ServerState::Ready),
            players: server.map_or(0, Server::clients),
            codec: self.config.hosting.best_codec,
            best_latency_ms: self.config.hosting.best_latency_ms,
        }
    }

    /// Opens the code dialog on the host.
    pub(super) fn open_host_pairing(&mut self, ctx: &egui::Context) {
        if self.hosting.install_dir.is_none() {
            self.toast_error("Install Kyber first: pairing hands over its sign-in.");
            return;
        }
        self.ensure_agent(ctx, true);
        if let Some(agent) = &self.pairing.agent {
            agent.open_pairing();
            self.pairing.open = true;
        }
    }

    fn ensure_agent(&mut self, ctx: &egui::Context, report: bool) {
        if self.pairing.agent.is_some() {
            return;
        }
        let repaint = ctx.clone();
        match Agent::start(self.config.hosting.agent_port, move || {
            repaint.request_repaint()
        }) {
            Ok(agent) => {
                self.pairing.agent = Some(agent);
                self.pairing.agent_failed = false;
            }
            Err(error) => {
                if report || !self.pairing.agent_failed {
                    self.toast_error(error);
                }
                self.pairing.agent_failed = true;
            }
        }
    }

    /// Keeps the agent and the network advert in step with what this PC
    /// is doing; handles pairing events.
    pub(super) fn poll_agent(&mut self, ctx: &egui::Context) {
        if self.hosting.is_demo() {
            return;
        }
        let hosting = self.hosting.server.is_some();
        let wanted = hosting || self.pairing.open;
        if wanted {
            self.ensure_agent(ctx, false);
        } else {
            self.pairing.agent = None;
        }
        if self.pairing.agent.is_some() {
            let info = self.host_info();
            let advert = Advert {
                name: info.host_name.clone(),
                agent_port: self.pairing.agent.as_ref().map_or(0, |a| a.port),
                kyber_port: info.kyber_port,
                fingerprint: info.fingerprint.clone(),
                pairing: self.pairing.open,
                hosting: info.hosting,
            };
            if let Some(discovery) = &self.discovery {
                discovery.advertise(Some(advert));
            }
            if let Some(agent) = &self.pairing.agent {
                agent.set_info(info);
            }
        } else if let Some(discovery) = &self.discovery {
            discovery.advertise(None);
        }

        let events = self
            .pairing
            .agent
            .as_ref()
            .map(Agent::take_events)
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Paired { id, name } => {
                    self.config.paired_devices.push(PairedDevice {
                        id,
                        name: name.clone(),
                        at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    });
                    self.save();
                    self.pairing.open = false;
                    let screen = ctx.screen_rect();
                    self.fx.burst(ctx, screen.center());
                    self.toast_info(format!("Paired with {name}."));
                }
                Event::WrongCode => self.toast_error("A device entered a wrong code."),
            }
        }
        // The code expired or was used up: close the dialog.
        if self.pairing.open
            && self
                .pairing
                .agent
                .as_ref()
                .and_then(Agent::pairing)
                .is_none()
        {
            self.pairing.open = false;
        }
    }

    // ---------------------------------------------------------- client side

    pub(super) fn begin_pairing(&mut self, target: PairTarget) {
        self.pairing.entry = Some(CodeEntry {
            target,
            digits: String::new(),
            key: 0,
            error: None,
            shake_at: None,
        });
    }

    fn checking_code(&self) -> bool {
        self.pairing.job.is_some() && self.pairing.entry.is_some()
    }

    /// Starts pairing with the typed code, in the background.
    fn submit_code(&mut self) {
        if self.pairing.job.is_some() {
            return;
        }
        let Some(entry) = &mut self.pairing.entry else {
            return;
        };
        if entry.digits.len() != CODE_LEN {
            entry.error = Some("The code has 6 digits.".to_owned());
            return;
        }
        entry.error = None;
        let digits = entry.digits.clone();
        let target = entry.target.clone();
        let (sender, receiver) = mpsc::channel();
        let job_target = target.clone();
        std::thread::spawn(move || {
            let resolved = (job_target.address.as_str(), job_target.agent_port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut all| all.next());
            let outcome = match resolved {
                Some(address) => agent::pair(address, &digits, &computer_name())
                    .map(|welcome| (welcome, address.ip())),
                None => Err(format!("Could not find {}.", job_target.address)),
            };
            let _ = sender.send(outcome);
        });
        self.pairing.job = Some(PairJob {
            target,
            result: receiver,
        });
    }

    /// The client's code entry: six boxes, a keypad for gamepads and mice,
    /// digits from the keyboard. Sends the code as soon as it is complete.
    pub(super) fn code_entry_modal(
        &mut self,
        ctx: &egui::Context,
        pad_navs: &[Nav],
        key_navs: &[Nav],
        live: bool,
    ) {
        let checking = self.checking_code();
        let now = ctx.input(|i| i.time);
        let mut close = false;
        let mut submit = false;
        let mut pressed = None;
        if live {
            let typed: Vec<egui::Event> = ctx.input(|i| i.events.clone());
            let Some(entry) = &mut self.pairing.entry else {
                return;
            };
            for nav in key_navs {
                match nav {
                    Nav::Back => close = true,
                    Nav::Accept => submit = true,
                    _ => {}
                }
            }
            for nav in pad_navs {
                match nav {
                    Nav::Back if entry.digits.is_empty() => close = true,
                    Nav::Back => pressed = Some(9),
                    Nav::Accept => pressed = Some(entry.key),
                    Nav::Up | Nav::Down | Nav::Left | Nav::Right => {
                        entry.key = ui::keypad_move(entry.key, *nav)
                    }
                    _ => {}
                }
            }
            if !checking {
                for event in typed {
                    match event {
                        egui::Event::Text(text) | egui::Event::Paste(text) => {
                            for digit in text.chars().filter(char::is_ascii_digit) {
                                if entry.digits.len() < CODE_LEN {
                                    entry.digits.push(digit);
                                    entry.error = None;
                                }
                            }
                        }
                        egui::Event::Key {
                            key: egui::Key::Backspace,
                            pressed: true,
                            ..
                        } => {
                            entry.digits.pop();
                        }
                        _ => {}
                    }
                }
            }
        }

        let Some(entry) = &self.pairing.entry else {
            return;
        };
        let name = entry.target.name.clone();
        let digits = entry.digits.clone();
        let key = entry.key;
        let error = entry.error.clone();
        let shake = entry
            .shake_at
            .map_or(0.0, |at| (1.0 - (now - at) as f32 / 0.45).max(0.0));
        let mut clicked = None;
        egui::Modal::new(egui::Id::new("code-entry"))
            .frame(
                egui::Frame::new()
                    .fill(ui::PANEL)
                    .stroke(Stroke::new(1.0, ui::LINE))
                    .corner_radius(10.0)
                    .inner_margin(Margin::same(36)),
            )
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.vertical_centered(|ui| {
                    ui.label(ui::caps_job(&format!("Pair with {name}"), 12.5, ui::LIME));
                    ui.add_space(2.0);
                    ui::headline(ui, "Type the *code*.", 40.0);
                    ui.label(
                        RichText::new(format!(
                            "{name} shows it on its Host screen, under Invite a device."
                        ))
                        .color(ui::DIM)
                        .size(15.0),
                    );
                    ui.add_space(18.0);
                    ui::code_boxes(ui, &digits, CODE_LEN, vec2(56.0, 72.0), !checking, shake);
                    ui.add_space(12.0);
                    let (line, colour) = match (&error, checking) {
                        (_, true) => ("Checking the code...".to_owned(), ui::LAVENDER),
                        (Some(error), false) => (error.clone(), ui::ERR),
                        (None, false) => (
                            "Type the digits, or use the pad \u{b7} B deletes \u{b7} Esc cancels".to_owned(),
                            ui::DIM,
                        ),
                    };
                    ui.label(RichText::new(line).color(colour).size(14.5));
                    ui.add_space(16.0);
                    let key_size = vec2(104.0, 54.0);
                    ui.allocate_ui(vec2(key_size.x * 3.0 + 16.0, key_size.y * 4.0 + 24.0), |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        clicked = ui::keypad(ui, if live { key } else { usize::MAX }, key_size);
                    });
                });
            });
        if let Some(index) = clicked {
            pressed = Some(index);
            if let Some(entry) = &mut self.pairing.entry {
                entry.key = index;
            }
        }

        if close {
            self.pairing.entry = None;
            return;
        }
        if let (Some(index), false) = (pressed, checking) {
            if let Some(entry) = &mut self.pairing.entry {
                match index {
                    9 => {
                        entry.digits.pop();
                    }
                    11 => submit = true,
                    _ if entry.digits.len() < CODE_LEN => {
                        entry.digits.push_str(ui::KEYPAD[index]);
                        entry.error = None;
                    }
                    _ => {}
                }
            }
        }
        let complete = self
            .pairing
            .entry
            .as_ref()
            .is_some_and(|e| e.digits.len() == CODE_LEN && e.error.is_none());
        if !checking && (submit || (complete && live)) {
            self.submit_code();
        }
        if shake > 0.0 || checking {
            ctx.request_repaint();
        }
    }

    pub(super) fn poll_pairing(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.pairing.job else {
            return;
        };
        let Ok(outcome) = job.result.try_recv() else {
            return;
        };
        let target = job.target.clone();
        self.pairing.job = None;
        match outcome {
            Ok((welcome, address)) => {
                self.pairing.entry = None;
                self.finish_pairing(target, welcome, address, ctx);
            }
            Err(error) => match &mut self.pairing.entry {
                Some(entry) => {
                    entry.digits.clear();
                    entry.error = Some(error);
                    entry.shake_at = Some(ctx.input(|i| i.time));
                }
                None => self.toast_error(format!("Pairing failed: {error}")),
            },
        }
    }

    fn finish_pairing(
        &mut self,
        target: PairTarget,
        welcome: Welcome,
        address: IpAddr,
        ctx: &egui::Context,
    ) {
        let existing = target.existing.and_then(|id| self.config.host(id).cloned());
        let mut host = existing.unwrap_or_else(|| Host {
            id: Host::new_id(),
            name: welcome.host_name.clone(),
            address: address.to_string(),
            ..Host::default()
        });
        host.port = welcome.kyber_port;
        host.auth = AuthMode::Basic;
        host.username = welcome.username.clone();
        host.tls = TlsMode::Fingerprint;
        host.fingerprint = welcome.fingerprint.clone();
        host.agent_port = target.agent_port;
        host.agent_id = welcome.client_id.clone();
        host.codec_hint = welcome.codec;
        if let Err(error) = secrets::set(host.id, &welcome.password)
            .and_then(|()| secrets::set_named(&agent_secret_name(host.id), &welcome.client_secret))
        {
            self.toast_error(format!(
                "Paired, but the sign-in could not be stored: {error}"
            ));
            return;
        }
        let name = host.display_name().to_owned();
        match self.config.host_mut(host.id) {
            Some(saved) => *saved = host.clone(),
            None => self.config.hosts.push(host.clone()),
        }
        self.host_selected = self
            .config
            .hosts
            .iter()
            .position(|h| h.id == host.id)
            .unwrap_or(0);
        self.scroll_to_host = true;
        self.editor = None;
        self.screen = Screen::Hosts;
        self.hosts_changed();
        let screen = ctx.screen_rect();
        self.fx
            .burst(ctx, Pos2::new(screen.center().x, screen.top() + 200.0));
        self.toast_info(format!("Paired with {name}. Press A to play."));
    }

    /// Asks paired hosts for their status now and then, while the Join
    /// screen is open.
    pub(super) fn refresh_statuses(&mut self) {
        // Collect answers.
        if let Some(receiver) = &self.pairing.statuses {
            let mut changed = false;
            while let Ok((host_id, outcome)) = receiver.try_recv() {
                match outcome {
                    Ok(status) => {
                        if let Some(host) = self.config.host_mut(host_id) {
                            if status.codec.is_some() && host.codec_hint != status.codec {
                                host.codec_hint = status.codec;
                                changed = true;
                            }
                        }
                        self.pairing.live.insert(host_id, status);
                    }
                    Err(_) => {
                        self.pairing.live.remove(&host_id);
                    }
                }
            }
            if changed {
                self.save();
            }
        }
        if self.screen != Screen::Hosts || self.hosting.is_demo() {
            return;
        }
        if self
            .pairing
            .last_refresh
            .is_some_and(|at| at.elapsed() < STATUS_EVERY)
        {
            return;
        }
        self.pairing.last_refresh = Some(Instant::now());
        let (sender, receiver) = mpsc::channel();
        for host in self.config.hosts.iter().filter(|h| !h.agent_id.is_empty()) {
            let Ok(Some(secret)) = secrets::get_named(&agent_secret_name(host.id)) else {
                continue;
            };
            let (id, client, port, address) = (
                host.id,
                host.agent_id.clone(),
                host.agent_port,
                host.address.clone(),
            );
            let sender = sender.clone();
            std::thread::spawn(move || {
                let outcome = (address.as_str(), port)
                    .to_socket_addrs()
                    .ok()
                    .and_then(|mut all| all.next())
                    .ok_or_else(|| "unresolved".to_owned())
                    .and_then(|address: SocketAddr| agent::status(address, &client, &secret));
                let _ = sender.send((id, outcome));
            });
        }
        self.pairing.statuses = Some(receiver);
    }

    /// Removes a deleted host's agent secret.
    pub(super) fn forget_agent_secret(host_id: u64) {
        let _ = secrets::delete_named(&agent_secret_name(host_id));
    }
}
