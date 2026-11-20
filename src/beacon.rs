//! The latency beacon: a strip that shows the current time as a barcode,
//! and a reader that finds such strips in a screenshot.
//!
//! When the beacon is on screen and a streaming client shows the same
//! screen, one screenshot holds the beacon twice: the original, and the
//! copy inside the stream window, which is older by exactly the stream's
//! latency. Reading both from the same screenshot gives that latency
//! without any clock synchronisation, for any streaming app.
//!
//! Layout, left to right: a lime marker, 20 cells, a lime marker. The
//! cells are two sync bits (1, 0), 16 bits of the time in milliseconds as a
//! Gray code (a frame caught mid-change reads as one of the two values, not
//! garbage), and two sync bits (0, 1). White is 1, black is 0.

use eframe::egui::{vec2, Color32, Painter, Pos2, Rect};

pub const HEIGHT: f32 = 64.0;
pub const MARKER_W: f32 = 48.0;
pub const CELL_W: f32 = 24.0;
pub const CELLS: usize = 20;
pub const WIDTH: f32 = MARKER_W * 2.0 + CELL_W * CELLS as f32;
const LIME: Color32 = Color32::from_rgb(0xC8, 0xFF, 0x3D);

pub fn to_gray(value: u16) -> u16 {
    value ^ (value >> 1)
}

pub fn from_gray(mut gray: u16) -> u16 {
    let mut value = gray;
    while gray > 0 {
        gray >>= 1;
        value ^= gray;
    }
    value
}

/// The 20 cells for `value`.
pub fn cells(value: u16) -> [bool; CELLS] {
    let gray = to_gray(value);
    let mut out = [false; CELLS];
    out[0] = true;
    out[1] = false;
    for bit in 0..16 {
        out[2 + bit] = gray & (1 << (15 - bit)) != 0;
    }
    out[18] = false;
    out[19] = true;
    out
}

/// Paints the beacon for `value` with its top-left at `origin`.
pub fn paint(painter: &Painter, origin: Pos2, value: u16) {
    let rect = Rect::from_min_size(origin, vec2(WIDTH, HEIGHT));
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    painter.rect_filled(
        Rect::from_min_size(origin, vec2(MARKER_W, HEIGHT)),
        0.0,
        LIME,
    );
    painter.rect_filled(
        Rect::from_min_size(origin + vec2(WIDTH - MARKER_W, 0.0), vec2(MARKER_W, HEIGHT)),
        0.0,
        LIME,
    );
    for (index, on) in cells(value).iter().enumerate() {
        if *on {
            let min = origin + vec2(MARKER_W + index as f32 * CELL_W, 0.0);
            painter.rect_filled(
                Rect::from_min_size(min, vec2(CELL_W, HEIGHT)),
                0.0,
                Color32::WHITE,
            );
        }
    }
}

// ------------------------------------------------------------------ reader

/// A screenshot: BGRA pixels, top row first.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

impl Frame {
    fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.width + x) * 4;
        (self.bgra[i + 2], self.bgra[i + 1], self.bgra[i])
    }

    fn luma(&self, x: usize, y: usize) -> f32 {
        let (r, g, b) = self.pixel(x, y);
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    }
}

/// One beacon found in a screenshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub value: u16,
    /// Marker height in pixels: the original is the largest.
    pub height: f32,
    pub x: f32,
    pub y: f32,
}

fn is_lime(r: u8, g: u8, b: u8) -> bool {
    // Lime survives video compression as "very green, some red, little blue".
    g >= 185 && (120..=245).contains(&r) && b <= 140 && g as i32 - b as i32 >= 90
}

#[derive(Debug)]
struct Blob {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    count: usize,
}

impl Blob {
    fn cx(&self) -> f32 {
        (self.min_x + self.max_x) as f32 * 0.5
    }
    fn cy(&self) -> f32 {
        (self.min_y + self.max_y) as f32 * 0.5
    }
    fn w(&self) -> f32 {
        (self.max_x - self.min_x + STEP) as f32
    }
    fn h(&self) -> f32 {
        (self.max_y - self.min_y + STEP) as f32
    }
}

/// Grid step for the lime search, in pixels.
const STEP: usize = 2;

