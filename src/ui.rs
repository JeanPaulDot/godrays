//! The Godrays look: near-black canvas, lavender and lime, halftone dots,
//! pixel-art marks, a tight bold grotesk with one blackletter word for
//! emphasis, and small monospace capitals for labels.
//!
//! Everything is sized to be readable from a couch and usable with a pad,
//! a keyboard or a mouse. Motion (the light shafts, card pops, the sun's
//! rays) runs only while the launcher is in front and no stream is running;
//! see [`set_motion`].

use std::sync::Arc;

use eframe::egui::{
    self, text::LayoutJob, vec2, Align, Align2, Color32, FontData, FontDefinitions, FontFamily,
    FontId, Id, LayerId, Order, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    TextFormat, Ui, Vec2,
};

use crate::fx::{self, Effect};
#[allow(unused_imports)]
use crate::fx as _;
use crate::pad::Nav;

// ------------------------------------------------------------------ colours

pub const VOID: Color32 = Color32::from_rgb(0x06, 0x04, 0x09);
pub const PANEL: Color32 = Color32::from_rgb(0x10, 0x0C, 0x18);
pub const PANEL_HI: Color32 = Color32::from_rgb(0x1C, 0x15, 0x2A);
pub const LINE: Color32 = Color32::from_rgb(0x2E, 0x24, 0x44);
pub const TEXT: Color32 = Color32::from_rgb(0xF6, 0xF3, 0xFC);
pub const DIM: Color32 = Color32::from_rgb(0x96, 0x8C, 0xB2);
pub const LAVENDER: Color32 = Color32::from_rgb(0xB7, 0x94, 0xFF);
pub const PURPLE: Color32 = Color32::from_rgb(0x7B, 0x45, 0xF5);
pub const PURPLE_DEEP: Color32 = Color32::from_rgb(0x2B, 0x12, 0x5E);
pub const LIME: Color32 = Color32::from_rgb(0xC8, 0xFF, 0x3D);
pub const WARN: Color32 = Color32::from_rgb(0xFF, 0xC4, 0x3D);
pub const ERR: Color32 = Color32::from_rgb(0xFF, 0x4F, 0x6D);
/// Text drawn on top of a lavender or lime surface.
pub const INK: Color32 = Color32::from_rgb(0x0B, 0x06, 0x14);

// Names the screens use.
pub const BG: Color32 = VOID;
pub const ACCENT: Color32 = LAVENDER;
pub const ACCENT2: Color32 = LIME;
pub const ACCENT3: Color32 = LIME;
pub const OK: Color32 = LIME;

// -------------------------------------------------------------------- fonts

const BOLD: &str = "bold";
const BLACK: &str = "black";
const GOTHIC: &str = "gothic";

/// Body text: the grotesk.
pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Labels, numbers and the command line.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// The heaviest weight, for the wordmark and the big card titles.
pub fn black(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BLACK.into()))
}

/// The blackletter accent, used for one word at a time.
pub fn gothic(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(GOTHIC.into()))
}

/// Loads the fonts from the system (Windows ships all of them; they are not
/// redistributed). Anything missing falls back to egui's built-in fonts, so
/// every family always resolves.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mono_defaults = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let dir = std::env::var_os("WINDIR")
        .map(|windows| std::path::PathBuf::from(windows).join("Fonts"))
        .unwrap_or_default();
    let mut load = |name: &str, files: &[&str]| -> Option<String> {
        let bytes = files
            .iter()
            .find_map(|file| std::fs::read(dir.join(file)).ok())?;
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
        Some(name.to_owned())
    };
    let regular = load("godrays-regular", &["arial.ttf", "segoeui.ttf"]);
    let bold_face = load("godrays-bold", &["arialbd.ttf", "segoeuib.ttf"]);
    let black_face = load("godrays-black", &["ariblk.ttf", "seguibl.ttf"]);
    let gothic_face = load("godrays-gothic", &["OLDENGL.TTF", "georgiab.ttf"]);
    let mono_face = load("godrays-mono", &["CascadiaMono.ttf", "consola.ttf"]);

    let chain = |first: &[&Option<String>], tail: &[String]| -> Vec<String> {
        first
            .iter()
            .filter_map(|face| (*face).clone())
            .chain(tail.iter().cloned())
            .collect()
    };
    fonts
        .families
        .insert(FontFamily::Proportional, chain(&[&regular], &defaults));
    fonts
        .families
        .insert(FontFamily::Monospace, chain(&[&mono_face], &mono_defaults));
    fonts.families.insert(
        FontFamily::Name(BOLD.into()),
        chain(&[&bold_face, &regular], &defaults),
    );
    fonts.families.insert(
        FontFamily::Name(BLACK.into()),
        chain(&[&black_face, &bold_face], &defaults),
    );
    fonts.families.insert(
        FontFamily::Name(GOTHIC.into()),
        chain(&[&gothic_face, &bold_face], &defaults),
    );
    ctx.set_fonts(fonts);
}

pub fn apply_theme(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = VOID;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = VOID;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = PURPLE.gamma_multiply(0.6);
    visuals.selection.stroke = Stroke::new(1.0, LIME);
    visuals.text_cursor.stroke = Stroke::new(2.0, LIME);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.inactive.bg_fill = VOID;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, LAVENDER);
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, LIME);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = vec2(10.0, 10.0);
        style.spacing.button_padding = vec2(14.0, 8.0);
        for (text_style, font_id) in style.text_styles.iter_mut() {
            font_id.size = match text_style {
                egui::TextStyle::Heading => 26.0,
                egui::TextStyle::Small => 13.0,
                egui::TextStyle::Monospace => 13.0,
                _ => 16.0,
            };
        }
    });
}

// ------------------------------------------------------------------ motion

/// Turns animation on or off for this frame. With it off, everything is
/// drawn in its resting pose and nothing asks for extra repaints.
pub fn set_motion(ctx: &egui::Context, on: bool) {
    let now = ctx.input(|input| input.time) as f32;
    ctx.data_mut(|data| {
        data.insert_temp(Id::new("motion-on"), on);
        if on {
            data.insert_temp(Id::new("motion-clock"), now);
        }
    });
}

fn motion(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(Id::new("motion-on")).unwrap_or(false))
}

/// Seconds, advancing only while motion is on.
pub fn clock(ctx: &egui::Context) -> f32 {
    ctx.data(|data| data.get_temp(Id::new("motion-clock")).unwrap_or(0.0))
}

/// 0..1 following `on`, over `seconds`. Jumps when motion is off.
fn slide(ctx: &egui::Context, id: Id, on: bool, seconds: f32) -> f32 {
    if motion(ctx) {
        ctx.animate_bool_with_time(id, on, seconds)
    } else if on {
        1.0
    } else {
        0.0
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
        channel(a.a(), b.a()),
    )
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Overshoots a little before settling: the "pop".
fn ease_out_back(t: f32) -> f32 {
    let c = 1.70158;
    1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2)
}

