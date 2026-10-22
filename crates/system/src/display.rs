//! Which displays are connected, and a stable name for each.
//!
//! A project names its displays by a [`DisplayDetails::id`] keyed on the display's own identity,
//! not on the order or position in which the OS enumerates it:
//!
//! - `AW3225QF [DELA246 #BY4BYZ3]`: monitor name, EDID manufacturer and product (the PnP id
//!   Device Manager shows) and the EDID serial number. A display with a usable serial is found
//!   by that alone, on whatever connector it is plugged into.
//! - `EPSON PJ [SECA519] @ target 168194`: without a usable serial, two units of the same model
//!   cannot be told apart by anything they report, so the connector decides: the OS target id
//!   of the graphics adapter's output. With several adapters the adapter's number comes first
//!   (`@ gpu 2 target 168194`); that number follows the adapters' order, which is not
//!   guaranteed to survive a reboot.
//! - `display @ target 168194`: a display without EDID.
//!
//! What was found on Windows (an RTX 3090, three Epson projectors, two of them the same model):
//!
//! - `QueryDisplayConfig(QDC_ALL_PATHS)` does not list a fixed set of connectors. It listed
//!   target ids 168192 to 168198 except 168195, each with an output type (DP, HDMI, DVI), more
//!   ids than the card has sockets, and the set changed when a display was added. So a number
//!   counted among the listed targets (the former `EPSON PJ @ port 4`) shifts: that is what
//!   moved every projector to another display once a third one was plugged in.
//! - A target id itself stays with its socket: the monitor's device path and its registry
//!   entry carry it (`DISPLAY\SECA519\5&12587d74&1&UID168194`), and the entries of earlier
//!   connections stay under their own ids. Presumably a socket driven directly and through a
//!   passive adapter (DP to HDMI) is two targets, so changing the adapter may change the id.
//! - `DISPLAYCONFIG_TARGET_DEVICE_NAME` has manufacturer, product, connector instance and
//!   device path, but no serial number. The serial comes from the EDID the OS keeps in the
//!   registry (`HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\<pnp>\<instance>\Device
//!   Parameters\EDID`, the instance taken from the device path). Monitors usually have one
//!   (the Dell: descriptor `BY4BYZ3`); the Epson projectors have none (`0x01010101` in the base
//!   block, no serial descriptor), and their EDIDs are byte-identical from unit to unit.
//! - The connector type the OS reports is shown but not part of the id: it is not reliable (an
//!   Epson on HDMI is reported as DVI).
//!
//! [`resolve`] also takes the OS name (`\\.\DISPLAY2`), the monitor name alone when it is unique
//! (`EPSON PJ`), the name with the PnP id (`EPSON PJ [SECA519]`) and the old ids
//! (`EPSON PJ @ port 4`) when their monitor name is unique. The old port number never decides.

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
    /// The EDID serial number when it is usable (see [`edid_serial`]).
    pub serial: Option<String>,
    /// The monitor's device path (Windows), for diagnostics.
    pub device_path: String,
    /// Refresh rate as a fraction, when the OS reports one.
    pub refresh: Option<(u32, u32)>,
}

/// A connected display with its stable id.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DisplayDetails {
    /// What projects store, see the module documentation.
    pub id: String,
    /// The OS name winit reports for the same display (`\\.\DISPLAY2`).
    pub os_name: String,
    /// The monitor's name (`EPSON PJ`), or `display` when it has none.
    pub monitor: String,
    /// Manufacturer and product as a PnP id, e.g. `SECA519`.
    pub edid: Option<String>,
    /// The EDID serial number, when the display has a usable one.
    pub serial: Option<String>,
    /// The connector: `target 168194`, or `gpu 2 target 168194` with several adapters.
    pub target: String,
    /// The connector type the OS reports (`HDMI`, `DP`, `DVI`, ...), informative only.
    pub connector: &'static str,
    pub refresh_hz: Option<f64>,
    /// The id this display had before ids were keyed on identity (`EPSON PJ @ port 4`): the
    /// connector's number among the connectors the OS lists, which shifts when displays are
    /// added. Only used to explain old ids, never to choose a display.
    #[serde(skip)]
    pub old_id: String,
    /// The adapter's number when there are several (1-based).
    #[serde(skip)]
    pub gpu: Option<usize>,
    /// The OS target id; `None` where the platform has none (Linux).
    #[serde(skip)]
    pub target_id: Option<u32>,
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

