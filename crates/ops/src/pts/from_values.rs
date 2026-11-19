//! `pts.from_chan` and `pts.from_table`: points from numbers on the CPU, as TouchDesigner's CHOP
//! to POP and DAT to POP make them. Each channel, or each column of a table, sets one number of
//! an attribute; a sample, or a row, is a point. Audio spectra, tracked blobs and hand-typed
//! coordinates become points this way.
//!
//! Names say where a number goes ([`target`]), with no typing for the usual ones: `x y z`,
//! `tx ty tz` and `px py pz` are the position, `r g b a` the colour, `nx ny nz` the normal, `u v`
//! the texture coordinates. `<name>_x`, `<name>.y`, `<name>(2)` and `<name>[2]` name a component
//! of any attribute (TouchDesigner's `P(0)` included), a plain name a one-number attribute. So
//! what `table.from_pts` and `chan.from_pts` write reads back the same.
//!
//! Without a position column the points lie along x from 0 to 1 in their order, so a spectrum
//! makes a line that `pts.math` can lift by its values.

use penumbra_core::{Chans, Error, Table};
use penumbra_gpu::points::{AttrType, canonical, default_of, valid_name};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::upload::{Column, Uploads, bounds_of};
use crate::gpu;

pub const FROM_CHAN: &str = "pts.from_chan";
pub const FROM_TABLE: &str = "pts.from_table";

/// Most points these nodes make.
pub const MAX_POINTS: usize = 1 << 20;

const NAMES_DOC: &str = "x y z, tx ty tz or px py pz place the points; r g b a colour them; nx ny nz, u v and any attribute's <name>_x, <name>.x, <name>(0) set the others; a plain name makes a one-number attribute. Without a position the points lie along x from 0 to 1";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            FROM_CHAN,
            "Points from channels: each sample is a point, each channel one of its numbers, by the channel's name. An audio spectrum, tracked blobs or a few typed values as points",
        )
        .input("in", DataKind::Chan)
        .describe_input("in", NAMES_DOC)
        .output("out", DataKind::Pts),
        || Box::new(FromValues::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            FROM_TABLE,
            "Points from a table: each row after the first is a point, each column one of its numbers, by the name in the first row. Typed coordinates or a list from a file as points",
        )
        .input("in", DataKind::Table)
        .describe_input("in", NAMES_DOC)
        .output("out", DataKind::Pts),
        || Box::new(FromValues::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// Where a channel or column named `name` goes: an attribute and the number of its component.
/// `None` for a name that names no attribute.
pub fn target(name: &str) -> Option<(String, usize)> {
    let n = name.trim();
    let fixed = |attr: &str, k: usize| Some((attr.to_string(), k));
    match n {
        "x" | "tx" | "px" => return fixed("position", 0),
        "y" | "ty" | "py" => return fixed("position", 1),
        "z" | "tz" | "pz" => return fixed("position", 2),
        "r" | "cr" => return fixed("color", 0),
        "g" | "cg" => return fixed("color", 1),
        "b" | "cb" => return fixed("color", 2),
        "a" | "ca" | "alpha" => return fixed("color", 3),
        "nx" => return fixed("normal", 0),
        "ny" => return fixed("normal", 1),
        "nz" => return fixed("normal", 2),
        "u" => return fixed("uv", 0),
        "v" => return fixed("uv", 1),
        // A row's number from table.from_pts or a spreadsheet: not an attribute.
        "index" | "" => return None,
        _ => {}
    }
    let named = |base: &str, k: usize| {
        let base = canonical(base);
        (valid_name(base) && k < 4).then(|| (base.to_string(), k))
    };
    // P(0), Color[2]
    if let Some((base, rest)) = n.split_once(['(', '[']) {
        let k = rest.trim_end_matches([')', ']']).trim().parse::<usize>().ok()?;
        return named(base.trim(), k);
    }
    // velocity_x, color.r
    for sep in ['.', '_'] {
        if let Some((base, c)) = n.rsplit_once(sep)
            && let Some(k) = component(c)
            && let Some(t) = named(base, k)
        {
            return Some(t);
        }
    }
    // P0, N2: a TouchDesigner name of a standard attribute and a digit.
    if let Some(last) = n.chars().last()
        && let Some(k) = last.to_digit(10)
        && let Some(base) = n.get(..n.len() - 1)
        && base != canonical(base)
        && default_of(base).is_some_and(|s| s.ty.components() > 1)
    {
        return named(base, k as usize);
    }
    named(n, 0)
}

/// The number of a component named `c`: x y z w, r g b a, or a digit.
fn component(c: &str) -> Option<usize> {
    match c {
        "x" | "r" | "0" => Some(0),
        "y" | "g" | "1" => Some(1),
        "z" | "b" | "2" => Some(2),
        "w" | "a" | "3" => Some(3),
        _ => None,
    }
}

/// The type an attribute gets from the components named for it: a standard one keeps its own,
/// another is as wide as its highest component (three numbers make a vec3).
fn type_of(name: &str, highest: usize) -> AttrType {
    if let Some(s) = default_of(name) {
        return s.ty;
    }
    match highest {
        0 => AttrType::F32,
        1 => AttrType::Vec2,
        2 => AttrType::Vec3,
        _ => AttrType::Vec4,
    }
}

/// Points from named columns of numbers, each `count` long (`None` where a value is missing),
/// and what to warn about.
pub(crate) fn columns(
    names: &[&str],
    value: &dyn Fn(usize, usize) -> Option<f64>,
    count: usize,
) -> (Vec<Column>, Vec<String>) {
    let mut warnings = Vec::new();
    // (attribute, its sources: (component, column index))
    let mut attrs: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    let mut ignored = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let Some((attr, k)) = target(name) else {
            if !name.trim().is_empty() && name.trim() != "index" {
                ignored.push(name.trim().to_string());
            }
            continue;
        };
        match attrs.iter_mut().find(|(a, _)| *a == attr) {
            Some((_, list)) => list.push((k, i)),
            None => attrs.push((attr, vec![(k, i)])),
        }
    }
    if !ignored.is_empty() {
        warnings.push(format!("{} name no attribute and are left out", ignored.join(", ")));
    }
    let mut out = Vec::new();
    let has_position = attrs.iter().any(|(a, _)| a == "position");
    if !has_position {
        let mut c = Column::new("position", AttrType::Vec3);
        for p in 0..count {
            let x = if count > 1 { p as f32 / (count - 1) as f32 } else { 0.0 };
            c.push([x, 0.0, 0.0, 0.0]);
        }
        out.push(c);
    }
    for (attr, sources) in &attrs {
        let highest = sources.iter().map(|(k, _)| *k).max().unwrap_or(0);
        let ty = type_of(attr, highest);
        let wide = ty.components() as usize;
        let past: Vec<&str> =
            sources.iter().filter(|(k, _)| *k >= wide).filter_map(|(_, i)| names.get(*i).copied()).collect();
        if !past.is_empty() {
            warnings.push(format!("`{attr}` has {wide} numbers: {} left out", past.join(", ")));
        }
        let default = default_of(attr).map_or([0.0; 4], |s| s.default);
        let mut c = Column::new(attr, ty);
        for p in 0..count {
            let mut v = default;
            for (k, i) in sources {
                if let (Some(slot), Some(x)) = (v.get_mut(*k), value(*i, p)) {
                    *slot = x as f32;
                }
            }
            c.push(v);
        }
        out.push(c);
    }
    (out, warnings)
}

