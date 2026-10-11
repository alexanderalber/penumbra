//! The shape layer, as pure data: tagged rectangles, ellipses and polygons in surface coordinates,
//! a grid repeat per shape, the text format they are stored in, the table they are exported as, and
//! the edits the viewer makes.
//!
//! **Surface coordinates** are those of the surface texture: 0..1 across its width and down its
//! height, origin top left (as a mapper's `region`). A rectangle 0.1 wide and 0.1 high is square
//! only on a square surface; the editor draws at the surface's aspect, so what looks square is.
//!
//! One shape per line:
//!
//! ```text
//! # windows, three floors of four
//! window rect 0.1,0.2 0.05,0.08 repeat 4x3 0.12,0.2
//! door ellipse 0.45,0.7 0.1,0.25
//! ledge poly 0.1,0.5 0.9,0.5 0.9,0.52 0.1,0.52
//! ```
//!
//! The first word is the tag (a name: letters, digits, `_`), the second the kind. A rectangle or
//! ellipse gives its top left corner and its size (the ellipse fills that box), a polygon its
//! points. `repeat CxR dx,dy` makes C columns and R rows of copies, `dx`, `dy` apart, so a
//! facade's grid of windows is one line that stays one line while its spacing is adjusted.

use penumbra_core::{Name, Table};

use crate::mapping::number;

/// Most shapes after repeats are expanded.
pub const MAX_INSTANCES: usize = 4096;
/// Most points of one polygon.
pub const MAX_POINTS: usize = 1024;
/// Most copies along one side of a repeat.
pub const MAX_REPEAT: u32 = 256;
/// Largest coordinate, in surface sizes: shapes may reach beyond the surface, not to infinity.
pub const MAX_COORD: f64 = 4.0;
/// Points on an ellipse's outline when it is drawn as a polygon.
pub const ELLIPSE_POINTS: usize = 64;
/// Tag colours, by the order in which tags first appear (sRGB-ish, bright on black).
pub const PALETTE: [[f32; 3]; 8] = [
    [1.0, 0.8, 0.3],
    [0.3, 0.85, 1.0],
    [1.0, 0.4, 0.6],
    [0.5, 1.0, 0.4],
    [0.75, 0.55, 1.0],
    [1.0, 0.6, 0.25],
    [0.3, 1.0, 0.85],
    [0.95, 0.95, 0.95],
];

#[derive(Clone, Debug, PartialEq)]
pub enum Geom {
    /// x, y, width, height; width and height never negative.
    Rect([f64; 4]),
    /// The ellipse filling this box.
    Ellipse([f64; 4]),
    Poly(Vec<[f64; 2]>),
}

/// Copies of a shape on a grid: `cols` x `rows`, `step` apart (the original is column 0, row 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Repeat {
    pub cols: u32,
    pub rows: u32,
    pub step: [f64; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub tag: String,
    pub geom: Geom,
    pub repeat: Option<Repeat>,
}

/// One shape after repeats: what the table lists and the masks draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    /// The line it comes from.
    pub shape: usize,
    pub col: u32,
    pub row: u32,
    pub tag: String,
    pub geom: Geom,
}

