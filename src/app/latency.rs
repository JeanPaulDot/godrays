//! The latency lab screens and the live statistics over a stream.

use std::sync::atomic::Ordering;

use eframe::egui::{self, vec2, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke};

use super::{App, SRow};
use crate::beacon;
use crate::config::{step_in, StreamSettings, BITRATES, VIDEO_BUFFERS};
use crate::lab::{self, Lab, LabResult, Plan, Stage};
use crate::pad::Nav;
use crate::ui::{self, Row, RowAct, RowKind};

/// What to do with a finished run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    /// Only show and keep the results.
    Show,
    /// Also make the fastest working pair the hosting default.
    PickEncoder,
}

const OTHER_APP_SECONDS: u64 = 20;

/// "just now", "12 min ago", "3 h ago", "2 days ago".
fn ago(unix: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(unix);
    match now.saturating_sub(unix) {
        s if s < 60 => "just now".to_owned(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s if s < 2 * 86_400 => "yesterday".to_owned(),
        s => format!("{} days ago", s / 86_400),
    }
}

impl App {
    pub(super) fn start_lab(&mut self, plan: Plan, purpose: Purpose, ctx: &egui::Context) {
        if self.lab.is_some() {
            return;
        }
        if self.session.is_some() {
            self.toast_info("Leave the stream first, then measure.");
            return;
        }
        if self.hosting.server.is_some() {
            self.toast_info("Stop hosting first: a test stream on the same GPU would skew both.");
            return;
        }
        let kyber_needed = !matches!(plan, Plan::OtherApp { .. });
        if kyber_needed && (self.client_path.is_none() || self.hosting.install_dir.is_none()) {
            self.toast_error("Kyber is not installed.");
            return;
        }
        let repaint = ctx.clone();
        self.lab_purpose = purpose;
        self.lab = Some(Lab::start(
            plan,
            self.hosting.install_dir.clone(),
            self.client_path.clone(),
            self.config.stream.clone(),
            self.lab_display_mode(),
            self.config.hosting.clone(),
            move || repaint.request_repaint(),
        ));
    }

    /// The display mode hosting would use, so the lab measures the same.
    /// The lab streams like hosting does: same resolution, same frame-rate
    /// cap. None when the display can stay as it is.
    fn lab_display_mode(&self) -> Option<crate::display::Mode> {
        if self.hosting.is_demo() {
            return None;
        }
        let hosting = &self.config.hosting;
        let current = crate::display::current();
        let size = (hosting.width > 0).then_some((hosting.width, hosting.height));
        crate::display::hosting_mode(&crate::display::all_modes(), current, size, hosting.max_fps)
            .filter(|mode| Some(*mode) != current)
    }

    pub(super) fn poll_lab(&mut self, ctx: &egui::Context) {
        let Some(stage) = self.lab.as_ref().map(Lab::stage) else {
            return;
        };
        match stage {
            Stage::Running { .. } => {}
            Stage::Failed(error) => {
                self.lab = None;
                if self.exit_after_lab {
                    println!("{error}");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                self.toast_error(error);
            }
            Stage::Done(results) => {
                self.lab = None;
                self.lab_results = lab::load_results();
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.report_lab(&results, ctx);
                if self.exit_after_lab {
                    for result in &results {
                        println!("{:<34} {}", result.label, result.headline());
                        if let Some(p) = &result.pipeline {
                            println!(
                                "    pipeline: encode {:.1}  network {:.1}  decode {:.1}  render {:.1}  total {:.1} (p10 {:.1}, p90 {:.1}), {:.0} fps",
                                p.encode, p.network, p.decode, p.render, p.total, p.total_p10, p.total_p90, p.fps
                            );
                        }
                        if let Some(s) = &result.screen {
                            println!(
                                "    on screen: median {:.0} ms (p10 {:.0}, p90 {:.0}) over {} screenshots",
                                s.median, s.p10, s.p90, s.samples
                            );
                        }
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn report_lab(&mut self, results: &[LabResult], ctx: &egui::Context) {
        let best = results
            .iter()
            .find(|r| r.played && r.pipeline.is_some() || r.screen.is_some());
        let Some(best) = best else {
            self.toast_error(
                "Nothing was measured: no picture arrived. For another app, the stream window must show this screen.",
            );
            return;
        };
        if self.lab_purpose == Purpose::PickEncoder {
            if let (Some(encoder), Some(codec), Some(pipeline)) =
                (best.encoder, best.codec, best.pipeline)
            {
                self.config.hosting.encoder = encoder;
                self.config.hosting.best_codec = Some(codec);
                self.config.hosting.best_latency_ms = Some(pipeline.total);
                self.save();
            }
        }
        let screen = ctx.screen_rect();
        self.fx
            .burst(ctx, Pos2::new(screen.center().x, screen.top() + 140.0));
        self.toast_info(format!("{}: {}", best.label, best.headline()));
    }

    /// While the lab runs it owns the window: a plain black screen (no lime
    /// that could be mistaken for the beacon) and the beacon on top.
    pub(super) fn lab_frame(&mut self, ctx: &egui::Context) {
        let Some(shared) = self.lab.as_ref().map(|lab| lab.shared.clone()) else {
            return;
        };
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) || self.pad_back_pressed() {
            if let Some(lab) = &self.lab {
                lab.cancel();
            }
        }
        ui::set_motion(ctx, false);
        let (what, step, steps) = match self.lab.as_ref().map(Lab::stage) {
            Some(Stage::Running { what, step, steps }) => (what, step, steps),
            _ => ("Finishing".to_owned(), 1, 1),
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui::VOID))
            .show(ctx, |ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.vertical_centered(|ui| {
                    ui.label(ui::caps_job("Latency lab", 13.0, ui::DIM));
                    ui.label(
                        RichText::new(format!("Measuring {what}"))
                            .font(ui::bold(34.0))
                            .color(ui::TEXT),
                    );
                    if steps > 1 {
                        ui.label(
                            RichText::new(format!("{step} of {steps}"))
                                .font(ui::mono(14.0))
                                .color(ui::DIM),
                        );
                    }
                    ui.add_space(10.0);
                    let note = if what == "Other app" {
                        "Show this screen in the other streaming app now (for example Moonlight connected to Sunshine on this PC). The strip at the top-left must be visible both on this screen and inside the stream window."
                    } else {
                        "A test stream window opens on the right for a few seconds. Leave the mouse and keyboard alone until it closes."
                    };
                    ui.label(RichText::new(note).color(ui::DIM).size(15.0));
                    ui.add_space(8.0);
                    ui.label(ui::caps_job("Esc cancels", 12.0, ui::DIM));
                });
            });

        if shared.beacon.load(Ordering::SeqCst) {
            let builder = egui::ViewportBuilder::default()
                .with_title("Godrays latency beacon")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                .with_active(false)
                .with_mouse_passthrough(true)
                .with_inner_size(vec2(beacon::WIDTH, beacon::HEIGHT))
                .with_position(egui::pos2(24.0, 24.0));
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("latency-beacon"),
                builder,
                |ctx, _class| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new())
                        .show(ctx, |ui| {
                            let origin = ui.max_rect().min;
                            beacon::paint(ui.painter(), origin, shared.beacon_value());
                        });
                },
            );
            // Every display refresh: the beacon is a clock.
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn pad_back_pressed(&mut self) -> bool {
        self.pad.poll(true).contains(&Nav::Back)
    }

    pub(super) fn latency_rows(&self) -> Vec<Row<SRow>> {
        let last = |pick: fn(&LabResult) -> bool| {
            self.lab_results
                .iter()
                .rev()
                .find(|r| pick(r))
                .map(LabResult::headline)
                .unwrap_or_else(|| "Not measured yet".to_owned())
        };
        let mut rows = vec![
            Row::new(
                SRow::LabMeasure,
                RowKind::Action,
                "Measure Kyber on this PC",
                last(|r| r.label.starts_with("Kyber") && r.screen.is_some()),
            )
            .help("Streams this PC to itself with the hosting encoder and your stream settings. Reports Kyber's own capture-to-display time per stage, and the delay seen on screen through the beacon strip."),
            Row::new(
                SRow::LabFastest,
                RowKind::Action,
                "Find the fastest encoder and codec",
                match (&self.config.hosting.best_codec, self.config.hosting.best_latency_ms) {
                    (Some(codec), Some(ms)) => format!(
                        "{} + {}: {ms:.1} ms",
                        self.config.hosting.encoder.label(),
                        codec.label()
                    ),
                    _ => "Not measured yet".to_owned(),
                },
            )
            .help("Tries each encoder and codec your graphics card offers, and makes the fastest one that really shows a picture the hosting default. About a minute."),
            Row::new(
                SRow::LabOther,
                RowKind::Action,
                "Measure another streaming app",
                last(|r| r.label == "Other app"),
            )
            .help("Shows the beacon strip for 20 seconds. Stream this screen with another app on this PC (Moonlight with Sunshine, for example): the same on-screen measurement, for a fair comparison."),
            Row::new(
                SRow::LiveStats,
                RowKind::Toggle(!self.config.hide_stats),
                "Latency stats over the stream",
                "",
            )
            .help("Shows Kyber's capture-to-display time and each stage in a small strip over the stream. Ctrl+Alt+Shift+S shows or hides it. Uses about 270 MB of disk per hour of streaming, deleted when the stream ends."),
        ];
        for (index, result) in self.lab_results.iter().rev().take(8).enumerate() {
            let detail = match (&result.pipeline, &result.screen) {
                (Some(p), Some(s)) if result.played => format!(
                    "{:.1} ms \u{b7} screen {:.0} ms \u{b7} enc {:.1} \u{b7} out {:.1}",
                    p.total, s.median, p.encode, p.render
                ),
                (Some(p), None) if result.played => format!(
                    "{:.1} ms \u{b7} enc {:.1} \u{b7} net {:.1} \u{b7} out {:.1}",
                    p.total, p.encode, p.network, p.render
                ),
                _ => result.headline(),
            };
            rows.push(
                Row::new(
                    SRow::LabResult(index),
                    RowKind::Info,
                    &format!("{} \u{b7} {}", result.short_label(), ago(result.at)),
                    detail,
                )
                .help("Pipeline: Kyber's timestamps, capture on the host to display on the client. On screen: the beacon strip read from screenshots, which includes the display's own refresh."),
            );
        }
        rows
    }

    pub(super) fn act_on_latency(&mut self, row: SRow, act: RowAct, ctx: &egui::Context) {
        let accept = act == RowAct::Accept;
        match row {
            SRow::LabMeasure if accept => {
                let plan = Plan::Kyber {
                    encoder: self.config.hosting.encoder_or_default(),
                    codec: self
                        .config
                        .hosting
                        .best_codec
                        .unwrap_or(self.config.stream.codec),
                };
                self.start_lab(plan, Purpose::Show, ctx);
            }
            SRow::LabFastest if accept => {
                self.start_lab(Plan::FindFastest, Purpose::PickEncoder, ctx)
            }
            SRow::LabOther if accept => self.start_lab(
                Plan::OtherApp {
                    seconds: OTHER_APP_SECONDS,
                },
                Purpose::Show,
                ctx,
            ),
            SRow::LiveStats => {
                self.config.hide_stats = !self.config.hide_stats;
                self.save();
            }
            _ => {}
        }
    }

    /// Over the stream, top-right: the live statistics and the Leave button.
    /// The toolbar over a running stream, top-right of the screen: the live
    /// latency strip (closable), Tune (stream settings, applied with a quick
    /// reconnect) and Leave.
    pub(super) fn stream_overlay(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else {
            self.tune = None;
            return;
        };
        let pid = session.pid;
        let stats_allowed = !self.config.hide_stats;
        let show_stats = stats_allowed && self.stats_visible;
        let show_buttons = !self.config.hide_quit_button;
        if !show_stats && !show_buttons {
            return;
        }
        let stats = session.stats();
        let stats_w: f32 = if show_stats { 500.0 } else { 0.0 };
        let reopen_w = if !show_stats && stats_allowed && show_buttons { 78.0 } else { 0.0 };
        let (tune_w, leave_w) = if show_buttons { (92.0, 124.0) } else { (0.0, 0.0) };
        let bar_w = stats_w + reopen_w + tune_w + leave_w;
        let tune_open = self.tune.is_some() && show_buttons;
        let panel_h = if tune_open { 318.0 } else { 0.0 };
        let width: f32 = if tune_open { bar_w.max(470.0) } else { bar_w };
        let size = vec2(width, BAR_H + panel_h);
        let monitor = ctx
            .input(|input| input.viewport().monitor_size)
            .unwrap_or(vec2(1920.0, 1080.0));
        let builder = egui::ViewportBuilder::default()
            .with_title("Godrays stream overlay")
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_transparent(true)
            // Appearing must not take the keyboard away from the stream.
            .with_active(false)
            .with_inner_size(size)
            .with_position(egui::pos2(monitor.x - size.x - 16.0, 14.0));
        let mut clicked: Option<Bar> = None;
        let mut tune_action: Option<TuneAction> = None;
        let draft = self.tune.as_ref().map(|t| t.draft.clone());
        let before = self.tune_before.clone();
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("leave-stream"),
            builder,
            |ctx, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let all = ui.max_rect();
                        let mut x = all.right() - bar_w;
                        let mut slot = |w: f32| {
                            let rect = Rect::from_min_size(Pos2::new(x, all.top()), vec2(w, BAR_H));
                            x += w;
                            rect
                        };
                        if show_stats {
                            let rect = slot(stats_w);
                            paint_stats(ui.painter(), rect, &stats);
                            let close = Rect::from_min_max(
                                Pos2::new(rect.right() - 30.0, rect.top()),
                                rect.max,
                            );
                            let response = ui.interact(close, ui.id().with("hide-stats"), Sense::click());
                            paint_close(ui.painter(), close, response.hovered());
                            if response.on_hover_text("Hide the stats (Ctrl+Alt+Shift+S)").clicked() {
                                clicked = Some(Bar::HideStats);
                            }
                        }
                        if reopen_w > 0.0 {
                            let rect = slot(reopen_w);
                            let response = ui.interact(rect, ui.id().with("show-stats"), Sense::click());
                            paint_button(ui.painter(), rect, "STATS", response.hovered(), false);
                            if response.clicked() {
                                clicked = Some(Bar::ShowStats);
                            }
                        }
                        if show_buttons {
                            let rect = slot(tune_w);
                            let response = ui.interact(rect, ui.id().with("tune"), Sense::click());
                            paint_button(ui.painter(), rect, "TUNE", response.hovered(), tune_open);
                            if response.clicked() {
                                clicked = Some(Bar::Tune);
                            }
                            let rect = slot(leave_w);
                            let response = ui.interact(rect, ui.id().with("leave"), Sense::click());
                            paint_leave(ui.painter(), rect, response.hovered());
                            if response.clicked() {
                                clicked = Some(Bar::Leave);
                            }
                        }
                        if let (true, Some(draft)) = (tune_open, &draft) {
                            let panel = Rect::from_min_max(
                                Pos2::new(all.right() - width, all.top() + BAR_H + 6.0),
                                all.max,
                            );
                            tune_action = tune_panel(ui, panel, draft, before.as_deref());
                        }
                    });
            },
        );

        match clicked {
            Some(Bar::HideStats) => {
                self.stats_visible = false;
                crate::winutil::focus_window_of(pid);
            }
            Some(Bar::ShowStats) => {
                self.stats_visible = true;
                crate::winutil::focus_window_of(pid);
            }
            Some(Bar::Tune) if self.tune.is_some() => {
                self.tune = None;
                crate::winutil::focus_window_of(pid);
            }
            Some(Bar::Tune) => {
                self.tune = Some(Tune {
                    draft: self.config.stream.clone(),
                })
            }
            Some(Bar::Leave) => {
                self.tune = None;
                if let Some(session) = &self.session {
                    session.stop();
                }
            }
            None => {}
        }
        match tune_action {
            Some(TuneAction::Change(row, delta)) => {
                if let Some(tune) = &mut self.tune {
                    change(&mut tune.draft, row, delta);
                }
            }
            Some(TuneAction::Cancel) => {
                self.tune = None;
                crate::winutil::focus_window_of(pid);
            }
            Some(TuneAction::Apply) => self.apply_tune(),
            None => {}
        }
    }

    /// Saves the tuned settings and reconnects with them.
    fn apply_tune(&mut self) {
        let Some(tune) = self.tune.take() else {
            return;
        };
        let (Some(stats), Some(host_id)) = (
            self.session.as_ref().map(|s| s.stats()),
            self.stream.as_ref().map(|v| v.host_id),
        ) else {
            return;
        };
        if stats.frames > 0 {
            self.tune_before = Some(format!(
                "{:.1} ms \u{b7} {:.0} fps with {}",
                stats.total,
                stats.fps,
                describe(&self.config.stream)
            ));
        }
        if tune.draft.codec != self.config.stream.codec {
            // An explicit choice beats the host's measured pick.
            self.config.stream.use_host_codec = false;
        }
        let use_host_codec = self.config.stream.use_host_codec;
        self.config.stream = tune.draft;
        self.config.stream.use_host_codec = use_host_codec;
        self.save();
        self.relaunch = Some(host_id);
        if let Some(session) = &self.session {
            session.stop();
        }
    }
}