/// Tells the backdrop where the selection is: the light leans towards it.
fn set_focus(ctx: &egui::Context, point: Pos2) {
    ctx.data_mut(|data| data.insert_temp(Id::new("focus-point"), point));
}

/// The backdrop: light shafts from above, dithered into dots on the GPU.
/// The light drifts towards whatever is selected.
pub fn background(ctx: &egui::Context, strength: f32) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(LayerId::background());
    let focus = ctx
        .data(|data| data.get_temp::<Pos2>(Id::new("focus-point")))
        .unwrap_or(Pos2::new(screen.center().x, screen.top()));
    let target = [
        0.5 + ((focus.x - screen.left()) / screen.width().max(1.0) - 0.5) * 0.75,
        -0.16,
    ];
    let light = if motion(ctx) {
        let dt = ctx.input(|input| input.stable_dt).min(0.1);
        let previous: [f32; 2] = ctx
            .data(|data| data.get_temp(Id::new("light")))
            .unwrap_or(target);
        let k = 1.0 - (-dt * 2.5).exp();
        [
            previous[0] + (target[0] - previous[0]) * k,
            previous[1] + (target[1] - previous[1]) * k,
        ]
    } else {
        target
    };
    ctx.data_mut(|data| data.insert_temp(Id::new("light"), light));

    // Eased so switching screens dims or brightens the light smoothly.
    let strength = if motion(ctx) {
        ctx.animate_value_with_time(Id::new("light-strength"), strength, 0.6)
    } else {
        strength
    };
    let effect = Effect::rays(
        clock(ctx),
        light,
        strength,
        [VOID, PURPLE_DEEP, LAVENDER, LIME],
    );
    if !fx::paint(&painter, screen, effect) {
        // No shader: a still, softer version made of plain circles.
        painter.rect_filled(screen, 0.0, VOID);
        let source = Pos2::new(
            screen.left() + screen.width() * light[0],
            screen.top() + screen.height() * light[1],
        );
        for step in 0..12 {
            let falloff = 1.0 - step as f32 / 12.0;
            painter.circle_filled(
                source,
                screen.height() * 0.9 * falloff,
                PURPLE.gamma_multiply(0.018),
            );
        }
    }
}

/// Fades and slides a screen in. `age` is seconds since it was opened.
pub fn enter(ui: &mut Ui, age: f32) {
    if !motion(ui.ctx()) {
        return;
    }
    let t = ease_out((age / 0.28).clamp(0.0, 1.0));
    ui.set_opacity(t);
    ui.add_space((1.0 - t) * 22.0);
}

// --------------------------------------------------------------- typography

fn format(font_id: FontId, colour: Color32, spacing: f32) -> TextFormat {
    TextFormat {
        font_id,
        color: colour,
        extra_letter_spacing: spacing,
        valign: Align::BOTTOM,
        ..Default::default()
    }
}

/// A big statement line. Words between asterisks are set in the
/// blackletter accent: `"Let there be *light*."`.
pub fn headline_job(text: &str, size: f32, colour: Color32, accent: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (index, part) in text.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }
        if index % 2 == 1 {
            job.append(part, 0.0, format(gothic(size * 1.08), accent, 0.0));
        } else {
            job.append(part, 0.0, format(bold(size), colour, -size * 0.045));
        }
    }
    job
}

pub fn headline(ui: &mut Ui, text: &str, size: f32) -> Response {
    ui.label(headline_job(text, size, TEXT, LIME))
}

/// Small monospace capitals, for labels and status lines.
pub fn caps_job(text: &str, size: f32, colour: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        format(mono(size), colour, size * 0.08),
    );
    job
}

pub fn caps(ui: &mut Ui, text: &str, colour: Color32) -> Response {
    ui.label(caps_job(text, 12.5, colour))
}

/// The wordmark: tight, heavy capitals.
pub fn wordmark_job(text: &str, size: f32, colour: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        format(black(size), colour, -size * 0.02),
    );
    job
}

/// Shortens `text` with an ellipsis until it fits `max_width`.
pub fn elide(painter: &Painter, text: &str, font_id: &FontId, max_width: f32) -> String {
    let width = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font_id.clone(), TEXT)
            .size()
            .x
    };
    if width(text) <= max_width {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "...";
        if width(&candidate) <= max_width {
            return candidate;
        }
    }
    String::new()
}

fn paint_job(painter: &Painter, pos: Pos2, anchor: Align2, job: LayoutJob) -> Rect {
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, TEXT);
    rect
}

// ------------------------------------------------------------- pixel art

/// Draws a bitmap: `#` solid, `+` dithered (every other cell), `o` accent.
fn bitmap(
    painter: &Painter,
    origin: Pos2,
    cell: f32,
    rows: &[&str],
    main: Color32,
    accent: Color32,
) {
    for (y, row) in rows.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            let colour = match c {
                '#' => main,
                '+' if (x + y) % 2 == 0 => main,
                'o' => accent,
                _ => continue,
            };
            let min = origin + vec2(x as f32 * cell, y as f32 * cell);
            // A hair of overlap avoids seams between cells.
            painter.rect_filled(
                Rect::from_min_size(min, Vec2::splat(cell + 0.35)),
                0.0,
                colour,
            );
        }
    }
}

fn bitmap_size(rows: &[&str], cell: f32) -> Vec2 {
    let width = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    vec2(width as f32 * cell, rows.len() as f32 * cell)
}

const SCREEN_ICON: [&str; 15] = [
    "################",
    "#..............#",
    "#..............#",
    "#.....o........#",
    "#.....oo.......#",
    "#.....ooo......#",
    "#.....oooo.....#",
    "#.....ooo......#",
    "#.....oo.......#",
    "#.....o........#",
    "#..............#",
    "#..............#",
    "################",
    "......####......",
    "...##########...",
];

const PLUS_ICON: [&str; 11] = [
    "....###....",
    "....###....",
    "....###....",
    "....###....",
    "###########",
    "###ooooo###",
    "###########",
    "....###....",
    "....###....",
    "....###....",
    "....###....",
];

const SEARCH_ICON: [&str; 12] = [
    "..#####.....",
    ".##...##....",
    "##.....##...",
    "#.......#...",
    "#.......#...",
    "#.......#...",
    "##.....##...",
    ".##...###...",
    "..#####ooo..",
    "........ooo.",
    ".........ooo",
    "..........oo",
];

/// The Godrays "G": a heavy octagonal letter on a 16 x 16 grid, after the
/// chamfered G of the Guru identity.
const G_GLYPH: [&str; 16] = [
    "...##########...",
    "..############..",
    ".##############.",
    "###.............",
    "###.............",
    "###.............",
    "###....#########",
    "###....#########",
    "###....#########",
    "###..........###",
    "###..........###",
    "###..........###",
    "###..........###",
    ".##############.",
    "..############..",
    "...##########...",
];