/// Manufacturer and product as Windows writes the PnP id (`SECA519`).
fn pnp_id((manufacturer_code, product): (u16, u16)) -> String {
    format!("{}{product:04X}", manufacturer(manufacturer_code))
}

/// The characters that structure an id; kept out of names and serials.
const RESERVED: &[char] = &['[', ']', '#', '@'];

fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control() && !RESERVED.contains(c)).collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() { "display".to_string() } else { name }
}

/// A serial as it may appear in an id: visible ASCII only, `None` for placeholders (empty, one
/// character repeated such as `0`, `1`, `00000000`, `FFFFFFFF`).
fn clean_serial(serial: &str) -> Option<String> {
    let s: String = serial.chars().filter(|c| c.is_ascii_graphic() && !RESERVED.contains(c)).collect();
    let first = s.chars().next()?;
    if s.chars().all(|c| c.eq_ignore_ascii_case(&first)) {
        return None;
    }
    Some(s)
}

/// The serial number in an EDID blob: the serial descriptor (tag `0xFF`, the text printed on
/// the label) when there is a usable one, else the 32-bit number in bytes 12 to 15 of the base
/// block when it is not a placeholder (0, 1, one byte repeated). `None` for blobs that are short,
/// lack the EDID header or fail the base block's checksum.
pub fn edid_serial(blob: &[u8]) -> Option<String> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let base = blob.get(..128)?;
    if base.get(..8)? != HEADER || base.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) != 0 {
        return None;
    }
    for at in [54usize, 72, 90, 108] {
        let Some(d) = base.get(at..at + 18) else { continue };
        if d.get(..5) == Some(&[0, 0, 0, 0xFF, 0]) {
            let text: String = d
                .get(5..)
                .unwrap_or_default()
                .iter()
                .take_while(|&&c| c != 0x0A && c != 0)
                .map(|&c| char::from(c))
                .collect();
            if let Some(s) = clean_serial(&text) {
                return Some(s);
            }
        }
    }
    let bytes: [u8; 4] = base.get(12..16)?.try_into().ok()?;
    let number = u32::from_le_bytes(bytes);
    if number == 1 || bytes.iter().all(|&b| b == bytes[0]) {
        return None;
    }
    Some(number.to_string())
}

/// Names raw displays. `connectors` are `(adapter, target)` of the connectors the OS lists; they
/// only number the old ids ([`DisplayDetails::old_id`]).
pub fn name_displays(raw: &[RawDisplay], connectors: &[(i64, u32)]) -> Vec<DisplayDetails> {
    let mut connectors = connectors.to_vec();
    connectors.extend(raw.iter().map(|d| (d.adapter, d.target)));
    connectors.sort_unstable();
    connectors.dedup();
    let mut adapters: Vec<i64> = connectors.iter().map(|c| c.0).collect();
    adapters.dedup();
    let several = adapters.len() > 1;
    let mut out = Vec::new();
    for d in raw {
        let gpu = adapters.iter().position(|&a| a == d.adapter).map(|a| a + 1).filter(|_| several);
        let number = 1 + connectors.iter().filter(|c| c.0 == d.adapter && c.1 < d.target).count();
        let (old_port, target) = match gpu {
            Some(g) => (format!("port {g}.{number}"), format!("gpu {g} target {}", d.target)),
            None => (format!("port {number}"), format!("target {}", d.target)),
        };
        let monitor = clean_name(&d.monitor);
        let edid = d.edid.map(pnp_id);
        let serial = d.serial.as_deref().and_then(clean_serial).filter(|_| edid.is_some());
        let id = match (&edid, &serial) {
            (Some(e), Some(s)) => format!("{monitor} [{e} #{s}]"),
            (Some(e), None) => format!("{monitor} [{e}] @ {target}"),
            (None, _) => format!("{monitor} @ {target}"),
        };
        out.push(DisplayDetails {
            id,
            os_name: d.os_name.clone(),
            old_id: format!("{monitor} @ {old_port}"),
            monitor,
            edid,
            serial,
            target,
            connector: technology(d.technology),
            refresh_hz: d.refresh.map(|(n, den)| f64::from(n) / f64::from(den.max(1))),
            gpu,
            target_id: Some(d.target),
        });
    }
    out
}

