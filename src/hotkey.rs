//! Ctrl+Alt+Shift+Q ends the running stream; Ctrl+Alt+Shift+S shows or
//! hides the latency statistics over it.
//!
//! While a stream has focus, the Kyber client swallows every key press
//! before Windows hotkeys can see it. Raw keyboard input is delivered
//! regardless, so the launcher listens for the shortcut there, on a hidden
//! message window with its own thread. Windows only; elsewhere the shortcut
//! is simply never reported.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combo {
    Quit,
    Stats,
}

/// Tracks the modifier keys and reports when a shortcut is pressed.
#[derive(Default)]
pub struct Shortcut {
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Shortcut {
    /// Feeds one key event (Windows virtual-key code). Returns the shortcut
    /// it completes, if any.
    pub fn on_key(&mut self, virtual_key: u16, down: bool) -> Option<Combo> {
        let held = down && self.ctrl && self.alt && self.shift;
        match virtual_key {
            0x11 | 0xA2 | 0xA3 => self.ctrl = down,
            0x12 | 0xA4 | 0xA5 => self.alt = down,
            0x10 | 0xA0 | 0xA1 => self.shift = down,
            0x51 if held => return Some(Combo::Quit),
            0x53 if held => return Some(Combo::Stats),
            _ => {}
        }
        None
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, Once};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
        RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        HWND_MESSAGE, MSG, WM_INPUT, WNDCLASSW,
    };

    use super::{Combo, Shortcut};

    static PRESSED: AtomicBool = AtomicBool::new(false);
    static STATS: AtomicBool = AtomicBool::new(false);
    static STATE: Mutex<Shortcut> = Mutex::new(Shortcut {
        ctrl: false,
        alt: false,
        shift: false,
    });
    static START: Once = Once::new();

    /// Key-up flag of RAWKEYBOARD::Flags.
    const RI_KEY_BREAK: u16 = 1;

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_INPUT {
            // SAFETY: RAWINPUT is plain data, large enough for any keyboard
            // event; Windows writes at most `size` bytes into it.
            let mut raw: RAWINPUT = std::mem::zeroed();
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            let read = GetRawInputData(
                lparam as HRAWINPUT,
                RID_INPUT,
                &mut raw as *mut RAWINPUT as *mut c_void,
                &mut size,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            );
            if read != u32::MAX && raw.header.dwType == RIM_TYPEKEYBOARD {
                let key = raw.data.keyboard;
                let down = key.Flags & RI_KEY_BREAK == 0;
                match STATE.lock().unwrap().on_key(key.VKey, down) {
                    Some(Combo::Quit) => PRESSED.store(true, Ordering::SeqCst),
                    Some(Combo::Stats) => STATS.store(true, Ordering::SeqCst),
                    None => {}
                }
            }
        }
        DefWindowProcW(window, message, wparam, lparam)
    }

    fn listen() {
        let class_name: Vec<u16> = "GodraysHotkey\0".encode_utf16().collect();
        // SAFETY: standard message-only window setup; every pointer handed
        // to Windows outlives the call it is used in, and the message loop
        // runs on the thread that created the window.
        unsafe {
            let mut class: WNDCLASSW = std::mem::zeroed();
            class.lpfnWndProc = Some(window_proc);
            class.lpszClassName = class_name.as_ptr();
            RegisterClassW(&class);
            let window = CreateWindowExW(
                0,
                class_name.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if window.is_null() {
                return;
            }
            // Generic desktop page, keyboard usage; INPUTSINK delivers the
            // events even while another window has focus.
            let device = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: window,
            };
            if RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
                == 0
            {
                return;
            }
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&message);
            }
        }
    }

    /// Starts listening (once) and forgets any earlier press.
    pub fn arm() {
        START.call_once(|| {
            std::thread::spawn(listen);
        });
        PRESSED.store(false, Ordering::SeqCst);
    }

    /// True once after the shortcut was pressed since the last call.
    pub fn quit_pressed() -> bool {
        PRESSED.swap(false, Ordering::SeqCst)
    }

    /// True once after Ctrl+Alt+Shift+S was pressed since the last call.
    pub fn stats_pressed() -> bool {
        STATS.swap(false, Ordering::SeqCst)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn arm() {}

    pub fn quit_pressed() -> bool {
        false
    }

    pub fn stats_pressed() -> bool {
        false
    }
}

pub use imp::{arm, quit_pressed, stats_pressed};

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: u16 = 0x11;
    const ALT: u16 = 0x12;
    const SHIFT: u16 = 0x10;
    const Q: u16 = 0x51;
    const S: u16 = 0x53;

    #[test]
    fn only_the_full_combination_triggers() {
        let mut shortcut = Shortcut::default();
        assert_eq!(shortcut.on_key(Q, true), None);
        shortcut.on_key(CTRL, true);
        shortcut.on_key(ALT, true);
        assert_eq!(shortcut.on_key(Q, true), None, "shift is missing");
        shortcut.on_key(SHIFT, true);
        assert_eq!(
            shortcut.on_key(Q, false),
            None,
            "key release must not trigger"
        );
        assert_eq!(shortcut.on_key(Q, true), Some(Combo::Quit));
        assert_eq!(shortcut.on_key(S, true), Some(Combo::Stats));
        // Releasing a modifier disarms it again.
        shortcut.on_key(ALT, false);
        assert_eq!(shortcut.on_key(Q, true), None);
    }

    #[test]
    fn left_and_right_modifier_keys_both_count() {
        let mut shortcut = Shortcut::default();
        shortcut.on_key(0xA3, true); // right ctrl
        shortcut.on_key(0xA4, true); // left alt
        shortcut.on_key(0xA1, true); // right shift
        assert_eq!(shortcut.on_key(Q, true), Some(Combo::Quit));
    }
}
