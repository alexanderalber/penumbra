//! `pts.file`: a point cloud from a file (TouchDesigner's Point File In POP): `.ply` (ASCII or
//! binary little endian) with position, normal, colour and any other number per vertex, and
//! `.xyz`, `.csv` or `.txt` with a number per column.
//!
//! The file is read and parsed off the cook thread, again when it is saved. Every size in a
//! file is a claim: the count a header gives is capped by [`MAX_POINTS`] and by what the file
//! really holds, so a short, cut or made-up file gives the points it has (with a warning) and
//! never more.

use std::path::Path;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Points;
#[cfg(test)]
use penumbra_gpu::points::MAX_ATTRS;
use penumbra_gpu::points::{AttrType, valid_name};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use super::from_values::Plan;
use super::upload::{Column, Uploads, bounds_of};
use crate::gpu;

pub const KIND: &str = "pts.file";

/// The extensions the node reads.
pub const EXTENSIONS: &[&str] = &["ply", "xyz", "csv", "txt", "pts"];

/// Most points a file gives.
pub const MAX_POINTS: usize = 1 << 22;

/// Most properties a PLY vertex may have.
pub const MAX_PROPERTIES: usize = 1024;

/// Most words (four bytes) of attribute values a file gives, all points together: 512 MB. The
/// numbers read on the way there (eight bytes each) stay within it as well.
pub const MAX_WORDS: usize = 1 << 27;

/// Largest file read: 4 M points of a few dozen bytes each, with room to spare.
const MAX_FILE_BYTES: u64 = 1 << 30;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "A point cloud from a file: .ply from a scanner or a 3D program, or .xyz and .csv text with a column per number. Updates when the file is saved",
        )
        .param(ParamSpec::file("file", EXTENSIONS).doc(
            "The file, relative to the project directory. .ply: ASCII or binary little endian; x y z place the points, nx ny nz, red green blue (alpha) and every other number of a vertex become attributes. .xyz, .csv, .txt, .pts: a number per column. A file uses one separator: tabs, semicolons, commas or spaces, found in its first lines. With tabs or semicolons a decimal comma reads as a point (1,5 is 1.5). An empty cell takes the attribute's default. A first line of names says what each column is (x y z, r g b, nx ny nz, any name). Without names the columns are x y z r g b; four or seven columns are x y z intensity (r g b), as laser scanners write them, and a first line holding only the number of points is skipped",
        ))
        .output("out", DataKind::Pts)
        .describe_output(
            "out",
            "One point per vertex or line, in the file's order. Colours stored as bytes (0 to 255) are taken as sRGB and made linear, as penumbra's colours are. Points hold at most 32 attributes: position, normal, colour and the other standard ones come first, then the file's others in their order, and the node warns about those left out. Exact bounds",
        ),
        || Box::new(PtsFile::default()) as Box<dyn Operator>,
    ))
}

/// A file's points, ready to upload, and what to warn about.
#[derive(Debug)]
pub(crate) struct Cloud {
    pub count: usize,
    pub columns: Vec<Column>,
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct PtsFile {
    slot: AsyncSlot<std::sync::Arc<Cloud>>,
    watch: Option<FileWatch>,
    uploads: Uploads,
    sent: Option<Points>,
}

impl Operator for PtsFile {
    fn gpu_bytes(&self) -> u64 {
        self.uploads.bytes()
    }

    fn trim(&mut self) {
        self.uploads.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let mut fresh = self.slot.poll();
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| read(&p).map(std::sync::Arc::new)));
        }
        if cx.exact_frames() && self.slot.busy() {
            fresh |= super::program::finish(&mut self.slot);
        }
        if let Some(e) = self.slot.error().filter(|_| !self.slot.busy()) {
            return Err(CookError::failed(e));
        }
        match (fresh, self.slot.latest().cloned(), &self.sent) {
            (true, Some(cloud), _) | (false, Some(cloud), None) => {
                if !cloud.warnings.is_empty() {
                    cx.warn(cloud.warnings.join("; "));
                }
                let bounds = cloud.columns.iter().find(|c| c.name == "position").and_then(bounds_of);
                let points = self.uploads.upload(gpu, cloud.count, &cloud.columns, bounds)?;
                self.sent = Some(points.clone());
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
            }
            (_, None, _) => Err(CookError::Pending),
            (false, Some(cloud), Some(_)) => {
                if !cloud.warnings.is_empty() {
                    cx.warn(cloud.warnings.join("; "));
                }
                Ok(Cooked::Unchanged)
            }
        }
    }
}

/// Reads and parses the file at `path`.
pub(crate) fn read(path: &Path) -> Result<Cloud, String> {
    let name = path.display();
    let len = std::fs::metadata(path).map_err(|e| format!("{name}: {e}"))?.len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{name}: {len} bytes, a point file may have {MAX_FILE_BYTES}"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    parse(ext.as_deref(), &bytes).map_err(|e| format!("{name}: {e}"))
}