impl Geom {
    pub fn kind(&self) -> &'static str {
        match self {
            Geom::Rect(_) => "rect",
            Geom::Ellipse(_) => "ellipse",
            Geom::Poly(_) => "poly",
        }
    }

    /// x, y, width, height of the bounding box.
    pub fn bounds(&self) -> [f64; 4] {
        match self {
            Geom::Rect(r) | Geom::Ellipse(r) => *r,
            Geom::Poly(points) => {
                let mut lo = [f64::INFINITY; 2];
                let mut hi = [f64::NEG_INFINITY; 2];
                for p in points {
                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                }
                if points.is_empty() { [0.0; 4] } else { [lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]] }
            }
        }
    }

    /// The centre of its area (of the box for rectangles and ellipses).
    pub fn centroid(&self) -> [f64; 2] {
        match self {
            Geom::Rect([x, y, w, h]) | Geom::Ellipse([x, y, w, h]) => [x + w / 2.0, y + h / 2.0],
            Geom::Poly(points) => {
                let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
                for (p, q) in points.iter().zip(points.iter().cycle().skip(1)) {
                    let cross = p[0] * q[1] - q[0] * p[1];
                    a += cross;
                    cx += (p[0] + q[0]) * cross;
                    cy += (p[1] + q[1]) * cross;
                }
                if a.abs() > 1e-12 {
                    [cx / (3.0 * a), cy / (3.0 * a)]
                } else {
                    let [x, y, w, h] = self.bounds();
                    [x + w / 2.0, y + h / 2.0]
                }
            }
        }
    }

    pub fn translated(&self, d: [f64; 2]) -> Geom {
        match self {
            Geom::Rect([x, y, w, h]) => Geom::Rect([x + d[0], y + d[1], *w, *h]),
            Geom::Ellipse([x, y, w, h]) => Geom::Ellipse([x + d[0], y + d[1], *w, *h]),
            Geom::Poly(points) => Geom::Poly(points.iter().map(|p| [p[0] + d[0], p[1] + d[1]]).collect()),
        }
    }

    /// The outline as a closed polygon (the last point is not repeated).
    pub fn outline(&self) -> Vec<[f64; 2]> {
        match self {
            Geom::Rect([x, y, w, h]) => vec![[*x, *y], [x + w, *y], [x + w, y + h], [*x, y + h]],
            Geom::Ellipse([x, y, w, h]) => {
                let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
                (0..ELLIPSE_POINTS)
                    .map(|i| {
                        let a = i as f64 / ELLIPSE_POINTS as f64 * std::f64::consts::TAU;
                        [cx + rx * a.cos(), cy + ry * a.sin()]
                    })
                    .collect()
            }
            Geom::Poly(points) => points.clone(),
        }
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        match self {
            Geom::Rect([x, y, w, h]) => p[0] >= *x && p[0] <= x + w && p[1] >= *y && p[1] <= y + h,
            Geom::Ellipse([x, y, w, h]) => {
                if *w <= 0.0 || *h <= 0.0 {
                    return false;
                }
                let u = (p[0] - (x + w / 2.0)) / (w / 2.0);
                let v = (p[1] - (y + h / 2.0)) / (h / 2.0);
                u * u + v * v <= 1.0
            }
            Geom::Poly(points) => {
                let mut inside = false;
                for (a, b) in points.iter().zip(points.iter().cycle().skip(1)) {
                    if (a[1] > p[1]) != (b[1] > p[1]) {
                        let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                        if p[0] < x {
                            inside = !inside;
                        }
                    }
                }
                inside
            }
        }
    }
}

/// A box from two opposite corners, in either order.
pub fn rect_from(a: [f64; 2], b: [f64; 2]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()]
}

impl Shape {
    /// The copies' offsets from the original, row by row.
    pub fn offsets(&self) -> Vec<(u32, u32, [f64; 2])> {
        let r = self.repeat.unwrap_or(Repeat { cols: 1, rows: 1, step: [0.0, 0.0] });
        let mut out = Vec::new();
        for row in 0..r.rows {
            for col in 0..r.cols {
                out.push((col, row, [f64::from(col) * r.step[0], f64::from(row) * r.step[1]]));
            }
        }
        out
    }

    /// Where the spacing handle of a repeat sits: the centre of the last copy.
    pub fn spacing_anchor(&self) -> Option<[f64; 2]> {
        let r = self.repeat?;
        let c = self.geom.centroid();
        Some([
            c[0] + f64::from(r.cols.saturating_sub(1)) * r.step[0],
            c[1] + f64::from(r.rows.saturating_sub(1)) * r.step[1],
        ])
    }

