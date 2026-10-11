//! Which displays are connected, and a stable name for each.
//!
//! The order in which the OS enumerates displays changes with hot-plugging and between boots,
//! so a project names its displays by a [`DisplayDetails::id`] made of the monitor's own name
//! (from its EDID) and the port it is plugged into: `EPSON PJ @ port 4`. The port keeps two
//! identical projectors apart; the monitor name keeps a projector recognisable when its cable
//! moves to another port ([`resolve`] falls back to the name when it is unique).
//!
//! Ports are numbered among all connectors the graphics adapter has, connected or not, so a
//! number does not change when something is plugged in elsewhere. The connector type the OS
//! reports is shown but not part of the id: it is not reliable (the desktop's Epson on HDMI
//! reports DVI, seen 2026-10-09).

use penumbra_core::Error;
use serde::Serialize;

/// What the OS reports about one active display, before naming (Windows: one display path).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawDisplay {
    /// The desktop's name for it, which is also what winit's `MonitorHandle::name` returns
    /// (`\\.\DISPLAY2` on Windows).
    pub os_name: String,
    /// The monitor's name from its EDID (`EPSON PJ`); empty when unknown.
    pub monitor: String,
    /// The connector's type (Windows: `DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY`).
    pub technology: i32,
    /// The graphics adapter (Windows: its LUID).
    pub adapter: i64,
    /// The connector on the adapter (Windows: the target id).
    pub target: u32,
    /// EDID manufacturer and product code, as the OS packs them.
    pub edid: Option<(u16, u16)>,
    /// The monitor's device path (Windows), for diagnostics.
    pub device_path: String,
    /// Refresh rate as a fraction, when the OS reports one.
    pub refresh: Option<(u32, u32)>,
}

/// A connected display with its stable id.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DisplayDetails {
    /// What projects store: `<monitor> @ <port>`, e.g. `EPSON PJ @ port 4`.
    pub id: String,
    /// The OS name winit reports for the same display (`\\.\DISPLAY2`).
    pub os_name: String,
    /// The monitor's name (`EPSON PJ`), or `display` when it has none.
    pub monitor: String,
    /// `port 4`: the connector's number among the adapter's connectors; `port 2.4` when the
    /// machine has several adapters with connectors (a laptop with hybrid graphics).
    pub port: String,
    /// The connector type the OS reports (`HDMI`, `DP`, `DVI`, ...), informative only.
    pub connector: &'static str,
    /// Manufacturer code and product, e.g. `SEC 0xA514`.
    pub edid: Option<String>,
    pub refresh_hz: Option<f64>,
}