/// The points in `bytes` of a file with the extension `ext` (lower case): PLY for `.ply` and for
/// a file whose first line is `ply`, text otherwise.
fn parse(ext: Option<&str>, bytes: &[u8]) -> Result<Cloud, String> {
    let first = bytes.split(|b| *b == b'\n').next().unwrap_or(&[]);
    if ext == Some("ply") || first.trim_ascii() == b"ply" { ply(bytes) } else { text(bytes) }
}

/// The number written in `cell`, NaN and infinity also as C runtimes print them (`-nan(ind)`,
/// `1.#INF`, `1.#QNAN`); `None` for anything else.
fn number(cell: &str) -> Option<f64> {
    if let Ok(v) = cell.parse::<f64>() {
        return Some(v);
    }
    let low = cell.to_ascii_lowercase();
    let (minus, body) = match low.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, low.strip_prefix('+').unwrap_or(&low)),
    };
    let nan = Some(f64::NAN);
    if body.strip_prefix("nan(").is_some_and(|r| r.ends_with(')')) {
        return nan;
    }
    let mark = body.strip_prefix("1.#")?.trim_end_matches('0');
    match mark {
        "inf" => Some(if minus { f64::NEG_INFINITY } else { f64::INFINITY }),
        "ind" | "qnan" | "snan" | "nan" => nan,
        _ => None,
    }
}

