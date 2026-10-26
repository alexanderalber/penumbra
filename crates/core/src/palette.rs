//! Palettes and colormaps: named lists of colours that a `palette` param ([`crate::ParamKind`])
//! names, so a look is switched by picking a name instead of editing colours one by one.
//!
//! A palette is a handful of colours meant one by one (a colour per face); a colormap is a
//! longer list meant as a continuous ramp (a value 0..1 picks a blend). Both are the same data,
//! a list of up to [`MAX_COLOURS`] colours; which one a node uses them as is the node's
//! business. The kind only sorts the built-in set ([`builtin`]).
//!
//! A `palette` param holds text or an int:
//! - a name: one of the built-in set or one the project defines in [`FILE`];
//! - a list of hex colours, as colorhunt or coolors give them: `31aaa9 f8e0a4 a82020 6c1a1a`,
//!   `#31aaa9, #f8e0a4`, `31aaa9-f8e0a4-a82020`, a pasted URL ending in the colours, or one run
//!   of digits (`31aaa9f8e0a4a82020`); 6 digits per colour, or 8 with alpha;
//! - an int (what a bound channel gives): the palette at that place in the full list, the
//!   built-in ones first, then the project's, wrapping around;
//! - empty: no palette (the node says what that means; a shader gets a count of 0).
//!
//! Colours are written in sRGB (as every colour picker shows them) and held linear, as every
//! other colour in penumbra. Nothing here reads files: [`parse_file`] takes the text.

use std::sync::OnceLock;

use crate::Value;

/// Most colours in one palette (the size of a shader's palette uniform).
pub const MAX_COLOURS: usize = 32;
/// Longest name of a palette.
pub const MAX_NAME: usize = 32;
/// Longest accepted palette value, in bytes.
pub const MAX_VALUE: usize = 4096;
/// The file in the project directory that holds the project's own palettes.
pub const FILE: &str = "palettes.txt";
/// Largest palettes file read.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// Most palettes one file defines.
pub const MAX_PALETTES: usize = 1000;

/// What a list of colours is meant as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A handful of colours meant one by one.
    Palette,
    /// A ramp meant to be blended along.
    Colormap,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Palette => "palette",
            Kind::Colormap => "colormap",
        }
    }
}

/// Where a palette comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    /// The project's [`FILE`].
    Project,
    /// Written out in the param itself.
    Inline,
}

impl Source {
    pub fn key(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::Project => "project",
            Source::Inline => "inline",
        }
    }
}

/// A resolved palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// The name; for an inline list, the list as hex.
    pub name: String,
    pub kind: Kind,
    pub source: Source,
    /// Linear RGBA, straight alpha; 1 to [`MAX_COLOURS`] of them.
    pub colours: Vec<[f64; 4]>,
}

impl Palette {
    /// The colours as `#rrggbb` (sRGB; `#rrggbbaa` where alpha is not 1).
    pub fn hex(&self) -> Vec<String> {
        self.colours.iter().map(|c| hex_of(*c)).collect()
    }

    /// The colour at `t` (0..1) along the list, blended between neighbours in linear light;
    /// held beyond the ends.
    pub fn at(&self, t: f64) -> [f64; 4] {
        let n = self.colours.len();
        let (Some(first), Some(last)) = (self.colours.first(), self.colours.last()) else {
            return [0.0; 4];
        };
        if n == 1 || !t.is_finite() || t <= 0.0 {
            return *first;
        }
        if t >= 1.0 {
            return *last;
        }
        let x = t * (n - 1) as f64;
        let i = (x.floor() as usize).min(n - 2);
        let f = x - i as f64;
        let (a, b) = (self.colours.get(i).unwrap_or(first), self.colours.get(i + 1).unwrap_or(last));
        let mut c = *a;
        for (v, end) in c.iter_mut().zip(b) {
            *v += (end - *v) * f;
        }
        c
    }

    /// The colour in place `t` (0..1) when the list is cut into equal bands, one per colour.
    pub fn band(&self, t: f64) -> [f64; 4] {
        let n = self.colours.len();
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let i = ((t * n as f64).floor() as usize).min(n.saturating_sub(1));
        self.colours.get(i).copied().unwrap_or([0.0; 4])
    }
}