/// Width and height of the mark's grid: the glyph plus its extrusion.
pub const MARK_CELLS: i32 = 17;

/// 4 x 4 ordered-dither threshold, 0..1.
fn bayer4(x: i32, y: i32) -> f32 {
    const M: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    (M[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize] as f32 + 0.5) / 16.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkCell {
    /// Upper part of the letter.
    Primary,
    /// Lower part; the two meet in an ordered dither.
    Secondary,
    /// The extrusion, one step towards the bottom right.
    Shadow,
}

/// Cells of the mark. The dither line between the two body colours drifts
/// slowly with `t`.
pub fn mark_cells(t: f32) -> Vec<(i32, i32, MarkCell)> {
    let body = |x: i32, y: i32| {
        (0..16).contains(&x)
            && (0..16).contains(&y)
            && G_GLYPH[y as usize].as_bytes()[x as usize] == b'#'
    };
    // Inside the letter's octagonal outline (counter included).
    let hull = |x: i32, y: i32| {
        (0..16).contains(&x)
            && (0..16).contains(&y)
            && x + y >= 3
            && (15 - x) + y >= 3
            && x + (15 - y) >= 3
            && (15 - x) + (15 - y) >= 3
    };
    let drift = 0.6 * (t * 1.1).sin();
    let mut cells = Vec::new();
    for y in 0..MARK_CELLS {
        for x in 0..MARK_CELLS {
            if body(x, y) {
                // Solid above, solid below, a short ordered-dither band between.
                let v = ((y as f32 - 6.0 + drift) / 4.0).clamp(0.0, 1.0);
                let kind = if bayer4(x, y) < v {
                    MarkCell::Secondary
                } else {
                    MarkCell::Primary
                };
                cells.push((x, y, kind));
                continue;
            }
            if hull(x, y) {
                continue;
            }
            // Extrusion: one solid step towards the bottom right.
            if body(x - 1, y - 1) {
                cells.push((x, y, MarkCell::Shadow));
            }
        }
    }
    cells
}


/// The "share" icon: a dot sending out octagonal rings. While `live`, a
/// bright ring travels outwards.
fn signal(painter: &Painter, origin: Pos2, cell: f32, t: f32, live: bool, colours: [Color32; 2]) {
    const SIZE: i32 = 21;
    let centre = SIZE / 2;
    let active = if live {
        ((t * 2.2) as i32).rem_euclid(3)
    } else {
        -1
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = (x - centre).abs() as f32;
            let dy = (y - centre).abs() as f32;
            // Octagonal distance, to echo the G's chamfers.
            let d = dx.max(dy).max((dx + dy) * 0.72);
            let colour = if d <= 1.5 {
                Some(colours[1])
            } else {
                match [4.0f32, 7.0, 10.0]
                    .iter()
                    .position(|r| (d - r).abs() < 0.55)
                {
                    Some(ring) if ring as i32 == active => Some(colours[1]),
                    Some(_) => Some(colours[0]),
                    None => None,
                }
            };
            if let Some(colour) = colour {
                let min = origin + vec2(x as f32 * cell, y as f32 * cell);
                painter.rect_filled(
                    Rect::from_min_size(min, Vec2::splat(cell + 0.3)),
                    0.0,
                    colour,
                );
            }
        }
    }
}

