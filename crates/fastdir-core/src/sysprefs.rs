//! Operating-system accessibility preferences: reduced motion, high
//! contrast and the "make text bigger" scale. Non-Windows platforms report
//! the defaults.

/// Accessibility preferences the user set in the OS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccessibilityPrefs {
    /// Animations should be skipped.
    pub reduce_motion: bool,
    /// A high-contrast theme is active.
    pub high_contrast: bool,
    /// Text size multiplier (1.0 = 100%).
    pub text_scale: f32,
}

impl Default for AccessibilityPrefs {
    fn default() -> Self {
        Self {
            reduce_motion: false,
            high_contrast: false,
            text_scale: 1.0,
        }
    }
}

/// The system colours of an active high-contrast theme, as `(r, g, b)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighContrastColors {
    /// Window background.
    pub window: (u8, u8, u8),
    /// Window text.
    pub text: (u8, u8, u8),
    /// Selection background.
    pub highlight: (u8, u8, u8),
    /// Selection text.
    pub highlight_text: (u8, u8, u8),
    /// Disabled / secondary text.
    pub gray_text: (u8, u8, u8),
    /// Hyperlinks.
    pub hotlight: (u8, u8, u8),
}

/// The high-contrast palette, if a high-contrast theme is active.
#[cfg(windows)]
pub fn high_contrast_colors() -> Option<HighContrastColors> {
    use windows_sys::Win32::Graphics::Gdi::GetSysColor;
    if !query().high_contrast {
        return None;
    }
    let rgb = |index: i32| {
        // SAFETY: GetSysColor has no preconditions beyond a valid index.
        let c = unsafe { GetSysColor(index) };
        (
            (c & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            ((c >> 16) & 0xff) as u8,
        )
    };
    // COLOR_WINDOW, COLOR_WINDOWTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
    // COLOR_GRAYTEXT, COLOR_HOTLIGHT.
    Some(HighContrastColors {
        window: rgb(5),
        text: rgb(8),
        highlight: rgb(13),
        highlight_text: rgb(14),
        gray_text: rgb(17),
        hotlight: rgb(26),
    })
}

/// The high-contrast palette, if a high-contrast theme is active.
#[cfg(not(windows))]
pub fn high_contrast_colors() -> Option<HighContrastColors> {
    None
}

/// WCAG relative luminance of an sRGB colour.
pub fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio between two colours (1.0 to 21.0).
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// A text-scale percentage from the OS as a safe multiplier.
pub fn text_scale_from_percent(percent: u32) -> f32 {
    (percent as f32 / 100.0).clamp(1.0, 2.25)
}

/// Read the current preferences.
#[cfg(windows)]
pub fn query() -> AccessibilityPrefs {
    use std::mem::size_of;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SystemParametersInfoW,
    };

    let mut prefs = AccessibilityPrefs::default();
    // SAFETY: each call passes a correctly sized, writable out-parameter.
    unsafe {
        let mut animations: i32 = 1;
        if SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut animations as *mut i32 as *mut _,
            0,
        ) != 0
        {
            prefs.reduce_motion = animations == 0;
        }
        let mut hc: HIGHCONTRASTW = std::mem::zeroed();
        hc.cbSize = size_of::<HIGHCONTRASTW>() as u32;
        if SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            &mut hc as *mut _ as *mut _,
            0,
        ) != 0
        {
            prefs.high_contrast = hc.dwFlags & HCF_HIGHCONTRASTON != 0;
        }
        let subkey: Vec<u16> = r"Software\Microsoft\Accessibility"
            .encode_utf16()
            .chain([0])
            .collect();
        let name: Vec<u16> = "TextScaleFactor\0".encode_utf16().collect();
        let mut value: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut value as *mut u32 as *mut _,
            &mut size,
        );
        if status == 0 && value > 0 {
            prefs.text_scale = text_scale_from_percent(value);
        }
    }
    prefs
}

/// Read the current preferences.
#[cfg(not(windows))]
pub fn query() -> AccessibilityPrefs {
    AccessibilityPrefs::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_scale_is_clamped() {
        assert_eq!(text_scale_from_percent(100), 1.0);
        assert_eq!(text_scale_from_percent(150), 1.5);
        assert_eq!(text_scale_from_percent(50), 1.0);
        assert_eq!(text_scale_from_percent(900), 2.25);
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        assert!((contrast_ratio((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio((255, 255, 255), (255, 255, 255)) - 1.0).abs() < 1e-9);
        // #767676 on white is the classic 4.54:1 AA boundary colour.
        let r = contrast_ratio((0x76, 0x76, 0x76), (255, 255, 255));
        assert!((4.5..4.6).contains(&r), "{r}");
    }

    #[test]
    fn query_does_not_panic() {
        let prefs = query();
        assert!(prefs.text_scale >= 1.0);
    }
}