/// How a stored display id found its display.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved<'a> {
    /// The id names this display: its identity matches (and, without a serial, its connector),
    /// or the id is its OS name.
    Exact(&'a DisplayDetails),
    /// A monitor name (alone or with its PnP id) that exactly one connected display has.
    ByName(&'a DisplayDetails),
    /// An old id (`EPSON PJ @ port 4`) whose monitor name exactly one connected display has.
    OldId(&'a DisplayDetails),
    /// Nothing that is certainly this display. `similar`: displays of the same model on other
    /// connectors, which may or may not be the same unit.
    NotConnected { similar: Vec<&'a DisplayDetails> },
    /// Several connected displays fit the id.
    Ambiguous(Vec<&'a DisplayDetails>),
}

impl<'a> Resolved<'a> {
    pub fn display(&self) -> Option<&'a DisplayDetails> {
        match self {
            Resolved::Exact(d) | Resolved::ByName(d) | Resolved::OldId(d) => Some(d),
            _ => None,
        }
    }

    /// What to tell the user about how `stored` resolved among `all`: an error (nothing is
    /// shown) or a warning (shown, but the id should be renewed); `None` when all is well.
    pub fn problem(&self, stored: &str, all: &[DisplayDetails]) -> Option<Problem> {
        let ids = |list: &[&DisplayDetails]| {
            list.iter().map(|d| format!("`{}`", d.id)).collect::<Vec<_>>().join(", ")
        };
        let stored = stored.trim();
        let old = matches!(parse(stored).place, Place::OldPort);
        match self {
            Resolved::Exact(_) | Resolved::ByName(_) => None,
            Resolved::OldId(d) => Some(Problem::Warning(format!(
                "`{stored}` is an old id with a port number that shifts when displays are added; \
                 shown on `{}`, the only `{}`. Choose the display again to store its stable id",
                d.id, d.monitor
            ))),
            Resolved::NotConnected { similar } if !similar.is_empty() => Some(Problem::Error(format!(
                "display `{stored}` is not connected, nothing is shown. The same model is connected \
                 as {}: if that is this projector, choose it (`test` shows which one it is)",
                ids(similar)
            ))),
            Resolved::NotConnected { .. } => {
                let all: Vec<&DisplayDetails> = all.iter().collect();
                Some(Problem::Error(format!(
                    "display `{stored}` is not connected, nothing is shown; connected: {}",
                    if all.is_empty() { "none".to_string() } else { ids(&all) }
                )))
            }
            Resolved::Ambiguous(list) => {
                let mut text =
                    format!("`{stored}` fits several displays ({}), nothing is shown; choose one", ids(list));
                if old {
                    text.push_str(". It is an old id, whose port numbers shift when displays are added");
                    if let Some(d) = all.iter().find(|d| d.old_id == stored) {
                        text.push_str(&format!(
                            "; by today's numbering it would be `{}`, check with `test` before choosing it",
                            d.id
                        ));
                    }
                }
                Some(Problem::Error(text))
            }
        }
    }
}

/// See [`Resolved::problem`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Warning(String),
    Error(String),
}

/// Where a stored id says the display is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// Nothing after the name, or something not understood.
    Any,
    Target {
        gpu: Option<usize>,
        id: u32,
    },
    /// An old id's `port 4` / `port 2.4`.
    OldPort,
}

#[derive(Debug, PartialEq, Eq)]
struct Wanted<'s> {
    name: &'s str,
    pnp: Option<&'s str>,
    serial: Option<&'s str>,
    place: Place,
}