/// The built-in set: (name, kind, colours as sRGB hex). The palettes meant for faces (sea,
/// blood, candy) come first; the colormaps viridis, magma, inferno, plasma, cividis
/// and twilight are matplotlib's (CC0), sampled at 16 points.
const BUILTIN: &[(&str, Kind, &str)] = &[
    ("sea", Kind::Palette, "8ad6d1 359fa0 fff0c5 ff8c52"),
    ("blood", Kind::Palette, "31aaa9 f8e0a4 a82020 6c1a1a"),
    ("candy", Kind::Palette, "2391ff fff83f ffcd54 f70063"),
    ("pumpkin", Kind::Palette, "ff7518 ffb347 4b1d52 1b1b2f"),
    ("ember", Kind::Palette, "370617 6a040f d00000 faa307"),
    ("neon", Kind::Palette, "08f7fe 09fbd3 fe53bb f5d300"),
    ("pastel", Kind::Palette, "ffd6e0 c1fba4 7bf1a8 90f1ef"),
    ("forest", Kind::Palette, "1b4332 2d6a4f 52b788 d8f3dc"),
    ("primaries", Kind::Palette, "ff0000 00ff00 0000ff"),
    ("mono", Kind::Palette, "000000 555555 aaaaaa ffffff"),
    ("grey", Kind::Colormap, "000000 ffffff"),
    ("fire", Kind::Colormap, "000000 5c0000 b81d00 ff6a00 ffb300 ffe680 ffffff"),
    ("ice", Kind::Colormap, "000000 0b2a5a 1f5fa0 4fa3e0 9fe3ff ffffff"),
    (
        "viridis",
        Kind::Colormap,
        "440154 481a6c 472f7d 414487 39568c 31688e 2a788e 23888e 1f988b 22a884 35b779 54c568 7ad151 a5db36 d2e21b fde725",
    ),
    (
        "magma",
        Kind::Colormap,
        "000004 0b0924 20114b 3b0f70 57157e 721f81 8c2981 a8327d c43c75 de4968 f1605d fa7f5e fe9f6d febf84 fddea0 fcfdbf",
    ),
    (
        "inferno",
        Kind::Colormap,
        "000004 0c0826 240c4f 420a68 5d126e 781c6d 932667 ae305c c73e4c dd513a ed6925 f8850f fca50a fac62d f2e661 fcffa4",
    ),
    (
        "plasma",
        Kind::Colormap,
        "0d0887 330597 5002a2 6a00a8 8405a7 9c179e b12a90 c33d80 d35171 e16462 ed7953 f68f44 fca636 fec029 f9dc24 f0f921",
    ),
    (
        "cividis",
        Kind::Colormap,
        "00224e 002e6c 1e3a6f 35456c 47516c 575d6d 666970 757575 848279 948e77 a59c74 b7a96e c8b866 dbc75a eed649 fee838",
    ),
    (
        "twilight",
        Kind::Colormap,
        "e2d9e2 bfccd3 8eb1c5 6d90c0 606cb8 5e43a5 531e7c 391146 3b113b 64194b 8e2c50 ac4d51 c0755e cb9f83 d8c5bb e2d9e2",
    ),
];

/// The built-in palettes, in their fixed order.
pub fn builtin() -> &'static [Palette] {
    static SET: OnceLock<Vec<Palette>> = OnceLock::new();
    SET.get_or_init(|| {
        BUILTIN
            .iter()
            .filter_map(|(name, kind, hex)| {
                colours(hex).ok().map(|colours| Palette {
                    name: (*name).to_string(),
                    kind: *kind,
                    source: Source::Builtin,
                    colours,
                })
            })
            .collect()
    })
}

/// Whether `name` is one of the built-in palettes.
pub fn is_builtin(name: &str) -> bool {
    BUILTIN.iter().any(|(n, _, _)| n.eq_ignore_ascii_case(name))
}

/// Whether `text` has the form of a palette name: a letter, then letters, digits, `_` or `-`.
pub fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && text.len() <= MAX_NAME
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// An sRGB-encoded channel (0..1) in linear light.
pub fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A linear channel in sRGB encoding (0..1, clamped).
pub fn to_srgb(c: f64) -> f64 {
    let c = if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 };
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// A linear colour as `#rrggbb`, or `#rrggbbaa` where alpha is not 1.
pub fn hex_of(c: [f64; 4]) -> String {
    let byte = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let [r, g, b, a] = c;
    let (r, g, b) = (byte(to_srgb(r)), byte(to_srgb(g)), byte(to_srgb(b)));
    let a = byte(if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 });
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