const BAR_H: f32 = 38.0;

#[derive(Clone, Copy)]
enum Bar {
    HideStats,
    ShowStats,
    Tune,
    Leave,
}

/// Stream settings being tuned over a running stream.
pub(super) struct Tune {
    draft: StreamSettings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TuneRow {
    Codec,
    Bitrate,
    Delivery,
    Buffer,
    FullColour,
}

enum TuneAction {
    Change(TuneRow, i32),
    Apply,
    Cancel,
}

fn bitrate_label(mbps: u32) -> String {
    if mbps == 0 {
        "Auto".to_owned()
    } else {
        format!("{mbps} Mbit/s")
    }
}

fn buffer_label(ms: u32) -> String {
    if ms == 0 {
        "None".to_owned()
    } else {
        format!("{ms} ms")
    }
}

fn describe(s: &StreamSettings) -> String {
    format!(
        "{} \u{b7} {} \u{b7} {}",
        s.codec.label(),
        bitrate_label(s.bitrate_mbps),
        s.video_transport.label()
    )
}

fn change(s: &mut StreamSettings, row: TuneRow, delta: i32) {
    match row {
        TuneRow::Codec => s.codec = s.codec.step(delta),
        TuneRow::Bitrate => s.bitrate_mbps = step_in(BITRATES, s.bitrate_mbps, delta),
        TuneRow::Delivery => s.video_transport = s.video_transport.step(delta),
        TuneRow::Buffer => s.video_buffer_ms = step_in(VIDEO_BUFFERS, s.video_buffer_ms, delta),
        TuneRow::FullColour => s.yuv444 = !s.yuv444,
    }
}

/// The Tune panel under the toolbar.
fn tune_panel(
    ui: &mut egui::Ui,
    panel: Rect,
    draft: &StreamSettings,
    before: Option<&str>,
) -> Option<TuneAction> {
    let painter = ui.painter().clone();
    painter.rect_filled(panel, 6.0, ui::PANEL);
    painter.rect_stroke(panel, 6.0, Stroke::new(1.0, ui::LINE), egui::StrokeKind::Inside);
    let inner = panel.shrink2(vec2(16.0, 12.0));
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        "TUNE THE STREAM",
        ui::mono(11.5),
        ui::LIME,
    );
    painter.text(
        inner.right_top(),
        Align2::RIGHT_TOP,
        "Apply reconnects in a second",
        ui::font(12.5),
        ui::DIM,
    );
    let rows = [
        (TuneRow::Codec, "Codec", draft.codec.label().to_owned()),
        (TuneRow::Bitrate, "Bitrate", bitrate_label(draft.bitrate_mbps)),
        (TuneRow::Delivery, "Video delivery", draft.video_transport.label().to_owned()),
        (TuneRow::Buffer, "Video buffer", buffer_label(draft.video_buffer_ms)),
        (
            TuneRow::FullColour,
            "Full colour (4:4:4)",
            if draft.yuv444 { "On" } else { "Off" }.to_owned(),
        ),
    ];
    let mut action = None;
    let mut y = inner.top() + 26.0;
    for (row, label, value) in rows {
        let line = Rect::from_min_size(Pos2::new(inner.left(), y), vec2(inner.width(), 36.0));
        painter.text(line.left_center(), Align2::LEFT_CENTER, label, ui::font(15.0), ui::TEXT);
        let right = Rect::from_min_max(Pos2::new(line.right() - 30.0, line.top() + 3.0), line.max - vec2(0.0, 3.0));
        let left = Rect::from_min_size(Pos2::new(line.right() - 230.0, line.top() + 3.0), vec2(30.0, 30.0));
        for (rect, delta, glyph) in [(left, -1, "\u{25c0}"), (right, 1, "\u{25b6}")] {
            let response = ui.interact(rect, ui.id().with(("tune", label, delta)), Sense::click());
            let hot = response.hovered();
            painter.rect_filled(rect, 4.0, if hot { ui::LAVENDER } else { ui::PANEL_HI });
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                glyph,
                ui::font(11.0),
                if hot { ui::INK } else { ui::TEXT },
            );
            if response.clicked() {
                action = Some(TuneAction::Change(row, delta));
            }
        }
        painter.text(
            Pos2::new((left.right() + right.left()) / 2.0, line.center().y),
            Align2::CENTER_CENTER,
            value,
            ui::bold(14.5),
            ui::TEXT,
        );
        painter.line_segment(
            [line.left_bottom(), line.right_bottom()],
            Stroke::new(1.0, ui::LINE.gamma_multiply(0.6)),
        );
        y += 38.0;
    }
    if let Some(before) = before {
        painter.text(
            Pos2::new(inner.left(), y + 10.0),
            Align2::LEFT_CENTER,
            format!("Before: {before}"),
            ui::font(12.5),
            ui::DIM,
        );
    }
    let buttons_top = inner.bottom() - 40.0;
    let apply = Rect::from_min_size(Pos2::new(inner.left(), buttons_top), vec2(inner.width() * 0.62, 40.0));
    let cancel = Rect::from_min_max(Pos2::new(apply.right() + 10.0, buttons_top), inner.right_bottom());
    let response = ui.interact(apply, ui.id().with("tune-apply"), Sense::click());
    let fill = if response.hovered() { ui::mix(ui::LIME, ui::TEXT, 0.25) } else { ui::LIME };
    painter.rect_filled(apply, 5.0, fill);
    painter.text(apply.center(), Align2::CENTER_CENTER, "APPLY \u{b7} RECONNECT", ui::bold(15.0), ui::INK);
    if response.clicked() {
        action = Some(TuneAction::Apply);
    }
    let response = ui.interact(cancel, ui.id().with("tune-cancel"), Sense::click());
    paint_button(&painter, cancel, "CANCEL", response.hovered(), false);
    if response.clicked() {
        action = Some(TuneAction::Cancel);
    }
    action
}