fn parse(id: &str) -> Wanted<'_> {
    let (head, place) = match id.split_once(" @ ") {
        Some((head, place)) => {
            let words: Vec<&str> = place.split_whitespace().collect();
            let place = match words.as_slice() {
                ["target", n] => n.parse().map_or(Place::Any, |id| Place::Target { gpu: None, id }),
                ["gpu", g, "target", n] => match (g.parse(), n.parse()) {
                    (Ok(g), Ok(id)) => Place::Target { gpu: Some(g), id },
                    _ => Place::Any,
                },
                ["port", _] => Place::OldPort,
                _ => Place::Any,
            };
            (head.trim(), place)
        }
        None => (id.trim(), Place::Any),
    };
    match head.strip_suffix(']').and_then(|h| h.rsplit_once('[')) {
        Some((name, inner)) => {
            let (pnp, serial) = match inner.split_once('#') {
                Some((pnp, serial)) => (pnp.trim(), Some(serial.trim())),
                None => (inner.trim(), None),
            };
            Wanted { name: name.trim(), pnp: Some(pnp), serial, place }
        }
        None => Wanted { name: head, pnp: None, serial: None, place },
    }
}

/// Finds the display `id` names, see the module documentation. The EDID identity decides; the
/// connector only between displays that cannot be told apart otherwise; a guess is never made.
pub fn resolve<'a>(id: &str, displays: &'a [DisplayDetails]) -> Resolved<'a> {
    let id = id.trim();
    if let Some(d) = displays.iter().find(|d| d.id == id || d.os_name == id) {
        return Resolved::Exact(d);
    }
    let w = parse(id);
    let model = |d: &&DisplayDetails| w.pnp.is_none_or(|p| d.edid.as_deref() == Some(p));
    let same_model = || -> Vec<&'a DisplayDetails> {
        if w.pnp.is_some() { displays.iter().filter(model).collect() } else { Vec::new() }
    };
    let decide =
        |list: Vec<&'a DisplayDetails>, one: fn(&'a DisplayDetails) -> Resolved<'a>| match list.as_slice() {
            [] => Resolved::NotConnected { similar: same_model() },
            [d] => one(d),
            _ => Resolved::Ambiguous(list),
        };
    if let (Some(_), Some(serial)) = (w.pnp, w.serial) {
        let list = displays.iter().filter(model).filter(|d| d.serial.as_deref() == Some(serial)).collect();
        return decide(list, Resolved::Exact);
    }
    if let Place::Target { gpu, id: target } = w.place {
        let list = displays
            .iter()
            .filter(|d| d.target_id == Some(target) && gpu.is_none_or(|g| d.gpu == Some(g)))
            .filter(|d| if w.pnp.is_some() { model(d) } else { d.edid.is_none() })
            .collect();
        return decide(list, Resolved::Exact);
    }
    let list = displays.iter().filter(model).filter(|d| d.monitor == w.name).collect();
    decide(list, if w.place == Place::OldPort { Resolved::OldId } else { Resolved::ByName })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSON_A519: (u16, u16) = (41804, 0xA519);
    const EPSON_A514: (u16, u16) = (41804, 0xA514);
    const DELL: (u16, u16) = (44048, 0xA246);

    fn raw(name: &str, monitor: &str, edid: (u16, u16), serial: Option<&str>, target: u32) -> RawDisplay {
        RawDisplay {
            os_name: name.into(),
            monitor: monitor.into(),
            technology: 5,
            adapter: 7,
            target,
            edid: Some(edid),
            serial: serial.map(Into::into),
            refresh: Some((60000, 1000)),
            ..Default::default()
        }
    }

    /// The desktop as found: a Dell with a serial, two Epsons without.
    fn desk() -> Vec<RawDisplay> {
        vec![
            raw("D1", "AW3225QF", DELL, Some("BY4BYZ3"), 168198),
            raw("D2", "EPSON PJ", EPSON_A519, None, 168194),
            raw("D3", "EPSON PJ", EPSON_A514, None, 168196),
        ]
    }

    /// What QDC_ALL_PATHS listed (168195 missing).
    const LISTED: &[(i64, u32)] =
        &[(7, 168192), (7, 168193), (7, 168194), (7, 168196), (7, 168197), (7, 168198)];

    fn ids(list: &[DisplayDetails]) -> Vec<&str> {
        list.iter().map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn ids_from_identity() {
        let list = name_displays(&desk(), LISTED);
        assert_eq!(
            ids(&list),
            [
                "AW3225QF [DELA246 #BY4BYZ3]",
                "EPSON PJ [SECA519] @ target 168194",
                "EPSON PJ [SECA514] @ target 168196"
            ]
        );
        assert_eq!(list[1].old_id, "EPSON PJ @ port 3");
        assert_eq!(list[2].old_id, "EPSON PJ @ port 4");
        assert_eq!(list[1].refresh_hz, Some(60.0));
        assert_eq!(list[1].connector, "HDMI");
        assert_eq!(list[1].target, "target 168194");
    }

    #[test]
    fn a_third_display_changes_no_other_id() {
        let before = name_displays(&desk(), LISTED);
        let mut more = desk();
        more.push(raw("D4", "EPSON PJ", EPSON_A519, None, 168195));
        let mut listed = LISTED.to_vec();
        listed.push((7, 168195));
        let after = name_displays(&more, &listed);
        assert_eq!(ids(&after)[..3], ids(&before)[..]);
        // The old numbering shifted, which is what the ids no longer depend on.
        assert_eq!(after[2].old_id, "EPSON PJ @ port 5");
        for d in &before {
            assert!(matches!(resolve(&d.id, &after), Resolved::Exact(x) if x.os_name == d.os_name));
        }
    }

    #[test]
    fn identical_projectors_told_apart_by_serial() {
        let list = name_displays(
            &[
                raw("D2", "EPSON PJ", EPSON_A519, Some("X5ZK000123"), 168194),
                raw("D4", "EPSON PJ", EPSON_A519, Some("X5ZK000456"), 168197),
            ],
            &[],
        );
        assert_eq!(ids(&list), ["EPSON PJ [SECA519 #X5ZK000123]", "EPSON PJ [SECA519 #X5ZK000456]"]);
        // Swapped connectors: each id stays with its projector.
        let swapped = name_displays(
            &[
                raw("D2", "EPSON PJ", EPSON_A519, Some("X5ZK000456"), 168194),
                raw("D4", "EPSON PJ", EPSON_A519, Some("X5ZK000123"), 168197),
            ],
            &[],
        );
        assert!(matches!(resolve(&list[0].id, &swapped), Resolved::Exact(d) if d.os_name == "D4"));
        assert!(matches!(resolve(&list[1].id, &swapped), Resolved::Exact(d) if d.os_name == "D2"));
    }

    #[test]
    fn with_serial_the_display_follows_its_cable() {
        let id = "AW3225QF [DELA246 #BY4BYZ3]";
        let moved = name_displays(&[raw("D9", "AW3225QF", DELL, Some("BY4BYZ3"), 168192)], &[]);
        assert_eq!(moved[0].id, id);
        assert!(matches!(resolve(id, &moved), Resolved::Exact(d) if d.os_name == "D9"));
        // Another unit of the same model is not it.
        let other = name_displays(&[raw("D9", "AW3225QF", DELL, Some("ZZ0001"), 168198)], &[]);
        assert!(matches!(resolve(id, &other), Resolved::NotConnected { similar } if similar.len() == 1));
    }

    #[test]
    fn without_serial_the_connector_decides() {
        let list = name_displays(
            &[
                raw("D2", "EPSON PJ", EPSON_A519, None, 168194),
                raw("D4", "EPSON PJ", EPSON_A519, None, 168197),
            ],
            &[],
        );
        assert!(
            matches!(resolve("EPSON PJ [SECA519] @ target 168197", &list), Resolved::Exact(d) if d.os_name == "D4")
        );
        // Moved to another connector: maybe the same unit, maybe the other; no guess.
        let moved = name_displays(&[raw("D4", "EPSON PJ", EPSON_A519, None, 168193)], &[]);
        assert!(matches!(
            resolve("EPSON PJ [SECA519] @ target 168197", &moved),
            Resolved::NotConnected { similar } if similar.len() == 1
        ));
        // Another model on that connector is not it.
        let swapped = name_displays(&[raw("D4", "EPSON PJ", EPSON_A514, None, 168197)], &[]);
        assert!(matches!(
            resolve("EPSON PJ [SECA519] @ target 168197", &swapped),
            Resolved::NotConnected { similar } if similar.is_empty()
        ));
        // Name and model without a connector: only when unique.
        assert!(matches!(resolve("EPSON PJ [SECA519]", &list), Resolved::Ambiguous(v) if v.len() == 2));
        assert!(matches!(resolve("EPSON PJ [SECA519]", &moved), Resolved::ByName(_)));
    }

    #[test]
    fn old_ids() {
        let list = name_displays(&desk(), LISTED);
        // The Dell is the only one of its name.
        assert!(matches!(resolve("AW3225QF @ port 6", &list), Resolved::OldId(d) if d.os_name == "D1"));
        assert!(matches!(resolve("AW3225QF @ port 2", &list), Resolved::OldId(_)));
        // Two Epsons: the port number does not decide, even when it matches today.
        let r = resolve("EPSON PJ @ port 4", &list);
        assert!(matches!(&r, Resolved::Ambiguous(v) if v.len() == 2));
        let Some(Problem::Error(text)) = r.problem("EPSON PJ @ port 4", &list) else { panic!() };
        assert!(text.contains("would be `EPSON PJ [SECA514] @ target 168196`"), "{text}");
        let one = name_displays(&desk()[2..], LISTED);
        let r = resolve("EPSON PJ @ port 4", &one);
        assert!(matches!(r, Resolved::OldId(_)));
        assert!(matches!(r.problem("EPSON PJ @ port 4", &one), Some(Problem::Warning(_))));
    }

    #[test]
    fn names_and_os_names() {
        let list = name_displays(&desk(), LISTED);
        assert!(matches!(resolve("D2", &list), Resolved::Exact(_)));
        assert!(matches!(resolve(" AW3225QF ", &list), Resolved::ByName(_)));
        assert!(matches!(resolve("EPSON PJ", &list), Resolved::Ambiguous(_)));
        assert!(matches!(resolve("BenQ", &list), Resolved::NotConnected { .. }));
        assert!(matches!(resolve("BenQ [BNQ7F1A #77] ", &list), Resolved::NotConnected { .. }));
        assert!(matches!(resolve("EPSON PJ @ hdmi", &list), Resolved::Ambiguous(_)));
        assert_eq!(resolve("", &list), Resolved::NotConnected { similar: Vec::new() });
        let r = resolve("BenQ", &list);
        let Some(Problem::Error(text)) = r.problem("BenQ", &list) else { panic!() };
        assert!(text.contains("connected: `AW3225QF [DELA246 #BY4BYZ3]`"), "{text}");
    }

    #[test]
    fn no_edid_and_odd_names() {
        let mut d = raw("D5", "  ", DELL, None, 9);
        d.edid = None;
        let mut odd = raw("D6", "A [b] #1 @ x", DELL, Some("FFFFFFFF"), 10);
        odd.adapter = 8;
        let list = name_displays(&[d, odd], &[]);
        assert_eq!(ids(&list), ["display @ gpu 1 target 9", "A b 1 x [DELA246] @ gpu 2 target 10"]);
        assert!(matches!(resolve("display @ gpu 1 target 9", &list), Resolved::Exact(_)));
        assert!(matches!(resolve("display @ target 9", &list[..1]), Resolved::Exact(_)));
        assert!(
            matches!(resolve("x [DELA246] @ gpu 2 target 10", &list), Resolved::Exact(d) if d.os_name == "D6")
        );
        assert!(matches!(resolve("display @ gpu 2 target 9", &list), Resolved::NotConnected { .. }));
    }

    #[test]
    fn manufacturer_letters() {
        // "EPS" = 00101 10000 10011 = 0x1613, stored byte-swapped by Windows.
        assert_eq!(manufacturer(0x1316), "EPS");
        assert_eq!(pnp_id(EPSON_A519), "SECA519");
    }

    /// A base block with the header, `serial` in bytes 12-15, the given descriptors and a valid
    /// checksum.
    fn edid(serial: u32, descriptors: &[(u8, &[u8])]) -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        b[12..16].copy_from_slice(&serial.to_le_bytes());
        for (i, (tag, text)) in descriptors.iter().enumerate() {
            let at = 72 + 18 * i;
            b[at + 3] = *tag;
            let mut t = text.to_vec();
            t.push(0x0A);
            t.resize(13, 0x20);
            b[at + 5..at + 18].copy_from_slice(&t);
        }
        let sum = b[..127].iter().fold(0u8, |s, &x| s.wrapping_add(x));
        b[127] = 0u8.wrapping_sub(sum);
        b
    }

    #[test]
    fn edid_serials() {
        // The descriptor wins over the number.
        assert_eq!(
            edid_serial(&edid(0x3036_3353, &[(0xFF, b"BY4BYZ3"), (0xFC, b"AW3225QF")])).as_deref(),
            Some("BY4BYZ3")
        );
        assert_eq!(edid_serial(&edid(0x3036_3353, &[(0xFC, b"AW3225QF")])).as_deref(), Some("808858451"));
        // The Epsons: a placeholder number, no descriptor.
        assert_eq!(edid_serial(&edid(0x0101_0101, &[(0xFC, b"EPSON PJ")])), None);
        for placeholder in [0, 1, 0xFFFF_FFFF] {
            assert_eq!(edid_serial(&edid(placeholder, &[])), None);
        }
        // A placeholder descriptor falls back to the number.
        assert_eq!(edid_serial(&edid(1234, &[(0xFF, b"0000000")])).as_deref(), Some("1234"));
        assert_eq!(edid_serial(&edid(0, &[(0xFF, b"0")])), None);
        assert_eq!(edid_serial(&edid(0, &[(0xFF, b" A#1[]\x01")])).as_deref(), Some("A1"));
        // Longer blobs (extension blocks) are fine.
        let mut long = edid(0, &[(0xFF, b"SN42")]);
        long.extend_from_slice(&[0x02; 128]);
        assert_eq!(edid_serial(&long).as_deref(), Some("SN42"));
    }

    #[test]
    fn broken_edids() {
        assert_eq!(edid_serial(&[]), None);
        let good = edid(0, &[(0xFF, b"SN42")]);
        assert_eq!(edid_serial(&good[..127]), None);
        let mut bad_sum = good.clone();
        bad_sum[127] ^= 1;
        assert_eq!(edid_serial(&bad_sum), None);
        let mut no_header = good;
        no_header[0] = 1;
        assert_eq!(edid_serial(&no_header), None);
        let garbage: Vec<u8> = (0..300u32).map(|i| (i * 37 % 251) as u8).collect();
        assert_eq!(edid_serial(&garbage), None);
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
