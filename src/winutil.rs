//! Platform helpers used while a stream is running.
//!
//! * `FullscreenRequest`: kyclient has no fullscreen flag, only the
//!   Ctrl+Alt+F shortcut, so the launcher presses it once the client window
//!   is up.
//! * `QuitCombo`: Start + Back + LB + RB held on any XInput pad ends the
//!   stream, so a session can be left without reaching for a keyboard.
//!
//! Both are Windows-only; other platforms get inert stand-ins.

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    };
    use windows_sys::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_STATE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId,
        IsWindowVisible, SetForegroundWindow, SetWindowPos, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN,
        SWP_SHOWWINDOW,
    };

    /// How long the client window must have been visible before the shortcut
    /// is sent, so its input handling is up.
    const SETTLE: Duration = Duration::from_millis(1500);
    /// Give up if no window appears (failed connection, sign-in in a browser...).
    const GIVE_UP: Duration = Duration::from_secs(60);

    const SC_LCTRL: u16 = 0x1D;
    const SC_LALT: u16 = 0x38;
    const SC_F: u16 = 0x21;

    struct Search {
        pid: u32,
        found: HWND,
    }

    unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the `Search` passed to EnumWindows below, which
        // outlives the enumeration.
        let search = &mut *(lparam as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == search.pid && IsWindowVisible(hwnd) != 0 {
            search.found = hwnd;
            return 0; // stop enumerating
        }
        1
    }

    fn visible_window_of(pid: u32) -> Option<HWND> {
        let mut search = Search {
            pid,
            found: std::ptr::null_mut(),
        };
        // SAFETY: the callback only runs during this call and `search` lives
        // on this stack frame for its whole duration.
        unsafe { EnumWindows(Some(on_window), &mut search as *mut Search as LPARAM) };
        (!search.found.is_null()).then_some(search.found)
    }

    /// Puts a lab client's window on the right half of the primary screen,
    /// on top, so the beacon at the top-left stays visible both in the
    /// original and inside the stream.
    pub fn place_lab_window(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(hwnd) = visible_window_of(pid) {
                // SAFETY: plain Win32 calls on a window we just enumerated.
                unsafe {
                    let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
                    let width = w * 48 / 100;
                    let height = width * h / w.max(1) + 40;
                    return SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        w / 2,
                        h / 8,
                        width,
                        height,
                        SWP_SHOWWINDOW,
                    ) != 0;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn key(scancode: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: scancode,
                    dwFlags: KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    pub struct FullscreenRequest {
        pid: u32,
        started: Instant,
        window_seen: Option<Instant>,
    }

    impl FullscreenRequest {
        pub fn new(pid: u32) -> Self {
            Self {
                pid,
                started: Instant::now(),
                window_seen: None,
            }
        }

        /// Returns true once the request is finished (sent or abandoned).
        pub fn poll(&mut self) -> bool {
            if self.started.elapsed() > GIVE_UP {
                return true;
            }
            let Some(hwnd) = visible_window_of(self.pid) else {
                self.window_seen = None;
                return false;
            };
            let seen = *self.window_seen.get_or_insert_with(Instant::now);
            if seen.elapsed() < SETTLE {
                return false;
            }
            // SAFETY: plain Win32 calls on a window handle we just enumerated;
            // a stale handle makes them fail harmlessly.
            unsafe {
                SetForegroundWindow(hwnd);
                // Never type the shortcut into some other application.
                if GetForegroundWindow() != hwnd {
                    return false;
                }
                let inputs = [
                    key(SC_LCTRL, false),
                    key(SC_LALT, false),
                    key(SC_F, false),
                    key(SC_F, true),
                    key(SC_LALT, true),
                    key(SC_LCTRL, true),
                ];
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
            true
        }
    }

    const HOLD: Duration = Duration::from_millis(1000);
    // XINPUT_GAMEPAD_START | BACK | LEFT_SHOULDER | RIGHT_SHOULDER
    const COMBO: u16 = 0x0010 | 0x0020 | 0x0100 | 0x0200;

    #[derive(Default)]
    pub struct QuitCombo {
        held_since: Option<Instant>,
    }

    impl QuitCombo {
        pub fn held_long_enough(&mut self) -> bool {
            let pressed = (0..4).any(|slot| {
                // SAFETY: XINPUT_STATE is plain data and XInputGetState only
                // writes into the struct we hand it.
                let mut state: XINPUT_STATE = unsafe { std::mem::zeroed() };
                let connected = unsafe { XInputGetState(slot, &mut state) } == 0;
                connected && (state.Gamepad.wButtons as u16) & COMBO == COMBO
            });
            if !pressed {
                self.held_since = None;
                return false;
            }
            self.held_since.get_or_insert_with(Instant::now).elapsed() >= HOLD
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct FullscreenRequest;

    impl FullscreenRequest {
        pub fn new(_pid: u32) -> Self {
            Self
        }

        pub fn poll(&mut self) -> bool {
            true
        }
    }

    pub fn place_lab_window(_pid: u32) -> bool {
        false
    }

    #[derive(Default)]
    pub struct QuitCombo;

    impl QuitCombo {
        pub fn held_long_enough(&mut self) -> bool {
            false
        }
    }
}

pub use imp::{place_lab_window, FullscreenRequest, QuitCombo};
