//! How the desktop looks, for choosing the app icon: whether the taskbar is light.

/// Whether the taskbar is light (Windows' "Windows mode", which can differ from the apps'
/// mode). `None` where unknown: the value is missing, unreadable, or there is no such setting.
pub fn taskbar_is_light() -> Option<bool> {
    #[cfg(windows)]
    {
        const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
        match crate::windows::get_dword(KEY, "SystemUsesLightTheme") {
            Ok(v) => v.map(|v| v != 0),
            Err(e) => {
                log::debug!("{e}");
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}