/// The app icon at `size` x `size` pixels, as RGBA: the G on a black
/// octagon (the same chamfer as the letter). Small sizes drop the extrusion
/// and fill the tile, so the letter stays legible.
pub fn icon_rgba(size: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; size * size * 4];
    let chamfer = (size as f32 * 0.2) as i64;
    let last = size as i64 - 1;
    for y in 0..size as i64 {
        for x in 0..size as i64 {
            let inside = x + y >= chamfer
                && (last - x) + y >= chamfer
                && x + (last - y) >= chamfer
                && (last - x) + (last - y) >= chamfer;
            if inside {
                let i = (y as usize * size + x as usize) * 4;
                rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    let small = size <= 32;
    let cell = if small {
        (size * 3 / 4 / 16).max(1)
    } else {
        ((size as f32 * 0.6 / 16.0).round() as usize).max(1)
    };
    let glyph = 16 * cell;
    // Centre the letter; the extrusion falls into the bottom-right margin.
    let offset = size.saturating_sub(glyph) / 2 - if small { 0 } else { cell };
    for (x, y, kind) in mark_cells(0.0) {
        let colour = match kind {
            // Black and white: a white G, no shading.
            MarkCell::Primary | MarkCell::Secondary => Color32::WHITE,
            MarkCell::Shadow => continue,
        };
        for dy in 0..cell {
            for dx in 0..cell {
                let px = offset + x as usize * cell + dx;
                let py = offset + y as usize * cell + dy;
                if px < size && py < size {
                    let i = (py * size + px) * 4;
                    rgba[i..i + 4].copy_from_slice(&[colour.r(), colour.g(), colour.b(), 255]);
                }
            }
        }
    }
    rgba
}

/// The window and taskbar icon.
pub fn window_icon() -> egui::IconData {
    const SIZE: usize = 64;
    egui::IconData {
        rgba: icon_rgba(SIZE),
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

/// Writes the icon as a multi-size .ico (for the .exe) and a 256 px .png.
pub fn export_icon(ico: &std::path::Path) -> Result<(), String> {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    let frames = [16usize, 24, 32, 48, 64, 128, 256]
        .iter()
        .map(|&size| {
            IcoFrame::as_png(
                &icon_rgba(size),
                size as u32,
                size as u32,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(parent) = ico.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(ico).map_err(|e| e.to_string())?;
    IcoEncoder::new(file)
        .encode_images(&frames)
        .map_err(|e| e.to_string())?;
    image::save_buffer(
        ico.with_extension("png"),
        &icon_rgba(256),
        256,
        256,
        image::ColorType::Rgba8,
    )
    .map_err(|e| e.to_string())
}

// ------------------------------------------------------------------- hints

/// One entry of the footer legend: a pad control, its keyboard equivalent
/// and what it does on the current screen.
pub struct Hint {
    pub pad: &'static str,
    pub key: &'static str,
    pub text: &'static str,
}

impl Hint {
    pub const fn new(pad: &'static str, key: &'static str, text: &'static str) -> Self {
        Self { pad, key, text }
    }
}

fn pad_colour(glyph: &str) -> Color32 {
    match glyph {
        "A" => LIME,
        "B" => ERR,
        "X" => LAVENDER,
        "Y" => WARN,
        _ => PANEL_HI,
    }
}

/// Draws the legend, using pad glyphs when a controller is connected and
/// key names otherwise.
pub fn hints(ui: &mut Ui, hints: &[Hint], use_pad: bool) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for hint in hints {
            let glyph = if use_pad { hint.pad } else { hint.key };
            if glyph.is_empty() {
                continue;
            }
            let label = glyph.to_uppercase();
            let galley = ui.painter().layout_no_wrap(label.clone(), mono(12.0), INK);
            let round = use_pad && glyph.len() == 1;
            let width = if round { 24.0 } else { galley.size().x + 14.0 };
            let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
            let fill = if use_pad { pad_colour(glyph) } else { PANEL_HI };
            let ink = if use_pad && round { INK } else { TEXT };
            ui.painter()
                .rect_filled(rect, if round { 12.0 } else { 4.0 }, fill);
            if !use_pad || !round {
                ui.painter()
                    .rect_stroke(rect, 4.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
            }
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, label, mono(12.0), ink);
            ui.label(caps_job(hint.text, 12.0, DIM));
            ui.add_space(12.0);
        }
    });
}

// -------------------------------------------------------------------- rows

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Cycles through values with left / right.
    Choice,
    Toggle(bool),
    /// Opens the text entry.
    Text,
    Action,
    Danger,
    /// Read-only line.
    Info,
}

pub struct Row<T> {
    pub id: T,
    pub label: String,
    pub value: String,
    pub kind: RowKind,
    /// One-line explanation shown under the list while the row is selected.
    pub help: &'static str,
    /// The value is a stand-in for an empty field and is drawn dimmed.
    pub placeholder: bool,
    /// A short explanation shown under the label, always.
    pub note: &'static str,
}

impl<T> Row<T> {
    pub fn new(id: T, kind: RowKind, label: &str, value: impl Into<String>) -> Self {
        Self {
            id,
            label: label.to_owned(),
            value: value.into(),
            kind,
            help: "",
            placeholder: false,
            note: "",
        }
    }

    pub fn placeholder(mut self, placeholder: bool) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn note(mut self, note: &'static str) -> Self {
        self.note = note;
        self
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowAct {
    Accept,
    Prev,
    Next,
}

const ROW_HEIGHT: f32 = 54.0;
const NOTED_ROW_HEIGHT: f32 = 66.0;

/// A vertical list of setting rows. Moves `selected` with up / down and
/// returns the row the user acted on, if any.
pub fn row_list<T: Copy>(
    ui: &mut Ui,
    rows: &[Row<T>],
    selected: &mut usize,
    navs: &[Nav],
) -> Option<(T, RowAct)> {
    if rows.is_empty() {
        return None;
    }
    *selected = (*selected).min(rows.len() - 1);
    let mut action = None;
    let mut moved = false;
    for nav in navs {
        match nav {
            Nav::Up if *selected > 0 => {
                *selected -= 1;
                moved = true;
            }
            Nav::Down if *selected + 1 < rows.len() => {
                *selected += 1;
                moved = true;
            }
            Nav::Accept => action = Some((rows[*selected].id, RowAct::Accept)),
            Nav::Left => action = Some((rows[*selected].id, RowAct::Prev)),
            Nav::Right => action = Some((rows[*selected].id, RowAct::Next)),
            _ => {}
        }
    }

    // Room on the right for the selected row's nudge.
    let width = (ui.available_width() - 14.0).min(860.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(4.0);
            for (index, row) in rows.iter().enumerate() {
                let height = if row.note.is_empty() {
                    ROW_HEIGHT
                } else {
                    NOTED_ROW_HEIGHT
                };
                let (rect, response) =
                    ui.allocate_exact_size(vec2(width, height), Sense::click());
                let is_selected = index == *selected;
                if is_selected && moved {
                    ui.scroll_to_rect(rect.expand(ROW_HEIGHT), None);
                }
                if let Some(act) = paint_row(ui, rect, &response, row, index, is_selected) {
                    *selected = index;
                    action = Some((row.id, act));
                }
            }
            ui.add_space(8.0);
        });
    action
}

fn arrow(painter: &Painter, center: Pos2, size: f32, right: bool, colour: Color32) {
    let dx = if right { size } else { -size };
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + vec2(dx * 0.5, 0.0),
            center + vec2(-dx * 0.5, -size * 0.6),
            center + vec2(-dx * 0.5, size * 0.6),
        ],
        colour,
        Stroke::NONE,
    ));
}

fn paint_row<T>(
    ui: &Ui,
    rect: Rect,
    response: &Response,
    row: &Row<T>,
    index: usize,
    selected: bool,
) -> Option<RowAct> {
    let ctx = ui.ctx();
    let painter = ui.painter_at(rect.expand(3.0));
    let focus = slide(ctx, response.id.with("focus"), selected, 0.14);
    let hover = slide(ctx, response.id.with("hover"), response.hovered(), 0.10);
    let rect = rect.translate(vec2(8.0 * ease_out(focus), 0.0));
    if selected {
        set_focus(ctx, rect.center());
    }

    let fill = mix(
        PANEL.gamma_multiply(0.9),
        PANEL_HI,
        hover * 0.5 + focus * 0.5,
    );
    painter.rect_filled(rect, 6.0, fill);
    if focus > 0.0 {
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.5, LAVENDER.gamma_multiply(focus)),
            StrokeKind::Inside,
        );
        // The lime tab on the left edge grows in.
        let bar = Rect::from_min_max(
            Pos2::new(rect.left(), rect.center().y - 16.0 * focus),
            Pos2::new(rect.left() + 5.0, rect.center().y + 16.0 * focus),
        );
        painter.rect_filled(bar, 0.0, LIME);
    }

    // Row number, like a printed index.
    paint_job(
        &painter,
        rect.left_center() + vec2(22.0, 0.0),
        Align2::LEFT_CENTER,
        caps_job(&format!("{:02}", index + 1), 11.5, mix(LINE, LIME, focus)),
    );
    let label_colour = match row.kind {
        RowKind::Danger => ERR,
        RowKind::Info => DIM,
        _ => TEXT,
    };
    let label_font = if selected { bold(17.0) } else { font(17.0) };
    if row.note.is_empty() {
        painter.text(
            rect.left_center() + vec2(56.0, 0.0),
            Align2::LEFT_CENTER,
            &row.label,
            label_font,
            label_colour,
        );
    } else {
        painter.text(
            rect.left_center() + vec2(56.0, -10.0),
            Align2::LEFT_CENTER,
            &row.label,
            label_font,
            label_colour,
        );
        let note_font = font(13.5);
        let note = elide(&painter, row.note, &note_font, rect.width() * 0.5);
        painter.text(
            rect.left_center() + vec2(56.0, 12.0),
            Align2::LEFT_CENTER,
            note,
            note_font,
            DIM,
        );
    }

    let right = rect.right_center() - vec2(18.0, 0.0);
    let mut act = None;
    match row.kind {
        RowKind::Toggle(on) => {
            let position = slide(ctx, response.id.with("toggle"), on, 0.16);
            let track = Rect::from_center_size(right - vec2(26.0, 0.0), vec2(52.0, 26.0));
            painter.rect_filled(track, 4.0, mix(LINE, LIME, position));
            let travel = track.width() - 24.0;
            let knob_x = track.left() + 12.0 + travel * ease_out_back(position).clamp(-0.1, 1.1);
            let knob =
                Rect::from_center_size(Pos2::new(knob_x, track.center().y), vec2(18.0, 18.0));
            painter.rect_filled(knob, 2.0, mix(DIM, INK, position));
            if response.clicked() {
                act = Some(RowAct::Accept);
            }
        }
        RowKind::Choice => {
            let arrow_colour = mix(DIM, LIME, focus);
            let right_arrow = right - vec2(6.0, 0.0);
            arrow(&painter, right_arrow, 9.0, true, arrow_colour);
            let value_right = right_arrow - vec2(18.0, 0.0);
            let value_font = bold(16.0);
            let value = elide(&painter, &row.value, &value_font, rect.width() * 0.5);
            let value_rect = painter.text(
                value_right,
                Align2::RIGHT_CENTER,
                value,
                value_font,
                mix(TEXT, LIME, focus),
            );
            let left_arrow = Pos2::new(value_rect.left() - 18.0, right.y);
            arrow(&painter, left_arrow, 9.0, false, arrow_colour);
            if response.clicked() {
                // Clicking the left arrow steps back; anywhere else steps forward.
                let on_left_arrow = response
                    .interact_pointer_pos()
                    .is_some_and(|p| (p.x - left_arrow.x).abs() < 16.0);
                act = Some(if on_left_arrow {
                    RowAct::Prev
                } else {
                    RowAct::Next
                });
            }
        }
        RowKind::Text | RowKind::Info | RowKind::Action | RowKind::Danger => {
            let value_font = font(16.0);
            let value = elide(&painter, &row.value, &value_font, rect.width() * 0.55);
            let colour = if row.kind == RowKind::Text && !row.placeholder {
                TEXT
            } else {
                DIM
            };
            let shown = painter.text(right, Align2::RIGHT_CENTER, value, value_font, colour);
            // Actions get a small arrow that nudges forward when selected.
            if matches!(row.kind, RowKind::Action | RowKind::Danger) && focus > 0.0 {
                let nudge = if motion(ctx) {
                    (clock(ctx) * 5.0).sin().abs() * 3.0
                } else {
                    0.0
                };
                let at = Pos2::new(shown.left() - 16.0 + nudge, right.y);
                let colour = if row.kind == RowKind::Danger {
                    ERR
                } else {
                    LIME
                };
                arrow(&painter, at, 8.0, true, colour.gamma_multiply(focus));
            }
            if response.clicked() {
                act = Some(RowAct::Accept);
            }
        }
    }
    act
}

