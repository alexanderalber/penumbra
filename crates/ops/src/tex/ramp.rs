//! `tex.ramp`: colours at positions (keys), blended in between, along a direction, around the
//! centre or out from it. The keys become a lookup row on the CPU, uploaded when they change;
//! `ramp.wgsl` spreads it over the picture.

use std::sync::Arc;

use half::f16;
use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::{Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::{MAX_SIDE, gpu, keep_size, side};

const KIND: &str = "tex.ramp";
const SOURCE: &str = include_str!("ramp.wgsl");
/// Texels of the lookup row: finer than 8-bit output can tell apart.
const LUT: usize = 1024;
/// Most keys; a ramp with more is a picture, better made as one.
const MAX_KEYS: usize = 256;

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Colours at positions, blended in between: a gradient")
        .param(ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels))
        .param(ParamSpec::int("height", 1080).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels))
        .param(keep_size())
        .param(ParamSpec::text("keys", "0 0 0 0 1\n1 1 1 1 1").multiline().doc(
            "One key per line: position (0 to 1), then r g b and optionally a (linear, straight alpha; \
             1 when left out)",
        ))
        .param(ParamSpec::choice("shape", &["horizontal", "vertical", "radial", "circular"]).doc(
            "Where 0 and 1 are: left to right, bottom to top, centre to the middle of the top edge, \
             once around the centre counter-clockwise from the right",
        ))
        .output("out", DataKind::Tex)
}

fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    gpu.cached_pass(KIND, SOURCE, &desc().params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || {
        Box::new(Ramp { lut: None, out: TexRing::new() }) as Box<dyn Operator>
    }))
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

/// A position and its colour (linear, straight alpha).
type Key = (f64, [f64; 4]);

/// Parses `keys`, sorted by position (stable, so equal positions make a hard step in the order
/// written).
fn keys(text: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let numbers: Result<Vec<f64>, _> = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .collect();
        let numbers = numbers.map_err(|_| format!("line {}: numbers only, `pos r g b a`", i + 1))?;
        let key = match numbers.as_slice() {
            [p, r, g, b] => (*p, [*r, *g, *b, 1.0]),
            [p, r, g, b, a] => (*p, [*r, *g, *b, *a]),
            _ => return Err(format!("line {}: write `pos r g b` or `pos r g b a`", i + 1)),
        };
        if !key.0.is_finite() || key.1.iter().any(|v| !v.is_finite()) {
            return Err(format!("line {}: not a finite number", i + 1));
        }
        keys.push(key);
        if keys.len() > MAX_KEYS {
            return Err(format!("more than {MAX_KEYS} keys"));
        }
    }
    if keys.is_empty() {
        return Err("no keys: write one per line, `pos r g b a`".into());
    }
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(keys)
}

/// The colour at `t`: the keys' colours blended linearly (straight alpha), held beyond the
/// first and the last.
fn at(keys: &[Key], t: f64) -> [f64; 4] {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for pair in keys.windows(2) {
        if let [(p0, c0), (p1, c1)] = pair
            && t <= *p1
        {
            let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
            let mut c = *c0;
            for (v, end) in c.iter_mut().zip(c1) {
                *v += (end - *v) * f;
            }
            return c;
        }
    }
    last.1
}

/// The lookup row as half-float RGBA bytes, premultiplied.
fn row(keys: &[Key]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(LUT * 8);
    for i in 0..LUT {
        let c = at(keys, i as f64 / (LUT - 1) as f64);
        let a = c[3].clamp(0.0, 1.0);
        for v in [c[0] * a, c[1] * a, c[2] * a, a] {
            // Half floats reach 65504; brighter keys are clipped there.
            bytes.extend(f16::from_f64(v.clamp(-65504.0, 65504.0)).to_le_bytes());
        }
    }
    bytes
}

struct Ramp {
    /// The keys text last uploaded, and its row.
    lut: Option<(String, Tex)>,
    out: TexRing,
}

impl Operator for Ramp {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let text = cx.text("keys")?;
        let lut = match &self.lut {
            Some((t, lut)) if t == text => lut.clone(),
            _ => {
                let keys = keys(text).map_err(|e| CookError::failed(format!("`keys`: {e}")))?;
                let mut bytes = row(&keys);
                let lut = gpu.upload_rgba16f(LUT as u32, 1, &mut bytes, KIND)?;
                self.lut = Some((text.to_string(), lut.clone()));
                lut
            }
        };
        let (w, h) = (side(cx, "width")?, side(cx, "height")?);
        let target = self.out.next(gpu, w, h, KIND)?;
        let param = |name: &str| cx.param(name).cloned();
        let input = PassInput { inputs: &[Some(&lut)], seconds: 0.0, frame: 0, param: &param };
        pass(gpu)?.render(gpu, &target, &input);
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }

    fn sets_size(&self, _: &NodeDef, _: &NodeDesc) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{at, keys};

    #[test]
    fn keys_parse_sort_and_blend() {
        let k = keys("# sky\n1 1 1 1\n0, 0, 0, 0, 0\n\n0.5 1 0 0 1").unwrap();
        assert_eq!(k.iter().map(|k| k.0).collect::<Vec<_>>(), [0.0, 0.5, 1.0]);
        assert_eq!(at(&k, 0.25), [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(at(&k, -1.0), [0.0; 4]);
        assert_eq!(at(&k, 2.0), [1.0; 4]);
        assert!(keys("").is_err());
        assert!(keys("0 1 1").is_err());
        assert!(keys("0 a b c").is_err());
        assert!(keys("NaN 1 1 1").is_err());
    }
}