#[derive(Default)]
struct FromValues {
    uploads: Uploads,
}

impl Operator for FromValues {
    fn gpu_bytes(&self) -> u64 {
        self.uploads.bytes()
    }

    fn trim(&mut self) {
        self.uploads.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let wired = cx.def().inputs.first().is_some_and(Option::is_some);
        let (cols, mut warnings, count) = if let Some(chans) = cx.input_as::<Chans>(0) {
            let names: Vec<&str> = chans.channels().iter().map(|c| c.name.as_str()).collect();
            let count = chans.channels().first().map_or(0, |c| c.samples.len());
            let value = |i: usize, p: usize| chans.channels().get(i).and_then(|c| c.samples.get(p)).copied();
            let n = count.min(MAX_POINTS);
            let (cols, w) = columns(&names, &value, n);
            (cols, w, (count, n))
        } else if let Some(table) = cx.input_as::<Table>(0) {
            let rows = table.rows();
            let names: Vec<&str> =
                rows.first().map(|r| r.iter().map(String::as_str).collect()).unwrap_or_default();
            let count = rows.len().saturating_sub(1);
            let value = |i: usize, p: usize| {
                rows.get(p + 1).and_then(|r| r.get(i)).and_then(|s| s.trim().parse::<f64>().ok())
            };
            let n = count.min(MAX_POINTS);
            let (cols, w) = columns(&names, &value, n);
            (cols, w, (count, n))
        } else if wired {
            return Err(CookError::Pending);
        } else {
            return Err(CookError::failed("connect channels or a table to `in`"));
        };
        if count.0 > count.1 {
            warnings.push(format!("{} values; the first {MAX_POINTS} become points", count.0));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let bounds = cols.iter().find(|c| c.name == "position").and_then(bounds_of);
        let points = self.uploads.upload(gpu, count.1, &cols, bounds)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
    }
}