// ------------------------------------------------------------------- cards

/// The pose of a card this frame: where it is, how selected it looks.
struct Pose {
    rect: Rect,
    focus: f32,
}

/// Selected cards grow with a little overshoot, lift and gently float.
fn card_pose(ctx: &egui::Context, response: &Response, rect: Rect, selected: bool) -> Pose {
    let focus = slide(ctx, response.id.with("focus"), selected, 0.22);
    let hover = slide(ctx, response.id.with("hover"), response.hovered(), 0.12);
    let scale = 1.0 + 0.05 * ease_out_back(focus) + 0.015 * hover;
    let float = if selected && motion(ctx) {
        (clock(ctx) * 2.0).sin() * 2.0
    } else {
        0.0
    };
    let rect = Rect::from_center_size(
        rect.center() + vec2(0.0, -6.0 * focus + float),
        rect.size() * scale,
    );
    if selected {
        set_focus(ctx, rect.center());
    }
    Pose { rect, focus }
}

/// Lavender when selected, near-black otherwise.
fn card_surface(ctx: &egui::Context, painter: &Painter, pose: &Pose, hovered: bool) {
    let rect = pose.rect;
    let radius = 8.0;
    // A hard offset shadow in lime, like a print misregistration.
    if pose.focus > 0.0 {
        painter.rect_filled(
            rect.translate(vec2(7.0, 7.0) * pose.focus),
            radius,
            LIME.gamma_multiply(pose.focus),
        );
    }
    let rest = if hovered { PANEL_HI } else { PANEL };
    painter.rect_filled(rect, radius, mix(rest, LAVENDER, pose.focus));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, mix(LINE, LAVENDER, pose.focus)),
        StrokeKind::Inside,
    );
    let _ = ctx;
}

fn status_line(
    painter: &Painter,
    ctx: &egui::Context,
    pos: Pos2,
    text: &str,
    colour: Color32,
    focus: f32,
) {
    // On a selected (lavender) card the status sits in a dark pill.
    let job = caps_job(text, 11.5, if focus > 0.5 { LIME } else { colour });
    let galley = painter.layout_job(job);
    let size = galley.size() + vec2(30.0, 10.0);
    let rect = Rect::from_min_size(pos - vec2(0.0, size.y * 0.5), size);
    if focus > 0.5 {
        painter.rect_filled(rect, 4.0, INK);
    }
    let dot = rect.left_center() + vec2(12.0, 0.0);
    let live = colour == OK && motion(ctx);
    if live {
        let pulse = (clock(ctx) * 2.4).sin() * 0.5 + 0.5;
        painter.rect_filled(
            Rect::from_center_size(dot, Vec2::splat(6.0 + 6.0 * pulse)),
            0.0,
            colour.gamma_multiply(0.3 * (1.0 - pulse)),
        );
    }
    let dot_colour = if focus > 0.5 { LIME } else { colour };
    painter.rect_filled(
        Rect::from_center_size(dot, Vec2::splat(6.0)),
        0.0,
        dot_colour,
    );
    painter.galley(
        Pos2::new(rect.left() + 22.0, rect.center().y - galley.size().y * 0.5),
        galley,
        TEXT,
    );
}

pub const TILE: Vec2 = vec2(252.0, 184.0);
pub const TILE_GAP: f32 = 24.0;

pub enum TileIcon {
    Monitor,
    Plus,
    Search,
}

pub struct Tile<'a> {
    pub icon: TileIcon,
    pub title: &'a str,
    pub subtitle: &'a str,
    /// Status line with its dot colour.
    pub status: Option<(&'a str, Color32)>,
    /// Hosts are solid tiles; the "add" and "find" actions are outlined.
    pub outlined: bool,
    /// Shown as "01", "02"... in the corner.
    pub number: usize,
}