    /// Sets the spacing so the last copy's centre lands at `p` (along the sides that repeat).
    pub fn set_spacing_to(&mut self, p: [f64; 2]) {
        let c = self.geom.centroid();
        if let Some(r) = &mut self.repeat {
            if r.cols > 1 {
                r.step[0] = clamp_coord((p[0] - c[0]) / f64::from(r.cols - 1));
            }
            if r.rows > 1 {
                r.step[1] = clamp_coord((p[1] - c[1]) / f64::from(r.rows - 1));
            }
        }
    }

    /// Changes the grid to `cols` x `rows` (1 x 1 removes it). A new grid starts with copies one
    /// and a half sizes apart, so they do not overlap.
    pub fn set_repeat(&mut self, cols: u32, rows: u32) {
        let cols = cols.clamp(1, MAX_REPEAT);
        let rows = rows.clamp(1, MAX_REPEAT);
        if cols == 1 && rows == 1 {
            self.repeat = None;
            return;
        }
        let [_, _, w, h] = self.geom.bounds();
        let step = self.repeat.map_or([w * 1.5, h * 1.5], |r| r.step);
        self.repeat = Some(Repeat { cols, rows, step });
    }

    /// Every copy as a shape of its own.
    pub fn explode(&self) -> Vec<Shape> {
        self.offsets()
            .into_iter()
            .map(|(_, _, d)| Shape { tag: self.tag.clone(), geom: self.geom.translated(d), repeat: None })
            .collect()
    }

    /// The points a drag can grab, in surface coordinates: a box's four corners then its four
    /// edge midpoints (top, right, bottom, left), or a polygon's points.
    pub fn handles(&self) -> Vec<[f64; 2]> {
        match &self.geom {
            Geom::Rect([x, y, w, h]) | Geom::Ellipse([x, y, w, h]) => {
                let (r, b, cx, cy) = (x + w, y + h, x + w / 2.0, y + h / 2.0);
                vec![[*x, *y], [r, *y], [r, b], [*x, b], [cx, *y], [r, cy], [cx, b], [*x, cy]]
            }
            Geom::Poly(points) => points.clone(),
        }
    }

    /// Moves handle `i` (see [`Shape::handles`]) to `p`. A box keeps the opposite corner or edge
    /// where it is and may flip over it.
    pub fn set_handle(&mut self, i: usize, p: [f64; 2]) {
        let p = [clamp_coord(p[0]), clamp_coord(p[1])];
        match &mut self.geom {
            Geom::Rect(r) | Geom::Ellipse(r) => {
                let [x, y, w, h] = *r;
                let (left, top, right, bottom) = (x, y, x + w, y + h);
                let (l, t, rr, b) = match i {
                    0 => (p[0], p[1], right, bottom),
                    1 => (left, p[1], p[0], bottom),
                    2 => (left, top, p[0], p[1]),
                    3 => (p[0], top, right, p[1]),
                    4 => (left, p[1], right, bottom),
                    5 => (left, top, p[0], bottom),
                    6 => (left, top, right, p[1]),
                    7 => (p[0], top, right, bottom),
                    _ => return,
                };
                *r = rect_from([l, t], [rr, b]);
            }
            Geom::Poly(points) => {
                if let Some(q) = points.get_mut(i) {
                    *q = p;
                }
            }
        }
    }

    pub fn move_by(&mut self, d: [f64; 2]) {
        self.geom = self.geom.translated(d);
        self.clamp();
    }

    fn clamp(&mut self) {
        match &mut self.geom {
            Geom::Rect(r) | Geom::Ellipse(r) => {
                for v in r.iter_mut() {
                    *v = clamp_coord(*v);
                }
            }
            Geom::Poly(points) => {
                for p in points.iter_mut() {
                    *p = [clamp_coord(p[0]), clamp_coord(p[1])];
                }
            }
        }
    }
}

fn clamp_coord(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 }
}

