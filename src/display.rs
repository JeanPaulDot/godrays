//! The hosting PC's screen resolution.
//!
//! Kyber streams the host's display as it is; it has no option to scale the
//! picture. So "stream at 1080p" means running the host's primary display
//! at 1080p while hosting, and putting it back afterwards.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl Mode {
    pub fn label(&self) -> String {
        format!("{} \u{d7} {} at {} Hz", self.width, self.height, self.hz)
    }
}

/// Reduces every mode the display reports to one entry per resolution, at
/// its highest refresh rate, largest first. Resolutions too small to be
/// useful for streaming a desktop are left out.
pub fn pick_modes(all: &[Mode]) -> Vec<Mode> {
    let mut best: Vec<Mode> = Vec::new();
    for mode in all.iter().filter(|m| m.width >= 1280 && m.height >= 720) {
        match best
            .iter_mut()
            .find(|b| b.width == mode.width && b.height == mode.height)
        {
            Some(existing) => existing.hz = existing.hz.max(mode.hz),
            None => best.push(*mode),
        }
    }
    best.sort_by_key(|m| std::cmp::Reverse((m.width * m.height, m.width)));
    best
}

/// The mode to host with: the chosen size (or the current one), at the
/// highest refresh rate not above `max_hz` (0: no cap). When every rate the
/// display offers for that size is above the cap, its lowest one. None when
/// there is nothing to choose from.
pub fn hosting_mode(
    all: &[Mode],
    current: Option<Mode>,
    size: Option<(u32, u32)>,
    max_hz: u32,
) -> Option<Mode> {
    let (width, height) = size.or(current.map(|m| (m.width, m.height)))?;
    let rates: Vec<u32> = all
        .iter()
        .filter(|m| m.width == width && m.height == height)
        .map(|m| m.hz)
        .collect();
    let hz = if max_hz == 0 {
        rates.iter().copied().max()?
    } else {
        rates
            .iter()
            .copied()
            .filter(|&hz| hz <= max_hz)
            .max()
            .or_else(|| rates.iter().copied().min())?
    };
    Some(Mode { width, height, hz })
}

#[cfg(windows)]
mod imp {
    use super::Mode;
    use windows_sys::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_FULLSCREEN, DEVMODEW,
        DISP_CHANGE_SUCCESSFUL, ENUM_CURRENT_SETTINGS,
    };

    fn read(index: u32) -> Option<DEVMODEW> {
        // SAFETY: DEVMODEW is plain data; dmSize tells Windows how much of it
        // may be written.
        let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
        mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        let found = unsafe { EnumDisplaySettingsW(std::ptr::null(), index, &mut mode) } != 0;
        found.then_some(mode)
    }

    fn describe(mode: &DEVMODEW) -> Mode {
        Mode {
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
        }
    }

    /// The primary display's current mode.
    pub fn current() -> Option<Mode> {
        read(ENUM_CURRENT_SETTINGS).as_ref().map(describe)
    }

    /// Every mode the primary display supports.
    pub fn all_modes() -> Vec<Mode> {
        (0..)
            .map_while(read)
            .filter(|mode| mode.dmBitsPerPel == 32)
            .map(|mode| describe(&mode))
            .collect()
    }

    /// Switches the primary display for as long as the launcher runs. The
    /// change is not saved: Windows goes back to the user's own setting if
    /// the launcher ends without restoring it.
    pub fn set(wanted: Mode) -> Result<(), String> {
        let mode = (0..)
            .map_while(read)
            .find(|mode| mode.dmBitsPerPel == 32 && describe(mode) == wanted)
            .ok_or_else(|| format!("This display does not offer {}.", wanted.label()))?;
        // SAFETY: `mode` came from EnumDisplaySettingsW and is passed back
        // unchanged.
        let result = unsafe {
            ChangeDisplaySettingsExW(
                std::ptr::null(),
                &mode,
                std::ptr::null_mut(),
                CDS_FULLSCREEN,
                std::ptr::null(),
            )
        };
        if result == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!(
                "Windows refused to switch the display to {} (code {result}).",
                wanted.label()
            ))
        }
    }

    /// Returns the display to the user's saved setting.
    pub fn restore() {
        // SAFETY: a null mode asks Windows to reapply the registry settings.
        unsafe {
            ChangeDisplaySettingsExW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Mode;

    pub fn current() -> Option<Mode> {
        None
    }

    pub fn all_modes() -> Vec<Mode> {
        Vec::new()
    }

    pub fn set(_wanted: Mode) -> Result<(), String> {
        Err("Changing the resolution is only available on Windows.".to_owned())
    }

    pub fn restore() {}
}

pub use imp::{all_modes, current, restore, set};

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> Mode {
        Mode { width, height, hz }
    }

    #[test]
    fn hosting_mode_caps_the_refresh_rate() {
        let all = [
            mode(1920, 1080, 60),
            mode(1920, 1080, 120),
            mode(1920, 1080, 165),
            mode(1920, 1080, 59),
            mode(2560, 1440, 144),
        ];
        let now = Some(mode(1920, 1080, 165));
        assert_eq!(hosting_mode(&all, now, None, 60), Some(mode(1920, 1080, 60)));
        assert_eq!(hosting_mode(&all, now, None, 144), Some(mode(1920, 1080, 120)));
        assert_eq!(hosting_mode(&all, now, None, 0), Some(mode(1920, 1080, 165)));
        // Only rates above the cap: the lowest one.
        assert_eq!(
            hosting_mode(&all, now, Some((2560, 1440)), 60),
            Some(mode(2560, 1440, 144))
        );
        assert_eq!(hosting_mode(&all, None, None, 60), None);
        assert_eq!(hosting_mode(&all, now, Some((800, 600)), 60), None);
    }

    #[test]
    fn one_entry_per_resolution_at_its_best_refresh_rate() {
        let all = [
            mode(1920, 1080, 60),
            mode(2560, 1440, 60),
            mode(1920, 1080, 144),
            mode(800, 600, 60),
            mode(2560, 1440, 165),
            mode(1280, 720, 60),
            mode(1920, 1080, 120),
        ];
        assert_eq!(
            pick_modes(&all),
            [
                mode(2560, 1440, 165),
                mode(1920, 1080, 144),
                mode(1280, 720, 60)
            ]
        );
        assert!(pick_modes(&[]).is_empty());
    }

    /// Reads the real display; changes nothing.
    #[cfg(windows)]
    #[test]
    fn the_current_mode_is_among_the_modes_the_display_reports() {
        let current = current().expect("a primary display");
        assert!(current.width > 0 && current.height > 0);
        assert!(all_modes()
            .iter()
            .any(|m| m.width == current.width && m.height == current.height));
    }
}