/// A number type of a PLY property: its size in bytes and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Num {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Num {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "char" | "int8" => Num::I8,
            "uchar" | "uint8" => Num::U8,
            "short" | "int16" => Num::I16,
            "ushort" | "uint16" => Num::U16,
            "int" | "int32" => Num::I32,
            "uint" | "uint32" => Num::U32,
            "float" | "float32" => Num::F32,
            "double" | "float64" => Num::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Num::I8 | Num::U8 => 1,
            Num::I16 | Num::U16 => 2,
            Num::I32 | Num::U32 | Num::F32 => 4,
            Num::F64 => 8,
        }
    }

    /// The number at the start of `b`, little endian; `None` past the end.
    fn read(self, b: &[u8]) -> Option<f64> {
        let take = |n: usize| b.get(..n);
        Some(match self {
            Num::I8 => f64::from(i8::from_le_bytes(take(1)?.try_into().ok()?)),
            Num::U8 => f64::from(u8::from_le_bytes(take(1)?.try_into().ok()?)),
            Num::I16 => f64::from(i16::from_le_bytes(take(2)?.try_into().ok()?)),
            Num::U16 => f64::from(u16::from_le_bytes(take(2)?.try_into().ok()?)),
            Num::I32 => f64::from(i32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::U32 => f64::from(u32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::F32 => f64::from(f32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::F64 => f64::from_le_bytes(take(8)?.try_into().ok()?),
        })
    }
}

/// An sRGB value stored as a whole number from 0 to `full` (255 for a byte) as linear light.
fn linear(v: f64, full: f64) -> f64 {
    let c = (v / full).clamp(0.0, 1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The attribute and component a PLY property fills; `None` for one left out.
fn ply_target(name: &str) -> Option<(String, usize)> {
    let fixed = |a: &str, k: usize| Some((a.to_string(), k));
    match name {
        "x" => fixed("position", 0),
        "y" => fixed("position", 1),
        "z" => fixed("position", 2),
        "nx" => fixed("normal", 0),
        "ny" => fixed("normal", 1),
        "nz" => fixed("normal", 2),
        "red" | "r" | "diffuse_red" => fixed("color", 0),
        "green" | "g" | "diffuse_green" => fixed("color", 1),
        "blue" | "b" | "diffuse_blue" => fixed("color", 2),
        "alpha" | "a" | "diffuse_alpha" => fixed("color", 3),
        "u" | "s" | "texture_u" => fixed("uv", 0),
        "v" | "t" | "texture_v" => fixed("uv", 1),
        // Any other number: an attribute of its name, where it can be one.
        other => valid_name(other).then(|| (other.to_string(), 0)),
    }
}

/// A PLY file: its vertices' numbers.
fn ply(bytes: &[u8]) -> Result<Cloud, String> {
    // The header is text up to "end_header" and its line break.
    let end = find(bytes, b"end_header").ok_or("no end_header: not a complete PLY header")?;
    let body_at =
        bytes.get(end..).and_then(|rest| rest.iter().position(|b| *b == b'\n')).map(|k| end + k + 1);
    let body_at = body_at.ok_or("the header ends without a line break")?;
    let header = String::from_utf8_lossy(bytes.get(..end).unwrap_or(&[]));
    let mut lines = header.lines().map(str::trim);
    if lines.next() != Some("ply") {
        return Err("a PLY file starts with the line `ply`".into());
    }
    let mut binary = None;
    let mut claimed: Option<usize> = None;
    let mut props: Vec<(String, Num)> = Vec::new();
    let mut in_vertex = false;
    let mut before_vertex = false;
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["format", "ascii", ..] => binary = Some(false),
            ["format", "binary_little_endian", ..] => binary = Some(true),
            ["format", other, ..] => {
                return Err(format!("format {other} is not read: ascii and binary_little_endian are"));
            }
            ["element", "vertex", n] => {
                if claimed.is_some() {
                    return Err("two vertex elements".into());
                }
                claimed = Some(n.parse::<usize>().map_err(|_| format!("`{n}` vertices is no count"))?);
                in_vertex = true;
            }
            ["element", ..] => {
                if claimed.is_none() {
                    before_vertex = true;
                }
                in_vertex = false;
            }
            ["property", "list", ..] if in_vertex => {
                return Err("a vertex with a list property is not read".into());
            }
            ["property", ty, name] if in_vertex => {
                let num = Num::of(ty).ok_or_else(|| format!("property type `{ty}` is not known"))?;
                if props.len() >= MAX_PROPERTIES {
                    return Err(format!("a vertex has more than {MAX_PROPERTIES} properties"));
                }
                props.push(((*name).to_string(), num));
            }
            _ => {}
        }
    }
    let binary = binary.ok_or("the header names no format")?;
    let claimed = claimed.ok_or("the file has no vertex element")?;
    if before_vertex {
        return Err("the vertex element must come first".into());
    }
    if props.is_empty() {
        return Err("the vertices have no properties".into());
    }
    let body = bytes.get(body_at..).unwrap_or(&[]);
    let mut warnings = Vec::new();
    // Where each property goes, and for a colour stored as whole numbers their full scale (255
    // for bytes, 65535 for ushort), to make it linear.
    let targets: Vec<Option<(String, usize, Option<f64>)>> = props
        .iter()
        .map(|(name, num)| {
            ply_target(name).map(|(a, k)| {
                let full = match num {
                    Num::U8 => Some(255.0),
                    Num::U16 => Some(65535.0),
                    _ => None,
                };
                (a.clone(), k, full.filter(|_| a == "color"))
            })
        })
        .collect();
    let names: Vec<String> = targets
        .iter()
        .zip(&props)
        .map(|(t, (raw, _))| match t {
            Some((a, k, _)) if a == "position" || a == "normal" || a == "color" || a == "uv" => {
                format!("{a}.{}", ["x", "y", "z", "w"].get(*k).copied().unwrap_or("x"))
            }
            Some((a, _, _)) => a.clone(),
            None => raw.clone(),
        })
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let plan = Plan::new(&refs)?;
    warnings.extend(plan.warnings.iter().cloned());
    let most = claimed.min(MAX_POINTS);
    if claimed > MAX_POINTS {
        warnings.push(format!("{claimed} vertices; the first {MAX_POINTS} become points"));
    }
    let mut cells = Cells::new(&plan, props.len());
    let wanted = cells.fit(most);
    if wanted < most {
        warnings.push(format!("{}: the first {wanted} vertices become points", cells.limit("vertex")));
    }
    if binary {
        let stride: usize = props.iter().map(|(_, n)| n.size()).sum();
        let there = body.len() / stride.max(1);
        let n = wanted.min(there);
        if n < wanted {
            warnings.push(format!("the header claims {claimed} vertices, the file holds {there}"));
        }
        cells.reserve(n);
        for v in 0..n {
            let mut at = v * stride;
            cells.row();
            for (i, (_, num)) in props.iter().enumerate() {
                if cells.wants(i)
                    && let Some(x) = body.get(at..).and_then(|b| num.read(b))
                {
                    cells.set(i, x);
                }
                at += num.size();
            }
        }
    } else {
        let text = String::from_utf8_lossy(body);
        cells.reserve(room(wanted, body));
        for line in text.lines().filter(|l| !l.trim().is_empty()).take(wanted) {
            cells.row();
            for (i, w) in line.split_whitespace().take(props.len()).enumerate() {
                if cells.wants(i)
                    && let Some(x) = number(w)
                {
                    cells.set(i, x);
                }
            }
        }
        if cells.count() < wanted {
            warnings.push(format!("the header claims {claimed} vertices, the file holds {}", cells.count()));
        }
    }
    // A colour of whole numbers: red, green and blue in sRGB, alpha straight.
    let whole =
        |v: f64, k: usize, full: f64| if k == 3 { (v / full).clamp(0.0, 1.0) } else { linear(v, full) };
    let value = |i: usize, p: usize| {
        let v = cells.get(i, p)?;
        Some(match targets.get(i) {
            Some(Some((_, k, Some(full)))) => whole(v, *k, *full),
            _ => v,
        })
    };
    let count = cells.count();
    let cols = plan.columns(&value, count);
    check(&cols)?;
    Ok(Cloud { count, columns: cols, warnings })
}

/// The points to make room for in the text `body` of a file whose header claims `wanted`: no
/// more than it has lines. The claim is no promise: a file of a few bytes may claim millions.
fn room(wanted: usize, body: &[u8]) -> usize {
    wanted.min(body.iter().filter(|b| **b == b'\n').count() + 1)
}

/// The numbers of the columns a [`Plan`] uses, point after point; every other column is skipped
/// as it is read. Stored as `f64`, so whole numbers past 2^24 (ids) stay exact until the
/// attributes take them; NaN where a value is missing.
struct Cells {
    /// For each column of the file, its place among the used ones.
    at: Vec<Option<usize>>,
    width: usize,
    /// Words of attribute values per point.
    words: usize,
    values: Vec<f64>,
    count: usize,
}

impl Cells {
    fn new(plan: &Plan, columns: usize) -> Self {
        let used = plan.used();
        let mut at = vec![None; columns];
        for (k, i) in used.iter().enumerate() {
            if let Some(a) = at.get_mut(*i) {
                *a = Some(k);
            }
        }
        Self { at, width: used.len(), words: plan.words().max(1), values: Vec::new(), count: 0 }
    }

    /// The most points of `wanted` whose values stay within [`MAX_WORDS`]: the attributes made
    /// of them, and the cells themselves (two words each) while the file is read.
    fn fit(&self, wanted: usize) -> usize {
        let per_point = self.words.max(self.width.saturating_mul(2)).max(1);
        wanted.min(MAX_WORDS / per_point)
    }

    /// What [`Cells::fit`] is bound by, for a warning: the values the points get, or the numbers
    /// read while the file is read (two words each), per `item`.
    fn limit(&self, item: &str) -> String {
        if self.words >= self.width.saturating_mul(2) {
            format!("{} values per {item}", self.words)
        } else {
            format!("{} numbers read per {item}", self.width)
        }
    }

    fn reserve(&mut self, points: usize) {
        self.values.reserve(points.saturating_mul(self.width));
    }

    /// Whether column `i` is used.
    fn wants(&self, i: usize) -> bool {
        self.at.get(i).is_some_and(Option::is_some)
    }

    /// Starts the next point, every value missing.
    fn row(&mut self) {
        self.values.extend(std::iter::repeat_n(f64::NAN, self.width));
        self.count += 1;
    }

    /// Column `i` of the last point.
    fn set(&mut self, i: usize, v: f64) {
        let Some(Some(k)) = self.at.get(i) else { return };
        let Some(base) = self.values.len().checked_sub(self.width) else { return };
        if let Some(slot) = self.values.get_mut(base + k) {
            *slot = v;
        }
    }

    /// Column `i` of point `p`; `None` where it is missing or unused.
    fn get(&self, i: usize, p: usize) -> Option<f64> {
        let k = (*self.at.get(i)?)?;
        let v = *self.values.get(p.checked_mul(self.width)?.checked_add(k)?)?;
        (!v.is_nan()).then_some(v)
    }

    fn count(&self) -> usize {
        self.count
    }
}

/// Where `needle` first starts in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// How the cells of a text file's lines are told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Separator {
    /// Spaces and tabs; a run of them is one gap.
    Space,
    /// One character; two in a row leave an empty cell between them.
    Char(char),
}

impl Separator {
    /// The separator of a file whose first lines are `lines`: a tab, a semicolon or a comma,
    /// the first of them that any of these lines holds, else spaces.
    fn of(lines: &[&str]) -> Self {
        ['\t', ';', ',']
            .into_iter()
            .find(|c| lines.iter().any(|l| l.contains(*c)))
            .map_or(Separator::Space, Separator::Char)
    }

    /// The cells of `line`, quotes taken off.
    fn cells<'a>(self, line: &'a str) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        let clean = |w: &'a str| w.trim().trim_matches('"').trim();
        match self {
            Separator::Space => Box::new(line.split_whitespace().map(clean)),
            Separator::Char(c) => Box::new(line.split(c).map(clean)),
        }
    }

    /// The number in `cell`. A file split by tabs or semicolons may write a decimal comma.
    fn number(self, cell: &str) -> Option<f64> {
        match self {
            Separator::Char('\t' | ';') if cell.contains(',') => number(&cell.replace(',', ".")),
            _ => number(cell),
        }
    }
}

