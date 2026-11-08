//! Choosing what to capture: a monitor by number or name, a window by a part of its title.
//! Pure, so the rules are tested on every platform.

use std::time::Duration;

use penumbra_system::{DisplayDetails, resolve};

/// A monitor as the desktop lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// The OS name (`\\.\DISPLAY2`).
    pub os_name: String,
    pub primary: bool,
    pub width: u32,
    pub height: u32,
}

/// The number in an OS name (`\\.\DISPLAY12` is 12), the order Windows' display settings use.
pub fn display_number(os_name: &str) -> Option<u32> {
    let digits: String = os_name.chars().rev().take_while(char::is_ascii_digit).collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// The order `display` numbers monitors in: by the number in their OS name, then by name.
pub fn monitor_order(a: &Monitor, b: &Monitor) -> std::cmp::Ordering {
    display_number(&a.os_name)
        .unwrap_or(u32::MAX)
        .cmp(&display_number(&b.os_name).unwrap_or(u32::MAX))
        .then_with(|| a.os_name.cmp(&b.os_name))
}

/// Sorts monitors by [`monitor_order`].
pub fn sort_monitors(monitors: &mut [Monitor]) {
    monitors.sort_by(monitor_order);
}

/// The stable id of a monitor, its OS name when the display is not known.
pub fn monitor_id(monitor: &Monitor, displays: &[DisplayDetails]) -> String {
    displays
        .iter()
        .find(|d| d.os_name == monitor.os_name)
        .map_or_else(|| monitor.os_name.clone(), |d| d.id.clone())
}

/// Which of `monitors` (sorted by [`sort_monitors`]) the `display` text names: empty for the
/// primary monitor, a number from 1, or a display id or name as `out.projector` takes it.
pub fn choose_monitor(
    spec: &str,
    monitors: &[Monitor],
    displays: &[DisplayDetails],
) -> Result<usize, String> {
    let spec = spec.trim();
    if monitors.is_empty() {
        return Err("no monitor is connected".into());
    }
    if spec.is_empty() {
        return monitors
            .iter()
            .position(|m| m.primary)
            .or(Some(0))
            .ok_or_else(|| "no primary monitor".into());
    }
    if let Ok(n) = spec.parse::<usize>() {
        return match n {
            0 => Err("monitors count from 1".into()),
            n if n > monitors.len() => Err(format!(
                "there is no monitor {n}: {} {} connected",
                monitors.len(),
                if monitors.len() == 1 { "is" } else { "are" }
            )),
            n => Ok(n - 1),
        };
    }
    if let Some(i) = monitors.iter().position(|m| m.os_name.eq_ignore_ascii_case(spec)) {
        return Ok(i);
    }
    let resolved = resolve(spec, displays);
    let Some(d) = resolved.display() else {
        return Err(format!("no connected monitor is `{spec}` (screen.sources lists them)"));
    };
    monitors
        .iter()
        .position(|m| m.os_name == d.os_name)
        .ok_or_else(|| format!("`{spec}` is {}, which shows no desktop", d.os_name))
}

/// Whether a window's title contains `part`, ignoring case.
pub fn title_matches(title: &str, part: &str) -> bool {
    let part = part.trim();
    !part.is_empty() && title.to_lowercase().contains(&part.to_lowercase())
}

/// The first of `titles` (front to back) that contains `part`.
pub fn first_window<'a>(titles: impl IntoIterator<Item = &'a str>, part: &str) -> Option<usize> {
    titles.into_iter().position(|t| title_matches(t, part))
}

/// The shortest pause before a failed capture is tried again.
pub const RETRY_MIN: Duration = Duration::from_millis(250);
/// The longest pause.
pub const RETRY_MAX: Duration = Duration::from_secs(4);

/// The pause after `previous`: twice as long, at most [`RETRY_MAX`].
pub fn backoff(previous: Duration) -> Duration {
    (previous * 2).clamp(RETRY_MIN, RETRY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, primary: bool) -> Monitor {
        Monitor { os_name: name.into(), primary, width: 1920, height: 1080 }
    }

    fn display(id: &str, os_name: &str, monitor: &str) -> DisplayDetails {
        DisplayDetails {
            id: id.into(),
            os_name: os_name.into(),
            monitor: monitor.into(),
            ..Default::default()
        }
    }

    #[test]
    fn monitors_sort_by_their_display_number() {
        let mut list = vec![
            monitor(r"\\.\DISPLAY10", false),
            monitor(r"\\.\DISPLAY2", true),
            monitor(r"\\.\DISPLAY1", false),
        ];
        sort_monitors(&mut list);
        let names: Vec<_> = list.iter().map(|m| m.os_name.as_str()).collect();
        assert_eq!(names, [r"\\.\DISPLAY1", r"\\.\DISPLAY2", r"\\.\DISPLAY10"]);
        assert_eq!(display_number("no digits"), None);
    }

    #[test]
    fn a_monitor_is_chosen_by_nothing_a_number_or_a_name() {
        let monitors = [monitor(r"\\.\DISPLAY1", false), monitor(r"\\.\DISPLAY2", true)];
        let displays = [
            display("AW3225QF [DELA246 #BY4BYZ3]", r"\\.\DISPLAY1", "AW3225QF"),
            display("EPSON PJ [SECA519] @ target 168194", r"\\.\DISPLAY2", "EPSON PJ"),
        ];
        assert_eq!(choose_monitor("", &monitors, &displays), Ok(1));
        assert_eq!(choose_monitor(" 1 ", &monitors, &displays), Ok(0));
        assert_eq!(choose_monitor("2", &monitors, &displays), Ok(1));
        assert!(choose_monitor("0", &monitors, &displays).is_err());
        assert!(choose_monitor("3", &monitors, &displays).unwrap_err().contains("2 are connected"));
        assert_eq!(choose_monitor(r"\\.\display2", &monitors, &displays), Ok(1));
        assert_eq!(choose_monitor("AW3225QF [DELA246 #BY4BYZ3]", &monitors, &displays), Ok(0));
        assert_eq!(choose_monitor("EPSON PJ", &monitors, &displays), Ok(1));
        assert!(choose_monitor("nothing like it", &monitors, &displays).is_err());
        assert!(choose_monitor("", &[], &displays).is_err());
        // Without a primary flag (should not happen), the first.
        assert_eq!(choose_monitor("", &[monitor("a", false)], &[]), Ok(0));
        assert_eq!(monitor_id(&monitors[0], &displays), "AW3225QF [DELA246 #BY4BYZ3]");
        assert_eq!(monitor_id(&monitor("x", false), &displays), "x");
    }

    #[test]
    fn a_window_is_the_first_whose_title_contains_the_text() {
        let titles = ["penumbra - demo", "Untitled - Notepad", "notes.txt - Notepad"];
        assert_eq!(first_window(titles, "notepad"), Some(1));
        assert_eq!(first_window(titles, "NOTES"), Some(2));
        assert_eq!(first_window(titles, "  Notepad "), Some(1));
        assert_eq!(first_window(titles, "firefox"), None);
        assert_eq!(first_window(titles, ""), None);
        assert!(title_matches("Ärger – Übersicht", "über"));
    }

    #[test]
    fn retries_slow_down_to_a_limit() {
        assert_eq!(backoff(Duration::ZERO), RETRY_MIN);
        assert_eq!(backoff(RETRY_MIN), RETRY_MIN * 2);
        assert_eq!(backoff(RETRY_MAX), RETRY_MAX);
    }
}
