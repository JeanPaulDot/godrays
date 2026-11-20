//! The latency lab screens and the live statistics over a stream.

use std::sync::atomic::Ordering;

use eframe::egui::{self, vec2, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke};

use super::{App, SRow};
use crate::beacon;
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
    fn lab_display_mode(&self) -> Option<crate::display::Mode> {
        let (w, h) = (self.config.hosting.width, self.config.hosting.height);
        if w == 0 || self.hosting.is_demo() {
            return None;
        }
        crate::display::pick_modes(&crate::display::all_modes())
            .into_iter()
            .find(|m| m.width == w && m.height == h)
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
    pub(super) fn stream_overlay(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else {
            return;
        };
        let show_stats = !self.config.hide_stats && self.stats_visible;
        let show_leave = !self.config.hide_quit_button;
        if !show_stats && !show_leave {
            return;
        }
        let stats = session.stats();
        let stats_w = if show_stats { 470.0 } else { 0.0 };
        let leave_w = if show_leave { 124.0 } else { 0.0 };
        let size = vec2(stats_w + leave_w, 38.0);
        let monitor = ctx
            .input(|input| input.viewport().monitor_size)
            .unwrap_or(vec2(1920.0, 1080.0));
        let builder = egui::ViewportBuilder::default()
            .with_title("Godrays stream overlay")
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            // Appearing must not take the keyboard away from the stream.
            .with_active(false)
            .with_inner_size(size)
            .with_position(egui::pos2(monitor.x - size.x - 16.0, 14.0));
        let mut leave = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("leave-stream"),
            builder,
            |ctx, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let all = ui.max_rect();
                        if show_stats {
                            let rect = Rect::from_min_size(all.min, vec2(stats_w, all.height()));
                            paint_stats(ui.painter(), rect, &stats);
                        }
                        if show_leave {
                            let rect = Rect::from_min_max(
                                Pos2::new(all.right() - leave_w, all.top()),
                                all.max,
                            );
                            let response = ui.interact(rect, ui.id().with("leave"), Sense::click());
                            paint_leave(ui.painter(), rect, response.hovered());
                            leave = response.clicked();
                        }
                    });
            },
        );
        if leave {
            if let Some(session) = &self.session {
                session.stop();
            }
        }
    }
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