pub fn tile(ui: &mut Ui, tile: &Tile, selected: bool, scroll_to: bool) -> Response {
    let (slot, response) = ui.allocate_exact_size(TILE, Sense::click());
    if scroll_to {
        ui.scroll_to_rect(slot.expand(TILE_GAP), Some(Align::Center));
    }
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let pose = card_pose(&ctx, &response, slot, selected);
    let rect = pose.rect;
    if tile.outlined && pose.focus < 0.01 {
        // Actions: a dashed outline instead of a solid card.
        dashed_rect(
            &painter,
            rect,
            if response.hovered() { LAVENDER } else { LINE },
        );
    } else {
        card_surface(&ctx, &painter, &pose, response.hovered());
    }
    let ink = mix(TEXT, INK, pose.focus);
    let dim = mix(DIM, INK.gamma_multiply(0.75), pose.focus);

    paint_job(
        &painter,
        rect.left_top() + vec2(16.0, 14.0),
        Align2::LEFT_TOP,
        caps_job(&format!("{:02}", tile.number), 11.5, dim),
    );
    let (rows, cell): (&[&str], f32) = match tile.icon {
        TileIcon::Monitor => (&SCREEN_ICON, 3.0),
        TileIcon::Plus => (&PLUS_ICON, 3.6),
        TileIcon::Search => (&SEARCH_ICON, 3.4),
    };
    let size = bitmap_size(rows, cell);
    bitmap(
        &painter,
        rect.right_top() + vec2(-16.0 - size.x, 16.0),
        cell,
        rows,
        TEXT,
        TEXT,
    );

    let text_width = rect.width() - 32.0;
    let title_font = bold(21.0);
    let title = elide(&painter, tile.title, &title_font, text_width);
    paint_job(
        &painter,
        rect.left_bottom() + vec2(16.0, -66.0),
        Align2::LEFT_BOTTOM,
        {
            let mut job = LayoutJob::default();
            job.append(&title, 0.0, format(title_font, ink, -0.6));
            job
        },
    );
    let small = font(13.5);
    let subtitle = elide(&painter, tile.subtitle, &small, text_width);
    painter.text(
        rect.left_bottom() + vec2(16.0, -50.0),
        Align2::LEFT_CENTER,
        subtitle,
        small,
        dim,
    );
    if let Some((status, colour)) = tile.status {
        status_line(
            &painter,
            &ctx,
            rect.left_bottom() + vec2(10.0, -22.0),
            status,
            colour,
            pose.focus,
        );
    }
    response
}

fn dashed_rect(painter: &Painter, rect: Rect, colour: Color32) {
    let stroke = Stroke::new(1.5, colour);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    for pair in corners.windows(2) {
        painter.extend(egui::Shape::dashed_line(
            &[pair[0], pair[1]],
            stroke,
            7.0,
            6.0,
        ));
    }
}

// -------------------------------------------------------------- hero cards

pub const HERO: Vec2 = vec2(360.0, 300.0);
pub const HERO_GAP: f32 = 40.0;

#[derive(Clone, Copy)]
pub enum HeroIcon {
    Join,
    Host,
}

/// One of the two big choices on the Home screen. `live` animates the sun
/// (this PC is being shared right now).
#[allow(clippy::too_many_arguments)]
pub fn hero_card(
    ui: &mut Ui,
    icon: HeroIcon,
    title: &str,
    subtitle: &str,
    status: &str,
    status_colour: Color32,
    selected: bool,
    live: bool,
) -> Response {
    let (slot, response) = ui.allocate_exact_size(HERO, Sense::click());
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let pose = card_pose(&ctx, &response, slot, selected);
    card_surface(&ctx, &painter, &pose, response.hovered());
    let rect = pose.rect;
    let ink = mix(TEXT, INK, pose.focus);
    let dim = mix(DIM, INK.gamma_multiply(0.75), pose.focus);
    let t = clock(&ctx);

    let number = match icon {
        HeroIcon::Join => "01",
        HeroIcon::Host => "02",
    };
    paint_job(
        &painter,
        rect.left_top() + vec2(22.0, 20.0),
        Align2::LEFT_TOP,
        caps_job(number, 12.5, dim),
    );
    match icon {
        HeroIcon::Join => {
            let cell = 5.5;
            let size = bitmap_size(&SCREEN_ICON, cell);
            // The play mark bounces while the card is selected.
            let bounce = if motion(&ctx) {
                pose.focus * (t * 4.0).sin().abs() * 4.0
            } else {
                0.0
            };
            bitmap(
                &painter,
                rect.right_top() + vec2(-24.0 - size.x, 22.0 - bounce),
                cell,
                &SCREEN_ICON,
                TEXT,
                TEXT,
            );
        }
        HeroIcon::Host => {
            let cell = 4.4;
            signal(
                &painter,
                rect.right_top() + vec2(-24.0 - 21.0 * cell, 18.0),
                cell,
                t,
                live,
                [TEXT.gamma_multiply(0.45), TEXT],
            );
        }
    }

    paint_job(
        &painter,
        rect.left_bottom() + vec2(20.0, -98.0),
        Align2::LEFT_BOTTOM,
        wordmark_job(title, 64.0, ink),
    );
    painter.text(
        rect.left_bottom() + vec2(22.0, -80.0),
        Align2::LEFT_CENTER,
        subtitle,
        font(17.0),
        dim,
    );
    status_line(
        &painter,
        &ctx,
        rect.left_bottom() + vec2(16.0, -36.0),
        status,
        status_colour,
        pose.focus,
    );
    response
}

// ----------------------------------------------------------- small widgets

/// A button. `selected` fills it: lime, or red for `danger`.
pub fn button(ui: &mut Ui, text: &str, selected: bool, danger: bool) -> Response {
    let label = text.to_uppercase();
    let galley = ui.painter().layout_no_wrap(label.clone(), bold(15.0), TEXT);
    let size = vec2(galley.size().x + 44.0, 46.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let ctx = ui.ctx();
    let hover = slide(ctx, response.id.with("hover"), response.hovered(), 0.1);
    let painter = ui.painter();
    let accent = if danger { ERR } else { LIME };
    if selected {
        // Offset shadow, pressed in on hover.
        let shift = 4.0 * (1.0 - hover);
        painter.rect_filled(rect.translate(vec2(shift, shift)), 4.0, PURPLE);
        let face = rect.translate(vec2(4.0 - shift, 4.0 - shift) * 0.5);
        painter.rect_filled(face, 4.0, accent);
        painter.text(face.center(), Align2::CENTER_CENTER, label, bold(15.0), INK);
    } else {
        painter.rect_filled(rect, 4.0, mix(PANEL, PANEL_HI, hover));
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.5, mix(LINE, LAVENDER, hover)),
            StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            bold(15.0),
            TEXT,
        );
    }
    response
}

/// A small status label for the top bar.
pub fn chip(ui: &mut Ui, text: &str, colour: Color32) {
    chip_live(ui, text, colour, false);
}

/// A chip whose dot pulses while `live`.
pub fn chip_live(ui: &mut Ui, text: &str, colour: Color32, live: bool) {
    let job = caps_job(text, 11.5, colour);
    let galley = ui.painter().layout_job(job);
    let size = vec2(galley.size().x + 32.0, 28.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, PANEL.gamma_multiply(0.9));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let dot = rect.left_center() + vec2(13.0, 0.0);
    if live && motion(ui.ctx()) {
        let pulse = (clock(ui.ctx()) * 2.4).sin() * 0.5 + 0.5;
        painter.rect_filled(
            Rect::from_center_size(dot, Vec2::splat(6.0 + 6.0 * pulse)),
            0.0,
            colour.gamma_multiply(0.3 * (1.0 - pulse)),
        );
    }
    painter.rect_filled(Rect::from_center_size(dot, Vec2::splat(6.0)), 0.0, colour);
    painter.galley(
        Pos2::new(rect.left() + 23.0, rect.center().y - galley.size().y * 0.5),
        galley,
        colour,
    );
}