/// Parses the shapes text; an error names the line.
pub fn parse(text: &str) -> Result<Vec<Shape>, String> {
    let mut shapes = Vec::new();
    let mut instances = 0usize;
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let shape = parse_line(line).map_err(|e| format!("shapes, line {}: {e}", n + 1))?;
        instances += shape.repeat.map_or(1, |r| (r.cols * r.rows) as usize);
        if instances > MAX_INSTANCES {
            return Err(format!("shapes: more than {MAX_INSTANCES} shapes once repeats are counted"));
        }
        shapes.push(shape);
    }
    Ok(shapes)
}

fn parse_line(line: &str) -> Result<Shape, String> {
    let mut words = line.split_whitespace();
    let tag = words.next().unwrap_or_default();
    Name::new(tag).map_err(|e| format!("tag: {e}"))?;
    let kind = words.next().ok_or("a kind (rect, ellipse, poly) after the tag")?;
    let rest: Vec<&str> = words.collect();
    let (geometry, repeat) = match rest.iter().position(|w| *w == "repeat") {
        Some(i) => (rest.get(..i).unwrap_or_default(), Some(rest.get(i + 1..).unwrap_or_default())),
        None => (rest.as_slice(), None),
    };
    let points = geometry.iter().map(|w| pair(w)).collect::<Result<Vec<_>, _>>()?;
    let geom = match kind {
        "rect" | "ellipse" => {
            let &[[x, y], [w, h]] = points.as_slice() else {
                return Err(format!("{kind} takes `x,y w,h`"));
            };
            let r = [x.min(x + w), y.min(y + h), w.abs(), h.abs()];
            if kind == "rect" { Geom::Rect(r) } else { Geom::Ellipse(r) }
        }
        "poly" => {
            if points.len() < 3 || points.len() > MAX_POINTS {
                return Err(format!("poly takes 3 to {MAX_POINTS} points `x,y`"));
            }
            Geom::Poly(points)
        }
        other => return Err(format!("unknown kind `{other}` (rect, ellipse, poly)")),
    };
    let repeat = match repeat {
        None => None,
        Some(&[grid, step]) => {
            let (c, r) = grid.split_once(['x', 'X']).ok_or("repeat takes `CxR dx,dy`")?;
            let count = |s: &str| {
                s.trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=MAX_REPEAT).contains(n))
                    .ok_or(format!("repeat counts are 1 to {MAX_REPEAT}"))
            };
            Some(Repeat { cols: count(c)?, rows: count(r)?, step: pair(step)? })
        }
        Some(_) => return Err("repeat takes `CxR dx,dy`".into()),
    };
    Ok(Shape { tag: tag.to_string(), geom, repeat })
}

fn pair(word: &str) -> Result<[f64; 2], String> {
    let parsed = word.split_once(',').and_then(|(x, y)| {
        let x: f64 = x.trim().parse().ok()?;
        let y: f64 = y.trim().parse().ok()?;
        (x.is_finite() && y.is_finite() && x.abs() <= MAX_COORD && y.abs() <= MAX_COORD).then_some([x, y])
    });
    parsed.ok_or_else(|| format!("`{word}` is not `x,y` (numbers within ±{MAX_COORD})"))
}

fn fmt_pair(p: [f64; 2]) -> String {
    format!("{},{}", number(p[0]), number(p[1]))
}

/// The text of `shapes`, one line each, numbers with at most five decimals.
pub fn to_text(shapes: &[Shape]) -> String {
    let mut out = String::new();
    for s in shapes {
        out.push_str(&s.tag);
        out.push(' ');
        out.push_str(s.geom.kind());
        match &s.geom {
            Geom::Rect([x, y, w, h]) | Geom::Ellipse([x, y, w, h]) => {
                out.push_str(&format!(" {} {}", fmt_pair([*x, *y]), fmt_pair([*w, *h])));
            }
            Geom::Poly(points) => {
                for p in points {
                    out.push(' ');
                    out.push_str(&fmt_pair(*p));
                }
            }
        }
        if let Some(r) = s.repeat {
            out.push_str(&format!(" repeat {}x{} {}", r.cols, r.rows, fmt_pair(r.step)));
        }
        out.push('\n');
    }
    out
}