/// A text file: a number per column, an optional first line of names.
fn text(bytes: &[u8]) -> Result<Cloud, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut lines =
        text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"));
    // Room for a count line and a line of names before the points.
    let mut data: Vec<&str> = lines.by_ref().take(MAX_POINTS + 2).collect();
    let more = lines.next().is_some();
    let mut warnings = Vec::new();
    let sep = Separator::of(data.get(..4).unwrap_or(&data));
    // Numbers split by spaces with decimal commas (`1,5 2,5`) look like a file split by commas.
    let spaced = |cell: &str| {
        let parts: Vec<&str> = cell.split_whitespace().collect();
        parts.len() > 1 && parts.iter().all(|p| number(p).is_some())
    };
    if sep == Separator::Char(',') && data.iter().take(4).any(|l| sep.cells(l).any(spaced)) {
        warnings.push(
            "cells between commas hold numbers split by spaces, as `1,5 2,5` does: the file is read as split by commas, so its numbers are wrong. Write decimal points, or tabs or semicolons between the numbers"
                .into(),
        );
    }
    // A .pts file from a laser scanner starts with the number of points on a line of its own:
    // a claim, not a point.
    if let Some(first) = data.first()
        && let [n] = sep.cells(first).filter(|c| !c.is_empty()).take(2).collect::<Vec<_>>().as_slice()
        && n.parse::<u64>().is_ok()
    {
        data.remove(0);
    }
    let first: Vec<&str> =
        data.first().map(|l| sep.cells(l).take(MAX_PROPERTIES + 1).collect()).unwrap_or_default();
    let named = first.iter().any(|w| !w.is_empty() && sep.number(w).is_none());
    if named {
        data.remove(0);
    }
    if data.len() > MAX_POINTS || more {
        warnings.push(format!("more than {MAX_POINTS} lines; the first {MAX_POINTS} become points"));
        data.truncate(MAX_POINTS);
    }
    let widest = data.iter().map(|l| sep.cells(l).take(MAX_PROPERTIES + 1).count()).max().unwrap_or(0);
    let columns = if named { first.len() } else { widest };
    if columns > MAX_PROPERTIES {
        warnings.push(format!("more than {MAX_PROPERTIES} columns: the rest are left out"));
    }
    let columns = columns.min(MAX_PROPERTIES);
    let names: Vec<String> = if named {
        first.iter().take(columns).map(|s| (*s).to_string()).collect()
    } else {
        // Four or seven columns, as laser scanners write them: an intensity after x y z.
        let order: &[&str] = match widest {
            4 => &["x", "y", "z", "intensity"],
            7 => &["x", "y", "z", "intensity", "r", "g", "b"],
            _ => &["x", "y", "z", "r", "g", "b"],
        };
        let mut names: Vec<String> = order.iter().take(columns).map(|s| (*s).to_string()).collect();
        if columns > order.len() {
            warnings.push(format!(
                "{columns} columns without names: the first {} are {}, the rest are left out",
                order.len(),
                order.join(" ")
            ));
            names.extend((order.len()..columns).map(|_| String::new()));
        }
        names
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let plan = Plan::new(&refs)?;
    warnings.extend(plan.warnings.iter().cloned());
    let mut cells = Cells::new(&plan, columns);
    let fit = cells.fit(data.len());
    if fit < data.len() {
        warnings.push(format!("{}: the first {fit} lines become points", cells.limit("point")));
        data.truncate(fit);
    }
    cells.reserve(data.len());
    for line in &data {
        cells.row();
        for (i, w) in sep.cells(line).take(columns).enumerate() {
            if cells.wants(i)
                && let Some(x) = sep.number(w)
            {
                cells.set(i, x);
            }
        }
    }
    let count = cells.count();
    // Colours of 0 to 255 are bytes in sRGB; up to 1 they are taken as they are. Only columns
    // without names are guessed this way.
    let colour: Vec<usize> = if named {
        Vec::new()
    } else {
        refs.iter().enumerate().filter(|(_, n)| ["r", "g", "b"].contains(*n)).map(|(i, _)| i).collect()
    };
    let bytes_colour = (0..count).any(|p| colour.iter().any(|i| cells.get(*i, p).is_some_and(|v| v > 1.0)));
    let value = |i: usize, p: usize| {
        let v = cells.get(i, p)?;
        Some(if bytes_colour && colour.contains(&i) { linear(v, 255.0) } else { v })
    };
    let cols = plan.columns(&value, count);
    check(&cols)?;
    Ok(Cloud { count, columns: cols, warnings })
}