fn hex_digits(s: &str) -> Option<Vec<u8>> {
    s.bytes()
        .map(|b| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        })
        .collect()
}

fn colour_of(digits: &[u8]) -> [f64; 4] {
    let byte = |i: usize| {
        let hi = digits.get(i).copied().unwrap_or(15);
        let lo = digits.get(i + 1).copied().unwrap_or(15);
        f64::from(hi * 16 + lo) / 255.0
    };
    let a = if digits.len() == 8 { byte(6) } else { 1.0 };
    [to_linear(byte(0)), to_linear(byte(2)), to_linear(byte(4)), a]
}

/// A list of hex colours (module docs) as linear colours.
pub fn colours(text: &str) -> Result<Vec<[f64; 4]>, String> {
    if text.len() > MAX_VALUE {
        return Err(format!("longer than {MAX_VALUE} bytes"));
    }
    // A pasted URL: the colours are its last part.
    let text = match text.trim().rsplit_once('/') {
        Some((_, last)) if text.contains("://") => last,
        _ => text,
    };
    let mut out = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '-')) {
        let token = token.trim_start_matches('#');
        if token.is_empty() {
            continue;
        }
        let digits = hex_digits(token).ok_or_else(|| {
            format!("`{token}` is no hex colour (write 6 hex digits per colour, e.g. 31aaa9)")
        })?;
        let per = match digits.len() {
            8 => 8,
            n if n % 6 == 0 => 6,
            _ => {
                return Err(format!("`{token}` is no hex colour (6 digits per colour, 8 with alpha)"));
            }
        };
        for chunk in digits.chunks(per) {
            out.push(colour_of(chunk));
            if out.len() > MAX_COLOURS {
                return Err(format!("more than {MAX_COLOURS} colours"));
            }
        }
    }
    if out.is_empty() {
        return Err("no colours".into());
    }
    Ok(out)
}

/// What a palette param's value asks for, before names are looked up.
#[derive(Clone, Debug, PartialEq)]
pub enum Wanted {
    None,
    /// A name, or something that may be one (a word of hex digits that is not a name in use
    /// is taken as colours).
    Name(String),
    Colours(Vec<[f64; 4]>),
    /// A place in the full list (built-in first, then the project's), wrapping.
    Index(i64),
}

/// What `value` asks for, or why it is no palette value. Accepts text and numbers (an int, or
/// a float a channel gave, rounded).
pub fn wanted(value: &Value) -> Result<Wanted, String> {
    match value {
        Value::Text(t) => {
            let t = t.trim();
            if t.is_empty() {
                Ok(Wanted::None)
            } else if is_name(t) {
                Ok(Wanted::Name(t.to_string()))
            } else {
                colours(t).map(Wanted::Colours)
            }
        }
        Value::Int(i) => Ok(Wanted::Index(*i)),
        Value::Float(f) if f.is_finite() => Ok(Wanted::Index(f.round() as i64)),
        _ => Err("a palette is a name, a list of hex colours or a number".into()),
    }
}

/// Resolves `value` against the built-in set and `project` (the project's own, [`parse_file`]).
/// `Ok(None)` for an empty value. Names are matched ignoring case.
pub fn resolve(value: &Value, project: &[Palette]) -> Result<Option<Palette>, String> {
    match wanted(value)? {
        Wanted::None => Ok(None),
        Wanted::Colours(colours) => Ok(Some(inline(colours))),
        Wanted::Index(i) => {
            let n = (builtin().len() + project.len()) as i64;
            if n == 0 {
                return Ok(None);
            }
            Ok(builtin().iter().chain(project).nth(i.rem_euclid(n) as usize).cloned())
        }
        Wanted::Name(name) => {
            if let Some(p) = find(&name, project) {
                return Ok(Some(p));
            }
            match colours(&name) {
                Ok(c) => Ok(Some(inline(c))),
                Err(_) => {
                    let mut names: Vec<String> = all(project).into_iter().map(|p| p.name).collect();
                    names.truncate(40);
                    Err(format!("no palette `{name}`; palettes: {}", names.join(", ")))
                }
            }
        }
    }
}

fn inline(colours: Vec<[f64; 4]>) -> Palette {
    let name = colours.iter().map(|c| hex_of(*c).trim_start_matches('#').to_string()).collect::<Vec<_>>();
    Palette { name: name.join(" "), kind: Kind::Palette, source: Source::Inline, colours }
}