fn lime_blobs(frame: &Frame) -> Vec<Blob> {
    let gw = frame.width / STEP;
    let gh = frame.height / STEP;
    let mut lime = vec![false; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (r, g, b) = frame.pixel(gx * STEP, gy * STEP);
            lime[gy * gw + gx] = is_lime(r, g, b);
        }
    }
    let mut seen = vec![false; gw * gh];
    let mut blobs = Vec::new();
    let mut stack = Vec::new();
    for start in 0..lime.len() {
        if !lime[start] || seen[start] {
            continue;
        }
        let mut blob = Blob {
            min_x: usize::MAX,
            min_y: usize::MAX,
            max_x: 0,
            max_y: 0,
            count: 0,
        };
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (gx, gy) = (i % gw, i / gw);
            blob.count += 1;
            blob.min_x = blob.min_x.min(gx * STEP);
            blob.max_x = blob.max_x.max(gx * STEP);
            blob.min_y = blob.min_y.min(gy * STEP);
            blob.max_y = blob.max_y.max(gy * STEP);
            let mut visit = |j: usize| {
                if lime[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if gx > 0 {
                visit(i - 1);
            }
            if gx + 1 < gw {
                visit(i + 1);
            }
            if gy > 0 {
                visit(i - gw);
            }
            if gy + 1 < gh {
                visit(i + gw);
            }
        }
        // Solid, marker-shaped and big enough to carry readable cells.
        let area = (blob.w() / STEP as f32) * (blob.h() / STEP as f32);
        let solid = blob.count as f32 / area.max(1.0) > 0.7;
        let aspect = blob.w() / blob.h();
        let shaped = (0.45..=1.1).contains(&aspect);
        if solid && shaped && blob.h() >= 16.0 {
            blobs.push(blob);
        }
    }
    blobs
}

/// Every beacon visible in `frame`.
pub fn read(frame: &Frame) -> Vec<Reading> {
    let blobs = lime_blobs(frame);
    let span = (WIDTH - MARKER_W) / HEIGHT; // centre-to-centre distance / height
    let mut readings = Vec::new();
    for left in &blobs {
        for right in &blobs {
            if right.cx() <= left.cx() {
                continue;
            }
            let h = (left.h() + right.h()) * 0.5;
            let similar = (left.h() / right.h()).clamp(0.0, 10.0);
            if !(0.8..=1.25).contains(&similar) || (left.cy() - right.cy()).abs() > h * 0.25 {
                continue;
            }
            let ratio = (right.cx() - left.cx()) / h;
            if (ratio - span).abs() > span * 0.1 {
                continue;
            }
            if let Some(value) = decode(frame, left, right) {
                readings.push(Reading {
                    value,
                    height: h,
                    x: left.cx(),
                    y: left.cy(),
                });
            }
        }
    }
    readings
}

fn decode(frame: &Frame, left: &Blob, right: &Blob) -> Option<u16> {
    let (x0, y0) = (left.cx(), left.cy());
    let (x1, y1) = (right.cx(), right.cy());
    let distance = WIDTH - MARKER_W;
    let cell_px = (x1 - x0) * CELL_W / distance;
    let half = (cell_px * 0.18).max(1.0) as isize;
    let mut levels = [0f32; CELLS];
    for (index, level) in levels.iter_mut().enumerate() {
        let t = (MARKER_W * 0.5 + CELL_W * (index as f32 + 0.5)) / distance;
        let cx = (x0 + (x1 - x0) * t) as isize;
        let cy = (y0 + (y1 - y0) * t) as isize;
        let (mut sum, mut count) = (0.0f32, 0.0f32);
        for dy in -half..=half {
            for dx in -half..=half {
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && (x as usize) < frame.width && (y as usize) < frame.height {
                    sum += frame.luma(x as usize, y as usize);
                    count += 1.0;
                }
            }
        }
        *level = sum / count.max(1.0);
    }
    let lo = levels.iter().cloned().fold(f32::MAX, f32::min);
    let hi = levels.iter().cloned().fold(f32::MIN, f32::max);
    if hi - lo < 60.0 {
        return None;
    }
    let threshold = (lo + hi) * 0.5;
    let bits: Vec<bool> = levels.iter().map(|l| *l > threshold).collect();
    if !(bits[0] && !bits[1] && !bits[18] && bits[19]) {
        return None;
    }
    let gray = bits[2..18]
        .iter()
        .fold(0u16, |acc, bit| (acc << 1) | u16::from(*bit));
    Some(from_gray(gray))
}

/// Latency in milliseconds from one screenshot: the original (largest)
/// beacon minus the first copy. `None` when there are not two beacons or
/// the difference is implausible.
pub fn latency(readings: &[Reading]) -> Option<f32> {
    let mut sorted = readings.to_vec();
    sorted.sort_by(|a, b| b.height.total_cmp(&a.height));
    let original = sorted.first()?;
    // The first copy is the largest one that is clearly smaller.
    let copy = sorted.iter().find(|r| r.height < original.height * 0.95)?;
    let ms = original.value.wrapping_sub(copy.value);
    (ms < 1000).then_some(ms as f32)
}