fn check(cols: &[Column]) -> Result<(), String> {
    if cols.iter().any(|c| c.ty == AttrType::Vec4 && c.name == "position") {
        return Err("position has three numbers".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(c: &Column) -> Vec<f32> {
        c.words.iter().map(|w| f32::from_bits(*w)).collect()
    }

    fn col<'a>(cloud: &'a Cloud, name: &str) -> &'a Column {
        cloud.columns.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn an_ascii_ply_gives_its_vertices() {
        let file = "ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\n\
                    property uchar red\nproperty uchar green\nproperty uchar blue\nproperty float intensity\n\
                    element face 1\nproperty list uchar int vertex_indices\nend_header\n\
                    1 2 3 255 0 0 0.5\n4 5 6 0 255 0 0.25\n3 0 1 1\n";
        let c = ply(file.as_bytes()).unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position")), [1.0, 2.0, 3.0, 1.0, 4.0, 5.0, 6.0, 1.0]);
        assert_eq!(floats(col(&c, "color"))[..4], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(floats(col(&c, "intensity")), [0.5, 0.25]);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn a_binary_ply_is_capped_by_what_it_holds() {
        let mut file = b"ply\nformat binary_little_endian 1.0\nelement vertex 1000000\nproperty float x\nproperty float y\nproperty float z\nproperty double nx\nend_header\n".to_vec();
        for v in 0..3 {
            for x in [v as f32, 0.5, -1.0] {
                file.extend(x.to_le_bytes());
            }
            file.extend(1.0f64.to_le_bytes());
        }
        // A cut vertex at the end.
        file.extend([0u8; 5]);
        let c = ply(&file).unwrap();
        assert_eq!(c.count, 3, "the header's claim is not believed");
        assert_eq!(floats(col(&c, "position"))[8..12], [2.0, 0.5, -1.0, 1.0]);
        assert_eq!(floats(col(&c, "normal"))[..3], [1.0, 0.0, 1.0]);
        assert!(c.warnings.iter().any(|w| w.contains("1000000")), "{:?}", c.warnings);
    }

    #[test]
    fn broken_files_are_errors_never_panics() {
        for bad in [
            &b""[..],
            b"ply",
            b"ply\nformat ascii 1.0\n",
            b"ply\nformat binary_big_endian 1.0\nelement vertex 1\nproperty float x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex lots\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex 1\nproperty quad x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex 1\nproperty list uchar int x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement face 1\nelement vertex 1\nproperty float x\nend_header\n",
        ] {
            assert!(ply(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        // Too few numbers on a line: the missing ones are 0, as is text that is no number.
        let c = ply(b"ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nend_header\n1\nq 2\n")
            .unwrap();
        assert_eq!(floats(col(&c, "position")), [1.0, 0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 1.0]);
        assert!(text(b"").is_ok_and(|c| c.count == 0));
        assert!(text(b"\xff\xfe garbage \x00").is_ok());
    }

    /// A name starting with `pts_` would make one of penumbra's working attributes, which most
    /// nodes drop: the file is refused, naming the attribute and another name.
    #[test]
    fn names_starting_with_pts_are_refused() {
        let e = text(b"x y z pts_heat\n1 2 3 4\n").err().unwrap();
        assert!(e.contains("`pts_heat`") && e.contains("such as `heat`"), "{e}");
        let file = b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float pts_wind\nend_header\n1 2\n";
        let e = ply(file).err().unwrap();
        assert!(e.contains("`pts_wind`") && e.contains("such as `wind`"), "{e}");
        // A component of an attribute named `pts` is fine.
        assert!(text(b"pts_x pts_y\n1 2\n").is_ok());
    }

    /// A tiny text body whose header claims millions of vertices makes room for its lines only.
    #[test]
    fn a_claim_of_millions_reserves_room_for_the_lines_there_are() {
        assert_eq!(room(4_000_000, b"1 2 3\n4 5 6\n"), 3);
        assert_eq!(room(2, b"1\n2\n3\n4\n"), 2);
        assert_eq!(room(4_000_000, b""), 1);
    }

    /// The warning that cuts the vertices names what bounds them: many one-number attributes
    /// cost two words each while the file is read, more than the points keep.
    #[test]
    fn the_cut_names_what_bounds_it() {
        let mut head = format!("ply\nformat ascii 1.0\nelement vertex {MAX_POINTS}\nproperty float x\n");
        for k in 0..20 {
            head.push_str(&format!("property float p{k}\n"));
        }
        head.push_str("end_header\n1\n");
        let c = ply(head.as_bytes()).unwrap();
        let cut = c.warnings.iter().find(|w| w.contains("vertices become points")).unwrap();
        assert!(cut.starts_with("21 numbers read per vertex"), "{:?}", c.warnings);
        let mut head = format!("ply\nformat ascii 1.0\nelement vertex {MAX_POINTS}\n");
        for k in 0..20 {
            // The fourth number only: an attribute of four numbers from one column.
            head.push_str(&format!("property float n{k}_w\n"));
        }
        head.push_str("end_header\n1\n");
        let c = ply(head.as_bytes()).unwrap();
        let cut = c.warnings.iter().find(|w| w.contains("vertices become points")).unwrap();
        assert!(cut.contains("values per vertex"), "{:?}", c.warnings);
    }

    #[test]
    fn text_files_take_names_or_x_y_z_r_g_b() {
        let c = text(b"# a comment\n1 2 3 255 255 0\n4 5 6 0 0 0\n").unwrap();
        assert_eq!(c.count, 2);
        let c = text(b"1,2,3,255,255,0\n4,5,6,0,0,0\n").unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position"))[4..7], [4.0, 5.0, 6.0]);
        assert_eq!(floats(col(&c, "color"))[..4], [1.0, 1.0, 0.0, 1.0]);
        let c = text(b"\"x\";\"y\";\"z\";\"weight\"\n1;2;3;0.5\n").unwrap();
        assert_eq!((c.count, floats(col(&c, "weight"))), (1, vec![0.5]));
        // A scanner's .pts: the count first, then x y z intensity r g b.
        let c = text(b"2\n1 2 3 -500 255 0 0\n4 5 6 20 0 0 255\n").unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position"))[..3], [1.0, 2.0, 3.0]);
        assert_eq!(floats(col(&c, "intensity")), [-500.0, 20.0]);
        assert_eq!(floats(col(&c, "color"))[4..8], [0.0, 0.0, 1.0, 1.0]);
    }

    /// A few megabytes that name thousands of attributes for thousands of points: an answer
    /// comes back at once, with no more attributes than points hold.
    #[test]
    fn hostile_names_and_lines_are_answered_quickly() {
        let names = 2000;
        let rows = 500_000;
        let mut head = String::from("ply\nformat ascii 1.0\n");
        head.push_str(&format!("element vertex {rows}\n"));
        for k in 0..names {
            head.push_str(&format!("property float p{k}\n"));
        }
        head.push_str("end_header\n");
        head.push_str(&"1\n".repeat(rows));
        let mut csv = (0..names).map(|k| format!("a{k}")).collect::<Vec<_>>().join(" ");
        csv.push('\n');
        csv.push_str(&"1\n".repeat(rows));
        // One line of ten million numbers.
        let long = "1 ".repeat(10_000_000);
        for (what, bytes) in
            [("ply", head.as_bytes()), ("text", csv.as_bytes()), ("long line", long.as_bytes())]
        {
            let start = std::time::Instant::now();
            let c = if what == "ply" { ply(bytes) } else { text(bytes) };
            let took = start.elapsed();
            assert!(took < std::time::Duration::from_secs(2), "{what}: {took:?}");
            if let Ok(c) = c {
                assert!(c.columns.len() <= MAX_ATTRS, "{what}: {} attributes", c.columns.len());
                let words: usize = c.columns.iter().map(|c| c.words.len()).sum();
                assert!(words <= MAX_WORDS, "{what}: {words} words");
            }
        }
        // More properties than a vertex may have: refused before anything is read.
        let mut many = String::from("ply\nformat ascii 1.0\nelement vertex 1\n");
        for k in 0..=MAX_PROPERTIES {
            many.push_str(&format!("property float p{k}\n"));
        }
        many.push_str("end_header\n1\n");
        assert!(ply(many.as_bytes()).is_err());
    }

    /// A file with more attributes than points hold keeps the standard ones and the first others,
    /// and says what it left out.
    #[test]
    fn many_attributes_keep_the_standard_ones_and_warn() {
        let mut file = String::from("ply\nformat ascii 1.0\nelement vertex 2\n");
        let extra: Vec<String> = (0..40).map(|k| format!("f_rest_{k}")).collect();
        for name in extra.iter().map(String::as_str).chain(["x", "y", "z", "red", "green", "blue"]) {
            let ty = if ["red", "green", "blue"].contains(&name) { "uchar" } else { "float" };
            file.push_str(&format!("property {ty} {name}\n"));
        }
        file.push_str("end_header\n");
        let row = |x: f32| {
            let mut r: Vec<String> = (0..40).map(|k| format!("{k}")).collect();
            r.extend([format!("{x}"), "2".into(), "3".into(), "255".into(), "0".into(), "0".into()]);
            r.join(" ") + "\n"
        };
        file.push_str(&row(1.0));
        file.push_str(&row(4.0));
        let c = ply(file.as_bytes()).unwrap();
        assert_eq!(c.columns.len(), MAX_ATTRS);
        assert_eq!(floats(col(&c, "position"))[4..7], [4.0, 2.0, 3.0]);
        assert_eq!(floats(col(&c, "color"))[..4], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(floats(col(&c, "f_rest_5")), [5.0, 5.0]);
        assert!(c.columns.iter().all(|c| c.name != "f_rest_39"), "the last others are left out");
        assert!(c.warnings.iter().any(|w| w.contains("f_rest_39")), "{:?}", c.warnings);
    }

    /// Text files: one separator per file. Empty cells stay empty, and a file split by
    /// semicolons may write its decimals with a comma.
    #[test]
    fn text_cells_keep_their_columns() {
        let c = text(b"x,y,z\n1,,3\n").unwrap();
        assert_eq!(floats(col(&c, "position"))[..3], [1.0, 0.0, 3.0]);
        let c = text(b"x;y;z\n1,5;2,25;3\n").unwrap();
        assert_eq!(floats(col(&c, "position"))[..3], [1.5, 2.25, 3.0]);
        let c = text(b"1,5;2,25;3\n4;5;6\n").unwrap();
        assert_eq!((c.count, floats(col(&c, "position"))[..3].to_vec()), (2, vec![1.5, 2.25, 3.0]));
        let c = text(b"x\ty\tz\n1,5\t\t3\n").unwrap();
        assert_eq!(floats(col(&c, "position"))[..3], [1.5, 0.0, 3.0]);
        // Spaces: runs of them are one gap.
        let c = text(b"1   2  3\n").unwrap();
        assert_eq!(floats(col(&c, "position"))[..3], [1.0, 2.0, 3.0]);
        // An empty cell of a standard attribute takes its default: scale 1.
        let c = text(b"x,scale\n1,\n2,0.5\n").unwrap();
        assert_eq!(floats(col(&c, "scale")), [1.0, 0.5]);
    }

    /// A file that names one column a thousand times (`x`, or `pos_x` and `P(0)` for the same
    /// number) keeps the last of them: the numbers read stay within [`MAX_WORDS`], where every
    /// column was kept before and a header of a few kilobytes asked for 16 GB.
    #[test]
    fn columns_named_alike_keep_the_values_read_within_max_words() {
        for names in [vec!["x"; MAX_PROPERTIES], vec!["position.x"; MAX_PROPERTIES]] {
            let plan = Plan::new(&names).unwrap();
            let mut cells = Cells::new(&plan, MAX_PROPERTIES);
            let n = cells.fit(MAX_POINTS);
            cells.reserve(n);
            let bytes = cells.values.capacity() * std::mem::size_of::<f64>();
            assert!(bytes <= MAX_WORDS * 4, "{n} points of {} cells: {bytes} bytes", cells.width);
            assert!(plan.warnings.iter().any(|w| w.contains("1024 columns")), "{:?}", plan.warnings);
        }
        let mut head = format!("ply\nformat ascii 1.0\nelement vertex {MAX_POINTS}\n");
        for _ in 0..MAX_PROPERTIES {
            head.push_str("property float x\n");
        }
        head.push_str("end_header\n");
        head.push_str(&(0..MAX_PROPERTIES).map(|k| k.to_string()).collect::<Vec<_>>().join(" "));
        head.push('\n');
        let c = ply(head.as_bytes()).unwrap();
        assert_eq!(c.count, 1);
        assert_eq!(floats(col(&c, "position"))[0], (MAX_PROPERTIES - 1) as f32, "the last column");
        assert!(c.warnings.iter().any(|w| w.contains("the last of them")), "{:?}", c.warnings);
    }

    /// Whole numbers past 2^24, where a float loses the last digits, come through exact.
    #[test]
    fn large_ids_from_a_file_stay_exact() {
        let c = text(b"x,id\n0,16777217\n0,4000000001\n").unwrap();
        assert_eq!(col(&c, "id").words, [16_777_217, 4_000_000_001]);
        let c = ply(b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty uint id\nend_header\n0 4000000001\n")
            .unwrap();
        assert_eq!(col(&c, "id").words, [4_000_000_001]);
    }

    /// Numbers split by spaces with decimal commas read as split by commas, and the node says so.
    #[test]
    fn spaces_and_decimal_commas_warn() {
        let c = text(b"1,5 2,5 3,5\n4,5 5,5 6,5\n").unwrap();
        assert!(c.warnings.iter().any(|w| w.contains("`1,5 2,5`")), "{:?}", c.warnings);
        // A plain file split by commas, with spaces after them, says nothing.
        let c = text(b"x, y, z\n1, 2, 3\n").unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    /// A text file whose first name starts with `ply` is text: only a first line `ply` alone
    /// makes a PLY file.
    #[test]
    fn a_text_file_starting_with_ply_is_text() {
        let c = parse(Some("csv"), b"plyId,x,y\n7,2,3\n").unwrap();
        assert_eq!((c.count, floats(col(&c, "position"))[..2].to_vec()), (1, vec![2.0, 3.0]));
        let ply = b"ply\r\nformat ascii 1.0\r\nelement vertex 1\r\nproperty float x\r\nend_header\r\n4\r\n";
        let c = parse(Some("txt"), ply).unwrap();
        assert_eq!(floats(col(&c, "position"))[0], 4.0);
    }

    /// Colours stored as ushort (0 to 65535) are made linear on their own scale.
    #[test]
    fn ushort_colours_reach_one() {
        let file = b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty ushort red\n\
                     property ushort green\nproperty ushort blue\nproperty ushort alpha\nend_header\n\
                     0 65535 0 32768 65535\n";
        let c = ply(file).unwrap();
        let rgba = floats(col(&c, "color"));
        assert_eq!((rgba[0], rgba[1], rgba[3]), (1.0, 0.0, 1.0));
        assert!((rgba[2] - 0.214).abs() < 1e-3, "sRGB 0.5 is about 0.214 linear: {}", rgba[2]);
    }

    /// NaN and infinity as C runtimes print them are numbers: a first row holding them is data,
    /// not names. A NaN takes the default; an infinity stays.
    #[test]
    fn nan_and_infinity_as_c_prints_them_are_numbers() {
        let c = text(b"-nan(ind),1.#INF,3\n1,2,3\n").unwrap();
        assert_eq!(c.count, 2, "{:?}", c.warnings);
        let p = floats(col(&c, "position"));
        assert_eq!((p[0], p[1], p[2]), (0.0, f32::INFINITY, 3.0));
        for cell in ["nan", "-1.#IND", "1.#QNAN0", "-1.#INF00", "NaN(snan)", "inf"] {
            assert!(number(cell).is_some(), "{cell}");
        }
        for cell in ["nancy", "1.#", "#inf", "info"] {
            assert!(number(cell).is_none(), "{cell}");
        }
    }
}