fn find(name: &str, project: &[Palette]) -> Option<Palette> {
    builtin().iter().chain(project).find(|p| p.name.eq_ignore_ascii_case(name)).cloned()
}

/// Every palette in list order: the built-in set, then `project`'s.
pub fn all(project: &[Palette]) -> Vec<Palette> {
    let mut out = builtin().to_vec();
    out.extend(project.iter().cloned());
    out
}

/// The palettes a project file defines, and what was wrong with lines that were skipped.
///
/// One palette per line, `name: colours` (`name = colours` works too), colours as in the
/// module docs. Blank lines and lines starting with `#` or `//` are comments. A bad line, a
/// name of the built-in set or one given twice is skipped with a note; the rest loads.
pub fn parse_file(text: &str) -> (Vec<Palette>, Vec<String>) {
    let mut out: Vec<Palette> = Vec::new();
    let mut notes = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let at = i + 1;
        let Some((name, list)) = line.split_once([':', '=']) else {
            notes.push(format!("line {at}: write `name: hex hex hex`"));
            continue;
        };
        let name = name.trim();
        if !is_name(name) {
            notes.push(format!(
                "line {at}: `{name}` is no palette name (a letter, then letters, digits, _ or -, at most {MAX_NAME})"
            ));
            continue;
        }
        if is_builtin(name) {
            notes.push(format!("line {at}: `{name}` is a built-in palette; give yours another name"));
            continue;
        }
        if out.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            notes.push(format!("line {at}: `{name}` is defined twice; the first one counts"));
            continue;
        }
        match colours(list) {
            Ok(colours) => {
                out.push(Palette {
                    name: name.to_string(),
                    kind: Kind::Palette,
                    source: Source::Project,
                    colours,
                });
            }
            Err(e) => notes.push(format!("line {at}: {e}")),
        }
        if out.len() >= MAX_PALETTES {
            notes.push(format!("more than {MAX_PALETTES} palettes; the rest is skipped"));
            break;
        }
    }
    (out, notes)
}

/// The first line of a new palettes file.
const FILE_HEADER: &str = "# This project's own palettes, one per line: name: hex colours (sRGB)\n";

/// The name a line of a palettes file defines, if it has the form of a definition.
fn line_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') || line.starts_with("//") {
        return None;
    }
    line.split_once([':', '=']).map(|(n, _)| n.trim())
}