// --------------------------------------------------------------- capture

/// Screenshot of the primary display.
#[cfg(windows)]
pub fn capture_screen() -> Option<Frame> {
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    // SAFETY: plain GDI calls; every handle created here is released below,
    // and the pixel buffer is sized for the requested 32-bit bitmap.
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            return None;
        }
        let screen = GetDC(std::ptr::null_mut());
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let previous = SelectObject(memory, bitmap);
        let copied = BitBlt(
            memory,
            0,
            0,
            width,
            height,
            screen,
            0,
            0,
            SRCCOPY | CAPTUREBLT,
        ) != 0;
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top row first
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        SelectObject(memory, previous);
        let lines = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(std::ptr::null_mut(), screen);
        (copied && lines == height).then_some(Frame {
            width: width as usize,
            height: height as usize,
            bgra,
        })
    }
}

#[cfg(not(windows))]
pub fn capture_screen() -> Option<Frame> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_code_round_trips() {
        for value in [0u16, 1, 2, 3, 1000, 32767, 32768, 65535] {
            assert_eq!(from_gray(to_gray(value)), value);
        }
        // Neighbours differ by exactly one bit.
        for value in 0u16..2000 {
            assert_eq!((to_gray(value) ^ to_gray(value + 1)).count_ones(), 1);
        }
    }

    /// Draws a beacon into a test frame, scaled, like a stream window would.
    fn draw(frame: &mut Frame, x: f32, y: f32, scale: f32, value: u16) {
        let lime = (0xC8, 0xFF, 0x3D);
        let mut fill = |x0: f32, y0: f32, w: f32, h: f32, rgb: (u8, u8, u8)| {
            for py in y0 as usize..(y0 + h) as usize {
                for px in x0 as usize..(x0 + w) as usize {
                    let i = (py * frame.width + px) * 4;
                    frame.bgra[i..i + 4].copy_from_slice(&[rgb.2, rgb.1, rgb.0, 255]);
                }
            }
        };
        let (h, m, c) = (HEIGHT * scale, MARKER_W * scale, CELL_W * scale);
        fill(x, y, WIDTH * scale, h, (0, 0, 0));
        fill(x, y, m, h, lime);
        fill(x + m + c * CELLS as f32, y, m, h, lime);
        for (index, on) in cells(value).iter().enumerate() {
            if *on {
                fill(x + m + c * index as f32, y, c, h, (255, 255, 255));
            }
        }
    }

    fn blank(width: usize, height: usize) -> Frame {
        // A mid-grey desktop.
        let mut bgra = vec![60u8; width * height * 4];
        for pixel in bgra.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Frame {
            width,
            height,
            bgra,
        }
    }

    #[test]
    fn the_original_and_a_scaled_copy_are_both_read() {
        let mut frame = blank(1600, 900);
        draw(&mut frame, 20.0, 20.0, 1.0, 51234);
        draw(&mut frame, 820.0, 400.0, 0.48, 51226);
        // A lime button elsewhere must not be mistaken for a beacon.
        let mut fill_lime = |x: usize, y: usize| {
            for py in y..y + 40 {
                for px in x..x + 160 {
                    let i = (py * frame.width + px) * 4;
                    frame.bgra[i..i + 4].copy_from_slice(&[0x3D, 0xFF, 0xC8, 255]);
                }
            }
        };
        fill_lime(300, 700);

        let readings = read(&frame);
        let mut values: Vec<u16> = readings.iter().map(|r| r.value).collect();
        values.sort();
        assert_eq!(values, [51226, 51234], "{readings:?}");
        assert_eq!(latency(&readings), Some(8.0));
    }

    #[test]
    fn the_time_wraps_around_cleanly() {
        let readings = [
            Reading {
                value: 3,
                height: 64.0,
                x: 0.0,
                y: 0.0,
            },
            Reading {
                value: 65530,
                height: 30.0,
                x: 0.0,
                y: 0.0,
            },
        ];
        assert_eq!(latency(&readings), Some(9.0));
        // A single beacon, or an implausible gap, gives nothing.
        assert_eq!(latency(&readings[..1]), None);
        let far = [
            readings[0],
            Reading {
                value: 50000,
                ..readings[1]
            },
        ];
        assert_eq!(latency(&far), None);
    }
}