/// A bar that fills with lime, its leading edge breaking up into a dither.
pub fn progress_bar(ui: &mut Ui, fraction: f32) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().min(560.0), 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, PANEL_HI);
    let cell = 3.5;
    let filled = rect.width() * fraction.clamp(0.0, 1.0);
    painter.rect_filled(
        Rect::from_min_size(rect.min, vec2(filled, rect.height())),
        2.0,
        LIME,
    );
    let rows = (rect.height() / cell) as i32;
    for column in 0..6 {
        let x = rect.left() + filled + column as f32 * cell;
        if x + cell > rect.right() {
            break;
        }
        for row in 0..rows {
            // Fewer cells the further from the edge.
            let keep = (row + column) % (column / 2 + 2) == 0;
            if keep {
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(x, rect.top() + row as f32 * cell),
                        Vec2::splat(cell),
                    ),
                    0.0,
                    LIME,
                );
            }
        }
    }
}


// ------------------------------------------------------------ big widgets

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// Lime: the main thing to do on this screen.
    Primary,
    /// Red: stop or remove.
    Danger,
    /// Outlined: a way elsewhere.
    Plain,
}

/// A large button with a title and a line of explanation, for the few main
/// actions of a screen.
pub fn action_card(
    ui: &mut Ui,
    width: f32,
    title: &str,
    subtitle: &str,
    kind: ActionKind,
    selected: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 76.0), Sense::click());
    let ctx = ui.ctx().clone();
    let focus = slide(&ctx, response.id.with("focus"), selected, 0.16);
    let hover = slide(&ctx, response.id.with("hover"), response.hovered(), 0.1);
    let painter = ui.painter();
    let (fill, ink, sub) = match kind {
        ActionKind::Primary => (LIME, INK, INK.gamma_multiply(0.7)),
        ActionKind::Danger => (ERR, INK, INK.gamma_multiply(0.7)),
        ActionKind::Plain => (mix(PANEL, PANEL_HI, hover), TEXT, DIM),
    };
    if focus > 0.0 {
        set_focus(&ctx, rect.center());
        let shadow = if kind == ActionKind::Plain { LAVENDER } else { PURPLE };
        painter.rect_filled(rect.translate(vec2(5.0, 5.0) * focus), 6.0, shadow.gamma_multiply(focus));
    }
    let face = rect.translate(vec2(-1.0, -1.0) * focus);
    painter.rect_filled(face, 6.0, fill);
    let outline = if kind == ActionKind::Plain {
        mix(LINE, LAVENDER, focus.max(hover))
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_stroke(face, 6.0, Stroke::new(1.5, outline), StrokeKind::Inside);
    painter.text(
        face.left_center() + vec2(22.0, -11.0),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        bold(18.0),
        ink,
    );
    painter.text(
        face.left_center() + vec2(22.0, 13.0),
        Align2::LEFT_CENTER,
        subtitle,
        font(14.0),
        sub,
    );
    arrow(painter, face.right_center() - vec2(24.0, 0.0), 10.0, true, ink.gamma_multiply(0.4 + 0.6 * focus));
    response
}

/// One-time code boxes. `digits` fills them left to right; the next empty
/// box is outlined in lime while `active`. `shake` (0..1, decaying) jiggles
/// the row after a wrong code.
pub fn code_boxes(ui: &mut Ui, digits: &str, len: usize, box_size: Vec2, active: bool, shake: f32) {
    let gap = box_size.x * 0.18;
    let group_gap = box_size.x * 0.35;
    let width = box_size.x * len as f32 + gap * (len - 1) as f32 + group_gap;
    let (rect, _) = ui.allocate_exact_size(vec2(width, box_size.y), Sense::hover());
    let ctx = ui.ctx().clone();
    let offset = if shake > 0.0 {
        (clock(&ctx) * 60.0).sin() * 10.0 * shake
    } else {
        0.0
    };
    let blink = !motion(&ctx) || (clock(&ctx) * 2.0).fract() < 0.6;
    let painter = ui.painter();
    let chars: Vec<char> = digits.chars().collect();
    for index in 0..len {
        let mut x = rect.left() + offset + index as f32 * (box_size.x + gap);
        if index >= len / 2 {
            x += group_gap;
        }
        let cell = Rect::from_min_size(Pos2::new(x, rect.top()), box_size);
        let filled = chars.get(index);
        let current = active && index == chars.len();
        painter.rect_filled(cell, 8.0, if filled.is_some() { PANEL_HI } else { VOID });
        let outline = if current {
            LIME
        } else if filled.is_some() {
            LAVENDER
        } else {
            LINE
        };
        painter.rect_stroke(cell, 8.0, Stroke::new(if current { 2.5 } else { 1.5 }, outline), StrokeKind::Inside);
        if let Some(digit) = filled {
            painter.text(
                cell.center(),
                Align2::CENTER_CENTER,
                digit.to_string(),
                black(box_size.y * 0.55),
                TEXT,
            );
        } else if current && blink {
            let caret = Rect::from_center_size(
                cell.center() + vec2(0.0, box_size.y * 0.28),
                vec2(box_size.x * 0.42, 4.0),
            );
            painter.rect_filled(caret, 1.0, LIME);
        }
    }
}

/// Labels of the code keypad, row by row.
pub const KEYPAD: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "DEL", "0", "OK"];

/// A 3 x 4 number pad for gamepads and mice. Returns the key clicked.
pub fn keypad(ui: &mut Ui, selected: usize, key_size: Vec2) -> Option<usize> {
    let gap = 8.0;
    let mut clicked = None;
    for row in 0..4 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for column in 0..3 {
                let index = row * 3 + column;
                let (rect, response) = ui.allocate_exact_size(key_size, Sense::click());
                let is_selected = index == selected;
                let painter = ui.painter();
                let special = index == 9 || index == 11;
                let fill = if is_selected {
                    if index == 11 { LIME } else { LAVENDER }
                } else if response.hovered() {
                    PANEL_HI
                } else if special {
                    PANEL
                } else {
                    VOID
                };
                painter.rect_filled(rect, 6.0, fill);
                painter.rect_stroke(rect, 6.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
                let ink = if is_selected { INK } else { TEXT };
                let label_font = if special { mono(14.0) } else { bold(22.0) };
                painter.text(rect.center(), Align2::CENTER_CENTER, KEYPAD[index], label_font, ink);
                if response.clicked() {
                    clicked = Some(index);
                }
            }
        });
    }
    clicked
}