/// `text` with the line of shape `index` replaced by `with` (one line each; none deletes it).
/// Other lines, comments and blank lines stay as they are; a comment after the replaced shape
/// stays with the first replacement. An index past the last shape appends.
pub fn replace(text: &str, index: usize, with: &[Shape]) -> String {
    let new_lines: Vec<String> =
        with.iter().map(|s| to_text(std::slice::from_ref(s)).trim_end().to_string()).collect();
    let mut out: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut done = false;
    for line in text.lines() {
        let (body, comment) = match line.split_once('#') {
            Some((b, c)) => (b, Some(c)),
            None => (line, None),
        };
        if body.trim().is_empty() {
            out.push(line.to_string());
            continue;
        }
        if n == index {
            done = true;
            for (k, l) in new_lines.iter().enumerate() {
                match comment {
                    Some(c) if k == 0 => out.push(format!("{l}  #{c}")),
                    _ => out.push(l.clone()),
                }
            }
            if new_lines.is_empty()
                && let Some(c) = comment
            {
                out.push(format!("#{c}"));
            }
        } else {
            out.push(line.to_string());
        }
        n += 1;
    }
    if !done {
        out.extend(new_lines);
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Every shape with its repeats expanded, in line order, copies row by row.
pub fn instances(shapes: &[Shape]) -> Vec<Instance> {
    let mut out = Vec::new();
    for (i, s) in shapes.iter().enumerate() {
        for (col, row, d) in s.offsets() {
            if out.len() >= MAX_INSTANCES {
                return out;
            }
            out.push(Instance { shape: i, col, row, tag: s.tag.clone(), geom: s.geom.translated(d) });
        }
    }
    out
}

/// Columns of the exported table, in order (the first row is this header).
pub const COLUMNS: [&str; 13] =
    ["id", "tag", "kind", "x", "y", "w", "h", "cx", "cy", "shape", "col", "row", "points"];

/// The table of `instances`: a header row, then one row each. Positions in surface coordinates;
/// `points` lists a polygon's points as `x,y` separated by spaces (empty for boxes).
pub fn table(instances: &[Instance]) -> Result<Table, penumbra_core::Error> {
    let mut rows = vec![COLUMNS.iter().map(|c| c.to_string()).collect::<Vec<_>>()];
    for (id, inst) in instances.iter().enumerate() {
        let [x, y, w, h] = inst.geom.bounds();
        let [cx, cy] = inst.geom.centroid();
        let points = match &inst.geom {
            Geom::Poly(points) => points.iter().map(|p| fmt_pair(*p)).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        rows.push(vec![
            id.to_string(),
            inst.tag.clone(),
            inst.geom.kind().to_string(),
            number(x),
            number(y),
            number(w),
            number(h),
            number(cx),
            number(cy),
            inst.shape.to_string(),
            inst.col.to_string(),
            inst.row.to_string(),
            points,
        ]);
    }
    Table::new(rows)
}

/// Shapes from a table with a header row naming at least `tag`, `kind`, `x`, `y`, `w`, `h`
/// (and `points` for polygons), such as the one [`table`] makes. Rows are shapes as they are;
/// repeats are already expanded there.
pub fn from_table(t: &Table) -> Result<Vec<Shape>, String> {
    let header = t.rows().first().ok_or("the table is empty: it needs a header row")?;
    let col = |name: &str| header.iter().position(|h| h.trim() == name);
    let need = |name: &str| col(name).ok_or(format!("the table has no `{name}` column"));
    let (tag, kind, x, y, w, h) =
        (need("tag")?, need("kind")?, need("x")?, need("y")?, need("w")?, need("h")?);
    let points = col("points");
    let mut shapes = Vec::new();
    for (r, row) in t.rows().iter().enumerate().skip(1) {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let cell = |c: usize| row.get(c).map_or("", |s| s.trim());
        let line = match cell(kind) {
            "poly" => format!("{} poly {}", cell(tag), points.map_or("", cell)),
            k => format!("{} {k} {},{} {},{}", cell(tag), cell(x), cell(y), cell(w), cell(h)),
        };
        let shape = parse_line(&line).map_err(|e| format!("table row {}: {e}", r + 1))?;
        shapes.push(shape);
        if shapes.len() > MAX_INSTANCES {
            return Err(format!("the table has more than {MAX_INSTANCES} shapes"));
        }
    }
    Ok(shapes)
}

/// The tags a filter text names (separated by spaces or commas); empty matches every tag.
pub fn filter(text: &str) -> Vec<String> {
    text.split([' ', ',', '\t', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub fn matches(filter: &[String], tag: &str) -> bool {
    filter.is_empty() || filter.iter().any(|f| f == tag)
}

/// The tags in order of first appearance; a tag's colour is [`PALETTE`] at its index.
pub fn tags(shapes: &[Shape]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in shapes {
        if !out.contains(&s.tag) {
            out.push(s.tag.clone());
        }
    }
    out
}

pub fn tag_color(tags: &[String], tag: &str) -> [f32; 3] {
    let i = tags.iter().position(|t| t == tag).unwrap_or(0);
    PALETTE.get(i % PALETTE.len()).copied().unwrap_or([1.0; 3])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "# a facade\n\
                        window rect 0.1,0.2 0.05,0.08 repeat 4x3 0.12,0.2\n\
                        door ellipse 0.45,0.7 0.1,0.25\n\
                        ledge poly 0.1,0.5 0.9,0.5 0.9,0.52 0.1,0.52  # under the windows\n";

    #[test]
    fn text_round_trips() {
        let shapes = parse(TEXT).unwrap();
        assert_eq!(shapes.len(), 3);
        assert_eq!(shapes[0].repeat, Some(Repeat { cols: 4, rows: 3, step: [0.12, 0.2] }));
        assert_eq!(shapes[1].geom, Geom::Ellipse([0.45, 0.7, 0.1, 0.25]));
        assert_eq!(parse(&to_text(&shapes)).unwrap(), shapes);
        // A negative size is the same box.
        assert_eq!(parse("a rect 0.5,0.5 -0.25,-0.5").unwrap()[0].geom, Geom::Rect([0.25, 0.0, 0.25, 0.5]));
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn bad_lines_name_the_line() {
        for (text, needle) in [
            ("ok rect 0,0 1,1\n2bad rect 0,0 1,1", "line 2: tag"),
            ("a circle 0,0 1,1", "unknown kind"),
            ("a rect 0,0", "x,y w,h"),
            ("a poly 0,0 1,1", "3 to"),
            ("a rect 0,0 nan,1", "not `x,y`"),
            ("a rect 0,0 9,1", "not `x,y`"),
            ("a rect 0,0 1,1 repeat 0x2 1,1", "1 to 256"),
            ("a rect 0,0 1,1 repeat 2x2", "CxR"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(needle), "{text}: {e}");
        }
        let many = "a rect 0,0 0.1,0.1 repeat 100x50 0.01,0.01";
        assert!(parse(many).unwrap_err().contains("more than"));
    }

    #[test]
    fn instances_and_table() {
        let shapes = parse(TEXT).unwrap();
        let inst = instances(&shapes);
        assert_eq!(inst.len(), 14);
        // Row by row: the fifth window is column 0 of row 1.
        assert_eq!((inst[4].col, inst[4].row), (0, 1));
        assert!(close(inst[4].geom.bounds(), [0.1, 0.4, 0.05, 0.08]));
        let t = table(&inst).unwrap();
        assert_eq!(t.num_rows(), 15);
        assert_eq!(t.rows()[0], COLUMNS.map(String::from).to_vec());
        assert_eq!(t.cell(2, 3), "0.22");
        assert_eq!(t.cell(14, 12), "0.1,0.5 0.9,0.5 0.9,0.52 0.1,0.52");
        // Back from the table: every copy a shape of its own, the same instances.
        let back = from_table(&t).unwrap();
        assert_eq!(back.len(), 14);
        let again = instances(&back);
        let near = |a: [f64; 4], b: [f64; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(
            again.iter().zip(&inst).all(|(a, b)| near(a.geom.bounds(), b.geom.bounds()) && a.tag == b.tag)
        );
        assert!(from_table(&Table::new(vec![vec!["tag".into()]]).unwrap()).unwrap_err().contains("kind"));
    }

    #[test]
    fn replacing_keeps_comments_and_other_lines() {
        let shapes = parse(TEXT).unwrap();
        let mut door = shapes[1].clone();
        door.move_by([0.1, 0.0]);
        let text = replace(TEXT, 1, &[door.clone()]);
        assert!(text.starts_with("# a facade\nwindow rect"));
        assert!(text.contains("door ellipse 0.55,0.7 0.1,0.25\n"));
        assert!(text.contains("# under the windows"));
        let exploded = replace(TEXT, 0, &shapes[0].explode());
        assert_eq!(parse(&exploded).unwrap().len(), 14);
        let deleted = replace(TEXT, 2, &[]);
        assert_eq!(parse(&deleted).unwrap().len(), 2);
        assert!(deleted.contains("# under the windows"));
        let appended = replace("", 5, &[door]);
        assert_eq!(appended, "door ellipse 0.55,0.7 0.1,0.25\n");
    }

    #[test]
    fn geometry() {
        let tri = Geom::Poly(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
        let c = tri.centroid();
        assert!((c[0] - 1.0 / 3.0).abs() < 1e-12 && (c[1] - 1.0 / 3.0).abs() < 1e-12);
        assert!(tri.contains([0.2, 0.2]) && !tri.contains([0.8, 0.8]));
        let e = Geom::Ellipse([0.0, 0.0, 2.0, 1.0]);
        assert!(e.contains([1.0, 0.5]) && e.contains([1.9, 0.5]) && !e.contains([0.1, 0.1]));
        assert_eq!(e.outline().len(), ELLIPSE_POINTS);
    }

    fn close(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-12)
    }

    #[test]
    fn edits() {
        let mut s = parse("w rect 0.1,0.1 0.2,0.2").unwrap().remove(0);
        // Dragging the top left corner past the bottom right flips the box.
        s.set_handle(0, [0.5, 0.4]);
        assert!(close(s.geom.bounds(), [0.3, 0.3, 0.2, 0.1]), "{:?}", s.geom);
        // The right edge only moves the right side.
        s.set_handle(5, [0.6, 0.0]);
        assert!(close(s.geom.bounds(), [0.3, 0.3, 0.3, 0.1]), "{:?}", s.geom);
        s.set_repeat(3, 1);
        let r = s.repeat.unwrap();
        assert!((r.step[0] - 0.45).abs() < 1e-12);
        // The spacing handle at the last copy's centre; moving it spreads the copies.
        assert!((s.spacing_anchor().unwrap()[0] - (0.45 + 0.9)).abs() < 1e-12);
        s.set_spacing_to([0.45 + 0.6, 0.0]);
        assert!((s.repeat.unwrap().step[0] - 0.3).abs() < 1e-12);
        assert!((s.repeat.unwrap().step[1] - 0.15).abs() < 1e-12, "rows do not repeat: untouched");
        let parts = s.explode();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.repeat.is_none()));
        s.set_repeat(1, 1);
        assert!(s.repeat.is_none());
        s.move_by([100.0, 0.0]);
        assert_eq!(s.geom.bounds()[0], MAX_COORD);
    }

    #[test]
    fn filters_and_colours() {
        assert_eq!(filter(" window, door "), vec!["window", "door"]);
        assert!(matches(&[], "x") && !matches(&["a".into()], "b"));
        let shapes = parse(TEXT).unwrap();
        let t = tags(&shapes);
        assert_eq!(t, vec!["window", "door", "ledge"]);
        assert_eq!(tag_color(&t, "door"), PALETTE[1]);
    }
}