/// `text` (a palettes file, empty for a new one) with palette `name` set to `colours`, or
/// removed for `None`: the first line defining it is replaced, any further ones are dropped; a
/// new one goes at the end. Comments and every other line stay as they are.
pub fn edit_file(text: &str, name: &str, colours: Option<&[[f64; 4]]>) -> String {
    let new_line = colours.map(|c| {
        let hex: Vec<String> = c.iter().map(|c| hex_of(*c)).collect();
        format!("{name}: {}", hex.join(" "))
    });
    let mut out = String::new();
    let mut placed = false;
    for line in text.lines() {
        if line_name(line).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            if let (Some(l), false) = (&new_line, placed) {
                out.push_str(l);
                out.push('\n');
                placed = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if let (Some(l), false) = (&new_line, placed) {
        if out.is_empty() {
            out.push_str(FILE_HEADER);
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn hex_lists_in_every_usual_form() {
        let want = colours("31aaa9 f8e0a4 a82020 6c1a1a").unwrap();
        assert_eq!(want.len(), 4);
        for text in [
            "#31aaa9, #f8e0a4, #a82020, #6c1a1a",
            "31aaa9-f8e0a4-a82020-6c1a1a",
            "https://coolors.co/31aaa9-f8e0a4-a82020-6c1a1a",
            "https://colorhunt.co/palette/31aaa9f8e0a4a820206c1a1a",
            "31AAA9;F8E0A4\n a82020\t6c1a1a",
        ] {
            assert_eq!(colours(text).unwrap(), want, "{text}");
        }
        let half = colours("ff000080").unwrap();
        assert!(close(half[0], [1.0, 0.0, 0.0, 128.0 / 255.0]));
        // sRGB 0x80 is about 0.216 in linear light.
        assert!((colours("808080").unwrap()[0][0] - 0.2158605).abs() < 1e-6);
        for bad in ["", "zzzzzz", "12345", "#12", "31aaa9 nope"] {
            assert!(colours(bad).is_err(), "{bad}");
        }
        assert!(colours(&"ffffff ".repeat(MAX_COLOURS + 1)).is_err());
        assert!(colours(&"f".repeat(MAX_VALUE + 6)).is_err());
    }

    #[test]
    fn hex_goes_back_to_where_it_came_from() {
        let p = resolve(&Value::Text("blood".into()), &[]).unwrap().unwrap();
        assert_eq!(p.hex(), ["#31aaa9", "#f8e0a4", "#a82020", "#6c1a1a"]);
        assert_eq!(hex_of([1.0, 0.0, 0.0, 0.5]), "#ff000080");
    }

    #[test]
    fn every_builtin_parses() {
        assert_eq!(builtin().len(), BUILTIN.len());
        for p in builtin().iter() {
            assert!(is_name(&p.name), "{}", p.name);
            assert!(!p.colours.is_empty() && p.colours.len() <= MAX_COLOURS);
        }
    }

    #[test]
    fn values_resolve_by_name_list_and_index() {
        let (project, notes) = parse_file("mine: 000000 ffffff\n");
        assert!(notes.is_empty());
        let get = |v: Value| resolve(&v, &project).unwrap().unwrap();
        assert_eq!(get(Value::Text("Blood".into())).name, "blood");
        assert_eq!(get(Value::Text("mine".into())).source, Source::Project);
        assert_eq!(get(Value::Int(0)).name, "sea");
        let n = BUILTIN.len() as i64;
        assert_eq!(get(Value::Int(n)).name, "mine");
        assert_eq!(get(Value::Int(n + 1)).name, "sea");
        assert_eq!(get(Value::Int(-1)).name, "mine");
        assert_eq!(get(Value::Float(2.4)).name, "candy");
        // A word of hex digits that names no palette is colours.
        assert_eq!(get(Value::Text("a82020".into())).source, Source::Inline);
        assert_eq!(resolve(&Value::Text("  ".into()), &project), Ok(None));
        let e = resolve(&Value::Text("nosuch".into()), &project).unwrap_err();
        assert!(e.contains("no palette `nosuch`") && e.contains("blood"), "{e}");
        assert!(resolve(&Value::Bool(true), &project).is_err());
    }

    #[test]
    fn files_skip_bad_lines_and_keep_the_rest() {
        let text = "# mine\n\nsunset: #ff5f6d #ffc371\nbad line\n9lives: ffffff\nblood: 000000\n\
                    sunset: 000000\ndusk = 2c3e50 fd746c\nbroken: zz\n";
        let (p, notes) = parse_file(text);
        assert_eq!(p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["sunset", "dusk"]);
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("line 4")));
        assert!(notes.iter().any(|n| n.contains("built-in")));
        let mut file = String::new();
        for q in &p {
            file = edit_file(&file, &q.name, Some(&q.colours));
        }
        let (again, none) = parse_file(&file);
        assert!(none.is_empty());
        assert_eq!(again, p);
        assert!(file.starts_with('#'));
    }

    #[test]
    fn editing_a_file_keeps_everything_else() {
        let text = "# mine\nsunset: ff5f6d ffc371\nbad line\ndusk: 2c3e50\n";
        let red = colours("ff0000").unwrap();
        let set = edit_file(text, "Sunset", Some(&red));
        assert_eq!(set, "# mine\nSunset: #ff0000\nbad line\ndusk: 2c3e50\n");
        let added = edit_file(&set, "dawn", Some(&red));
        assert!(added.ends_with("dusk: 2c3e50\ndawn: #ff0000\n"));
        let removed = edit_file(&added, "dusk", None);
        assert_eq!(removed, "# mine\nSunset: #ff0000\nbad line\ndawn: #ff0000\n");
        assert_eq!(edit_file("", "x", None), "");
    }

    #[test]
    fn blends_and_bands() {
        let p = resolve(&Value::Text("000000 ffffff".into()), &[]).unwrap().unwrap();
        assert!(close(p.at(0.5), [0.5, 0.5, 0.5, 1.0]));
        assert!(close(p.at(-1.0), [0.0, 0.0, 0.0, 1.0]));
        assert!(close(p.at(f64::NAN), [0.0, 0.0, 0.0, 1.0]));
        assert!(close(p.band(0.49), [0.0, 0.0, 0.0, 1.0]));
        assert!(close(p.band(0.5), [1.0, 1.0, 1.0, 1.0]));
        assert!(close(p.band(1.0), [1.0, 1.0, 1.0, 1.0]));
    }
}
