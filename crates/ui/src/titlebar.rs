//! The native title bar's colour, so Claude's blue mode reaches it too. Windows draws the title
//! bar itself; on Windows 11 its colours can be set per window through DWM
//! (`DWMWA_CAPTION_COLOR`, `DWMWA_TEXT_COLOR`). Elsewhere, and on Windows 10 where DWM refuses
//! the attribute, this does nothing.
//!
//! FFI module: the only place in this crate with `unsafe`.

use egui::Color32;

/// Sets the caption and caption text colour of the window behind `handle`. Returns whether the
/// system took it (for the log; failing is harmless).
pub fn set_colors(handle: &impl raw_window_handle::HasWindowHandle, caption: Color32, text: Color32) -> bool {
    let Ok(handle) = handle.window_handle() else { return false };
    platform::set(handle.as_raw(), caption, text)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use egui::Color32;
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute};

    /// A COLORREF: 0x00BBGGRR.
    fn colorref(c: Color32) -> u32 {
        u32::from(c.r()) | (u32::from(c.g()) << 8) | (u32::from(c.b()) << 16)
    }

    pub fn set(handle: RawWindowHandle, caption: Color32, text: Color32) -> bool {
        let RawWindowHandle::Win32(h) = handle else { return false };
        let hwnd = h.hwnd.get() as *mut core::ffi::c_void;
        let mut ok = true;
        for (attribute, color) in [(DWMWA_CAPTION_COLOR, caption), (DWMWA_TEXT_COLOR, text)] {
            let value = colorref(color);
            // SAFETY: `hwnd` is the live window eframe handed out for this frame; the pointer
            // and size describe a u32 on the stack that outlives the call; DWM only reads it.
            let result = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute as u32,
                    (&raw const value).cast(),
                    size_of::<u32>() as u32,
                )
            };
            ok &= result >= 0;
        }
        ok
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn set(_: raw_window_handle::RawWindowHandle, _: egui::Color32, _: egui::Color32) -> bool {
        false
    }
}
