//! Turns gamepad and keyboard input into the navigation events the launcher
//! screens understand.

use std::time::{Duration, Instant};

use eframe::egui;
use gilrs::{Axis, Button, EventType, Gilrs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    /// A / Enter
    Accept,
    /// B / Escape
    Back,
    /// X / E or F2
    Edit,
    /// Y / S or F1
    Settings,
    /// Start
    Menu,
    /// LB / RB, PageUp / PageDown
    PrevTab,
    NextTab,
}

const REPEAT_DELAY: Duration = Duration::from_millis(380);
const REPEAT_RATE: Duration = Duration::from_millis(95);
const STICK_THRESHOLD: f32 = 0.6;

#[derive(Default)]
struct Held {
    /// D-pad state.
    dpad: [bool; 4],
    /// Left stick position.
    stick: (f32, f32),
    /// Direction currently repeating and when it fires next.
    repeating: Option<(Nav, Instant)>,
}

impl Held {
    fn direction(&self) -> Option<Nav> {
        const DIRS: [Nav; 4] = [Nav::Up, Nav::Down, Nav::Left, Nav::Right];
        if let Some(i) = self.dpad.iter().position(|d| *d) {
            return Some(DIRS[i]);
        }
        let (x, y) = self.stick;
        if x.abs() < STICK_THRESHOLD && y.abs() < STICK_THRESHOLD {
            None
        } else if x.abs() > y.abs() {
            Some(if x > 0.0 { Nav::Right } else { Nav::Left })
        } else {
            Some(if y > 0.0 { Nav::Up } else { Nav::Down })
        }
    }
}

pub struct Pad {
    gilrs: Option<Gilrs>,
    held: Held,
    /// Name of the most recently used controller, for the status line.
    pub active_name: Option<String>,
}

impl Pad {
    pub fn new() -> Self {
        Self {
            // A machine without any gamepad API still gets keyboard and mouse.
            gilrs: Gilrs::new().ok(),
            held: Held::default(),
            active_name: None,
        }
    }

    pub fn connected(&self) -> usize {
        self.gilrs
            .as_ref()
            .map(|g| g.gamepads().count())
            .unwrap_or(0)
    }

    /// Drains pending gamepad events. With `active` false the events are
    /// discarded, which is what we want while a stream owns the controller.
    pub fn poll(&mut self, active: bool) -> Vec<Nav> {
        let mut out = Vec::new();
        let Some(gilrs) = &mut self.gilrs else {
            return out;
        };
        while let Some(event) = gilrs.next_event() {
            if !active {
                continue;
            }
            match event.event {
                EventType::ButtonPressed(button, _) => {
                    self.active_name = Some(gilrs.gamepad(event.id).name().to_owned());
                    match button {
                        Button::South => out.push(Nav::Accept),
                        Button::East => out.push(Nav::Back),
                        Button::West => out.push(Nav::Edit),
                        Button::North => out.push(Nav::Settings),
                        Button::Start => out.push(Nav::Menu),
                        Button::LeftTrigger => out.push(Nav::PrevTab),
                        Button::RightTrigger => out.push(Nav::NextTab),
                        Button::DPadUp => self.held.dpad[0] = true,
                        Button::DPadDown => self.held.dpad[1] = true,
                        Button::DPadLeft => self.held.dpad[2] = true,
                        Button::DPadRight => self.held.dpad[3] = true,
                        _ => {}
                    }
                }
                EventType::ButtonReleased(button, _) => match button {
                    Button::DPadUp => self.held.dpad[0] = false,
                    Button::DPadDown => self.held.dpad[1] = false,
                    Button::DPadLeft => self.held.dpad[2] = false,
                    Button::DPadRight => self.held.dpad[3] = false,
                    _ => {}
                },
                EventType::AxisChanged(Axis::LeftStickX, value, _) => self.held.stick.0 = value,
                EventType::AxisChanged(Axis::LeftStickY, value, _) => self.held.stick.1 = value,
                EventType::Disconnected => self.held = Held::default(),
                _ => {}
            }
        }

        if !active {
            self.held = Held::default();
            return out;
        }

        // Emit the held direction once, then auto-repeat.
        let now = Instant::now();
        match (self.held.direction(), self.held.repeating) {
            (None, _) => self.held.repeating = None,
            (Some(dir), Some((current, next))) if dir == current => {
                if now >= next {
                    out.push(dir);
                    self.held.repeating = Some((dir, now + REPEAT_RATE));
                }
            }
            (Some(dir), _) => {
                out.push(dir);
                self.held.repeating = Some((dir, now + REPEAT_DELAY));
            }
        }
        out
    }
}

/// Keyboard equivalents of the gamepad controls. `typing` is true while a
/// text field has focus: then only Enter and Escape are taken, so letters and
/// arrow keys keep working inside the field.
pub fn keyboard(ctx: &egui::Context, typing: bool) -> Vec<Nav> {
    use egui::Key;
    let mut out = Vec::new();
    ctx.input(|input| {
        for event in &input.events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                ..
            } = event
            else {
                continue;
            };
            // Moving may auto-repeat while a key is held. Actions may not:
            // a held Enter must not connect, fail and connect again.
            let nav = match key {
                Key::Enter if !repeat => Nav::Accept,
                Key::Escape if !repeat => Nav::Back,
                _ if typing => continue,
                Key::ArrowUp => Nav::Up,
                Key::ArrowDown => Nav::Down,
                Key::ArrowLeft => Nav::Left,
                Key::ArrowRight => Nav::Right,
                Key::Space if !repeat => Nav::Accept,
                Key::E | Key::F2 if !repeat => Nav::Edit,
                Key::S | Key::F1 if !repeat => Nav::Settings,
                Key::PageUp if !repeat => Nav::PrevTab,
                Key::PageDown if !repeat => Nav::NextTab,
                _ => continue,
            };
            out.push(nav);
        }
    });
    out
}