/// The connected displays, named. Empty where the platform has no backend yet (Linux: winit's
/// connector names are used directly, see the output crate).
pub fn displays() -> Result<Vec<DisplayDetails>, Error> {
    #[cfg(windows)]
    {
        Ok(name_displays(&crate::windows::displays()?, &crate::windows::connectors()?))
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

fn technology(t: i32) -> &'static str {
    match t {
        0 => "VGA",
        4 => "DVI",
        5 => "HDMI",
        6 | 11 | 13 | i32::MIN => "internal",
        10 => "DP",
        18 => "USB-C",
        15 => "Miracast",
        16 | 17 => "indirect",
        _ => "other",
    }
}

/// The three letters EDID packs into five bits each (stored big-endian; Windows hands the word
/// over byte-swapped).
fn manufacturer(code: u16) -> String {
    let code = code.swap_bytes();
    let letter = |shift: u16| {
        let v = ((code >> shift) & 0x1f) as u8;
        if (1..=26).contains(&v) { char::from(b'A' + v - 1) } else { '?' }
    };
    [letter(10), letter(5), letter(0)].iter().collect()
}

/// Names raw displays. `connectors` are `(adapter, target)` of every connector, connected or
/// not; a display whose connector is missing there (it should not be) still gets a number.
pub fn name_displays(raw: &[RawDisplay], connectors: &[(i64, u32)]) -> Vec<DisplayDetails> {
    let mut connectors = connectors.to_vec();
    connectors.extend(raw.iter().map(|d| (d.adapter, d.target)));
    connectors.sort_unstable();
    connectors.dedup();
    let mut adapters: Vec<i64> = connectors.iter().map(|c| c.0).collect();
    adapters.dedup();
    let mut out = Vec::new();
    for d in raw {
        let number = 1 + connectors.iter().filter(|c| c.0 == d.adapter && c.1 < d.target).count();
        let port = match adapters.iter().position(|&a| a == d.adapter) {
            Some(a) if adapters.len() > 1 => format!("port {}.{number}", a + 1),
            _ => format!("port {number}"),
        };
        let monitor = d.monitor.trim();
        let monitor = if monitor.is_empty() { "display".to_string() } else { monitor.to_string() };
        out.push(DisplayDetails {
            id: format!("{monitor} @ {port}"),
            os_name: d.os_name.clone(),
            monitor,
            port,
            connector: technology(d.technology),
            edid: d.edid.map(|(m, p)| format!("{} 0x{p:04X}", manufacturer(m))),
            refresh_hz: d.refresh.map(|(n, den)| f64::from(n) / f64::from(den.max(1))),
        });
    }
    out
}

/// How a stored display id found its display.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved<'a> {
    /// Monitor and port match (or the id is the OS name).
    Exact(&'a DisplayDetails),
    /// Only the monitor name matches, and exactly one connected display has it: the cable moved.
    ByName(&'a DisplayDetails),
    NotConnected,
    /// Several connected displays have the monitor name and none is on the stored port.
    Ambiguous(Vec<&'a DisplayDetails>),
}

impl<'a> Resolved<'a> {
    pub fn display(&self) -> Option<&'a DisplayDetails> {
        match self {
            Resolved::Exact(d) | Resolved::ByName(d) => Some(d),
            _ => None,
        }
    }
}

/// Finds the display `id` names. Exact first, then by monitor name when that is unique; the
/// part before ` @ ` alone also works as an id (`EPSON PJ`).
pub fn resolve<'a>(id: &str, displays: &'a [DisplayDetails]) -> Resolved<'a> {
    let id = id.trim();
    if let Some(d) = displays.iter().find(|d| d.id == id || d.os_name == id) {
        return Resolved::Exact(d);
    }
    let name = id.split_once(" @ ").map_or(id, |(m, _)| m.trim());
    let same: Vec<&DisplayDetails> = displays.iter().filter(|d| d.monitor == name).collect();
    match same.as_slice() {
        [] => Resolved::NotConnected,
        [one] => Resolved::ByName(one),
        _ => Resolved::Ambiguous(same),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, monitor: &str, adapter: i64, target: u32) -> RawDisplay {
        RawDisplay {
            os_name: name.into(),
            monitor: monitor.into(),
            technology: 5,
            adapter,
            target,
            // As the desktop's Epson reports them.
            edid: Some((41804, 42260)),
            refresh: Some((60000, 1000)),
            ..Default::default()
        }
    }

    const PORTS: &[(i64, u32)] = &[(7, 10), (7, 11), (7, 12), (7, 14), (7, 15), (7, 16)];

    #[test]
    fn ports_count_every_connector() {
        let list = name_displays(
            &[raw("D1", "AW3225QF", 7, 16), raw("D2", "EPSON PJ", 7, 14), raw("D3", " ", 7, 10)],
            PORTS,
        );
        let ids: Vec<&str> = list.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["AW3225QF @ port 6", "EPSON PJ @ port 4", "display @ port 1"]);
        assert_eq!(list[1].edid.as_deref(), Some("SEC 0xA514"));
        assert_eq!(list[1].refresh_hz, Some(60.0));
        assert_eq!(list[1].connector, "HDMI");
        // Unplugging another display does not renumber.
        let alone = name_displays(&[raw("D2", "EPSON PJ", 7, 14)], PORTS);
        assert_eq!(alone[0].id, "EPSON PJ @ port 4");
    }

    #[test]
    fn several_adapters() {
        let list =
            name_displays(&[raw("a", "Panel", 1, 3), raw("b", "EPSON PJ", 2, 9)], &[(1, 3), (2, 8), (2, 9)]);
        assert_eq!(list[0].id, "Panel @ port 1.1");
        assert_eq!(list[1].id, "EPSON PJ @ port 2.2");
    }

    #[test]
    fn manufacturer_letters() {
        // "EPS" = 00101 10000 10011 = 0x1613, stored byte-swapped by Windows.
        assert_eq!(manufacturer(0x1316), "EPS");
    }

    #[test]
    fn resolving() {
        let list = name_displays(
            &[raw("D2", "EPSON PJ", 7, 10), raw("D3", "EPSON PJ", 7, 11), raw("D1", "AW3225QF", 7, 12)],
            PORTS,
        );
        assert!(matches!(resolve("EPSON PJ @ port 2", &list), Resolved::Exact(d) if d.os_name == "D3"));
        assert!(
            matches!(resolve("AW3225QF @ port 6", &list), Resolved::ByName(d) if d.monitor == "AW3225QF")
        );
        assert!(matches!(resolve("EPSON PJ @ port 6", &list), Resolved::Ambiguous(v) if v.len() == 2));
        assert!(matches!(resolve("EPSON PJ", &list), Resolved::Ambiguous(_)));
        assert!(matches!(resolve(" AW3225QF ", &list), Resolved::ByName(_)));
        assert!(matches!(resolve("D2", &list), Resolved::Exact(_)));
        assert_eq!(resolve("BenQ @ port 1", &list), Resolved::NotConnected);
    }

    /// Prints what this machine reports (`cargo test -p penumbra-system -- --nocapture`).
    #[test]
    fn this_machine() {
        #[cfg(windows)]
        {
            for r in crate::windows::displays().unwrap() {
                println!("{r:?}");
            }
            println!("{:?}", crate::windows::connectors().unwrap());
        }
        for d in displays().unwrap() {
            println!("{d:?}");
        }
    }
}