fn paint_close(painter: &egui::Painter, rect: Rect, hot: bool) {
    if hot {
        painter.rect_filled(rect, 0.0, ui::PANEL_HI);
    }
    let c = rect.center();
    let stroke = Stroke::new(2.0, if hot { ui::TEXT } else { ui::DIM });
    painter.line_segment([c - vec2(5.0, 5.0), c + vec2(5.0, 5.0)], stroke);
    painter.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], stroke);
}

fn paint_button(painter: &egui::Painter, rect: Rect, text: &str, hot: bool, on: bool) {
    let (fill, ink) = if on {
        (ui::LAVENDER, ui::INK)
    } else if hot {
        (ui::PANEL_HI, ui::TEXT)
    } else {
        (ui::PANEL, ui::TEXT)
    };
    painter.rect_filled(rect, 0.0, fill);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, ui::LINE), egui::StrokeKind::Inside);
    painter.text(rect.center(), Align2::CENTER_CENTER, text, ui::mono(12.5), ink);
}

fn paint_stats(painter: &egui::Painter, rect: Rect, stats: &crate::metrics::Summary) {
    painter.rect_filled(rect, 0.0, ui::VOID);
    painter.rect_filled(
        Rect::from_min_size(rect.min, vec2(4.0, rect.height())),
        0.0,
        ui::LIME,
    );
    if stats.frames == 0 {
        painter.text(
            rect.left_center() + vec2(16.0, 0.0),
            Align2::LEFT_CENTER,
            "MEASURING...",
            ui::mono(12.5),
            ui::DIM,
        );
        return;
    }
    let total = painter.text(
        rect.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        format!("{:.1} ms", stats.total),
        ui::bold(18.0),
        ui::LIME,
    );
    painter.text(
        Pos2::new(total.right() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!(
            "ENC {:.1} \u{b7} NET {:.1} \u{b7} DEC {:.1} \u{b7} OUT {:.1} \u{b7} {:.0} FPS",
            stats.encode, stats.network, stats.decode, stats.render, stats.fps
        ),
        ui::mono(12.0),
        ui::TEXT,
    );
}

fn paint_leave(painter: &egui::Painter, rect: Rect, hot: bool) {
    let (fill, ink): (Color32, Color32) = if hot {
        (ui::ERR, ui::INK)
    } else {
        (ui::PANEL, ui::TEXT)
    };
    painter.rect_filled(rect, 0.0, fill);
    let cross = rect.left_center() + vec2(22.0, 0.0);
    let stroke = Stroke::new(2.5, ink);
    painter.line_segment([cross - vec2(6.0, 6.0), cross + vec2(6.0, 6.0)], stroke);
    painter.line_segment([cross + vec2(-6.0, 6.0), cross + vec2(6.0, -6.0)], stroke);
    painter.text(
        rect.left_center() + vec2(42.0, 0.0),
        Align2::LEFT_CENTER,
        "Leave",
        ui::font(16.0),
        ink,
    );
}