/// Moves the keypad selection with the d-pad.
pub fn keypad_move(selected: usize, nav: Nav) -> usize {
    let (row, column) = (selected / 3, selected % 3);
    match nav {
        Nav::Left if column > 0 => selected - 1,
        Nav::Right if column < 2 => selected + 1,
        Nav::Up if row > 0 => selected - 3,
        Nav::Down if row < 3 => selected + 3,
        _ => selected,
    }
}

/// An entry of the settings category list.
pub fn nav_item(ui: &mut Ui, width: f32, index: usize, title: &str, subtitle: &str, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 62.0), Sense::click());
    let ctx = ui.ctx().clone();
    let focus = slide(&ctx, response.id.with("focus"), selected, 0.14);
    let hover = slide(&ctx, response.id.with("hover"), response.hovered(), 0.1);
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, mix(mix(VOID.gamma_multiply(0.0), PANEL, hover), LAVENDER, focus));
    let ink = mix(TEXT, INK, focus);
    let dim = mix(DIM, INK.gamma_multiply(0.7), focus);
    paint_job(
        painter,
        rect.left_center() + vec2(14.0, -10.0),
        Align2::LEFT_CENTER,
        caps_job(&format!("{:02}", index + 1), 11.0, dim),
    );
    painter.text(rect.left_center() + vec2(44.0, -10.0), Align2::LEFT_CENTER, title, bold(17.0), ink);
    let small = font(13.0);
    let line = elide(painter, subtitle, &small, width - 58.0);
    painter.text(rect.left_center() + vec2(44.0, 12.0), Align2::LEFT_CENTER, line, small, dim);
    response
}

/// A small fact: a caps label over a value, on a dark tile.
pub fn fact(ui: &mut Ui, width: f32, label: &str, value: &str, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 62.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, PANEL.gamma_multiply(0.9));
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    paint_job(
        painter,
        rect.left_top() + vec2(14.0, 12.0),
        Align2::LEFT_TOP,
        caps_job(label, 11.0, DIM),
    );
    let value_font = bold(16.0);
    let value = elide(painter, value, &value_font, width - 28.0);
    painter.text(rect.left_bottom() + vec2(14.0, -16.0), Align2::LEFT_CENTER, value, value_font, colour);
}

// ---------------------------------------------------------------- confetti

struct Piece {
    velocity: Vec2,
    colour: Color32,
    size: f32,
}

/// A short burst of square pixels for the good moments (connected, hosting,
/// set up).
#[derive(Default)]
pub struct Confetti {
    origin: Pos2,
    started: f64,
    pieces: Vec<Piece>,
}

impl Confetti {
    const SECONDS: f32 = 1.5;

    pub fn burst(&mut self, ctx: &egui::Context, origin: Pos2) {
        if !motion(ctx) {
            return;
        }
        self.origin = origin;
        self.started = ctx.input(|input| input.time);
        // A tiny generator is plenty for scattering pixels.
        let mut seed = (self.started * 1000.0) as u32 | 1;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10_000) as f32 / 10_000.0
        };
        let colours = [LIME, LAVENDER, LIME, PURPLE, TEXT];
        self.pieces = (0..80)
            .map(|index| {
                let angle = (-170.0 + 160.0 * random()).to_radians();
                let speed = 240.0 + 520.0 * random();
                Piece {
                    velocity: vec2(angle.cos(), angle.sin()) * speed,
                    colour: colours[index % colours.len()],
                    size: 4.0 + 6.0 * random(),
                }
            })
            .collect();
    }

    pub fn paint(&mut self, ctx: &egui::Context) {
        if self.pieces.is_empty() {
            return;
        }
        let age = (ctx.input(|input| input.time) - self.started) as f32;
        if age > Self::SECONDS || !motion(ctx) {
            self.pieces.clear();
            return;
        }
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("confetti")));
        let fade = (1.0 - age / Self::SECONDS).clamp(0.0, 1.0);
        for piece in &self.pieces {
            let position = self.origin + piece.velocity * age + vec2(0.0, 620.0 * age * age * 0.5);
            // Snap to a 4-point grid: the burst reads as pixels.
            let snapped = Pos2::new(
                (position.x / 4.0).round() * 4.0,
                (position.y / 4.0).round() * 4.0,
            );
            painter.rect_filled(
                Rect::from_center_size(snapped, Vec2::splat(piece.size)),
                0.0,
                piece.colour.gamma_multiply(fade),
            );
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_words_are_marked_with_asterisks() {
        let job = headline_job("Let there be *light*.", 40.0, TEXT, LIME);
        let parts: Vec<&str> = job
            .sections
            .iter()
            .map(|section| &job.text[section.byte_range.clone()])
            .collect();
        assert_eq!(parts, ["Let there be ", "light", "."]);
        assert_eq!(job.sections[1].format.color, LIME);
        assert_eq!(
            job.sections[1].format.font_id.family,
            FontFamily::Name(GOTHIC.into())
        );
    }

    #[test]
    fn the_mark_is_a_dithered_g_with_an_extrusion() {
        let cells = mark_cells(0.0);
        for (x, y, _) in &cells {
            assert!((0..MARK_CELLS).contains(x) && (0..MARK_CELLS).contains(y));
        }
        let count = |kind| cells.iter().filter(|cell| cell.2 == kind).count();
        let body: usize = G_GLYPH.iter().map(|row| row.matches('#').count()).sum();
        assert_eq!(count(MarkCell::Primary) + count(MarkCell::Secondary), body);
        // Both body colours appear (the dither), and the extrusion exists.
        assert!(count(MarkCell::Primary) > 0 && count(MarkCell::Secondary) > 0);
        assert!(count(MarkCell::Shadow) > 0);
        // The top row is all the upper colour.
        assert!(cells
            .iter()
            .filter(|c| c.1 == 0)
            .all(|c| c.2 == MarkCell::Primary));
        // The dither line moves over time.
        assert_ne!(mark_cells(0.0), mark_cells(1.4));
    }

    #[test]
    fn bitmaps_are_rectangular() {
        for rows in [
            &SCREEN_ICON[..],
            &PLUS_ICON[..],
            &SEARCH_ICON[..],
            &G_GLYPH[..],
        ] {
            let width = rows[0].len();
            assert!(rows.iter().all(|row| row.len() == width));
        }
        let icon = window_icon();
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        for size in [16, 24, 32, 48, 64, 128, 256] {
            let rgba = icon_rgba(size);
            assert_eq!(rgba.len(), size * size * 4);
            // Black and white only, and the white letter is drawn at every size.
            assert!(rgba
                .chunks_exact(4)
                .filter(|p| p[3] == 255)
                .all(|p| p[0] == p[1] && p[1] == p[2]));
            assert!(rgba.chunks_exact(4).any(|p| p[..3] == [255, 255, 255]));
            // The corners are cut: the top-left pixel is transparent.
            assert_eq!(rgba[3], 0);
        }
    }
}
