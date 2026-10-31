//! Animation: takes, their layers, and the curves that drive properties over time.
//!
//! A take is an `AnimationStack`. It holds `AnimationLayer`s in order. A layer holds
//! `AnimationCurveNode`s, each driving one property of one object (`Lcl Translation` of a
//! model, the layer's own `Weight`, ...). A curve node has up to three `AnimationCurve`s, one
//! per component (`d|X`, `d|Y`, `d|Z`); a component without a curve keeps the curve node's own
//! value. Key times are counted in ticks: 46186158000 per second in FBX 7.x, 141120000 where
//! the header opts into the FBX 8 unit.
//!
//! Keys interpolate constant (holding the previous or the next key's value), linear or cubic.
//! Cubic keys store their slopes and weights the way the FBX SDK does; auto, TCB and user
//! tangents are solved into one Bezier segment per key pair when the curve is read, so
//! evaluating a curve costs a binary search and a few Newton steps. Outside its keys a curve
//! holds, repeats, repeats with an offset, mirrors or follows its end slope, as its
//! extrapolation says.
//!
//! Layers combine as ufbx combines them: the take's first layer replaces the file's value; each
//! later layer is added (`BlendMode` 0), mixed in by its weight (2, override passthrough) or
//! replaces it outright (1, override). With accumulation mode 0 (the default), rotations mix as
//! quaternions and scales multiply. A layer's `Weight` (percent) may itself be animated. Left
//! out: properties connected to other properties (they keep their animation), animated
//! `RotationOrder`, velocity tangents (also ignored by ufbx), and the layer flags `Mute`,
//! `Solo` and `Lock`.
//!
//! Ported from ufbx's `ufbxi_read_animation_curve`, `ufbx_evaluate_curve`,
//! `ufbxi_extrapolate_curve`, `ufbxi_evaluate_props` and `ufbxi_combine_anim_layer` (Unlicense).

use crate::document::{ConnectionKind, Document};
use crate::transform::{Hierarchy, Mat3, NodeWorld, RotationOrder, TransformProps, euler};
use crate::tree::{Array, Node, Value};
use crate::{Error, Limits};

/// Ticks per second in FBX 7.x.
pub const TICKS_PER_SECOND: i64 = 46_186_158_000;
/// Ticks per second where the header opts into FBX 8's unit (`TCDefinition` other than 127).
pub const TICKS_PER_SECOND_V8: i64 = 141_120_000;

/// How the value moves from a key to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Holds the key's value until the next key.
    ConstantPrevious,
    /// Jumps to the next key's value right after the key.
    ConstantNext,
    Linear,
    /// A Bezier segment shaped by the key's right tangent and the next key's left tangent.
    Cubic,
}

/// A tangent handle relative to its key: `dx` seconds, `dy` value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tangent {
    pub dx: f32,
    pub dy: f32,
}

/// One key: a value at a time (seconds), and how it leads on to the next key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub time: f64,
    pub value: f64,
    pub interpolation: Interpolation,
    pub left: Tangent,
    pub right: Tangent,
}

/// What a curve does before its first or after its last key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Extrapolation {
    /// The end key's value.
    #[default]
    Constant,
    /// The keys again.
    Repeat,
    /// The keys again, each pass shifted by the change over one pass.
    RepeatRelative,
    /// The keys again, every other pass backwards.
    Mirror,
    /// On along the end key's tangent.
    Slope,
}

/// An extrapolation and how often it repeats (`None`: forever).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Beyond {
    pub mode: Extrapolation,
    pub repetitions: Option<u32>,
}

/// One component's keys over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve {
    pub keys: Vec<Key>,
    pub pre: Beyond,
    pub post: Beyond,
    /// The file's ticks per second, for repeating in whole ticks.
    pub ticks_per_second: f64,
}

// Bits of `KeyAttrFlags`.
const CONSTANT: u32 = 0x2;
const CUBIC: u32 = 0x8;
const TANGENT_TCB: u32 = 0x200;
const TANGENT_USER: u32 = 0x400;
const CONSTANT_NEXT: u32 = 0x100;
const CLAMP: u32 = 0x1000;
const TIME_INDEPENDENT: u32 = 0x2000;
const CLAMP_PROGRESSIVE: u32 = 0x4000;
const WEIGHTED_RIGHT: u32 = 0x0100_0000;
const WEIGHTED_NEXT_LEFT: u32 = 0x0200_0000;
/// A tangent weight the file does not state.
const DEFAULT_WEIGHT: f32 = 0.333_333;
/// Slopes of auto tangents within this of a neighbour's value are flattened by `CLAMP`. ufbx
/// leaves it at zero by default.
const CLAMP_THRESHOLD: f64 = 0.0;

fn beyond(node: &Node, name: &str) -> Beyond {
    let Some(child) = node.child(name) else { return Beyond::default() };
    let int = |n: &str| child.child(n).and_then(|c| c.value(0)).and_then(Value::as_i64);
    let Some(code) = int("Type") else { return Beyond::default() };
    let mode = match u8::try_from(code).unwrap_or(0) {
        b'A' => Extrapolation::RepeatRelative,
        b'K' => Extrapolation::Slope,
        b'M' => Extrapolation::Mirror,
        b'R' => Extrapolation::Repeat,
        _ => Extrapolation::Constant,
    };
    let repetitions = int("Repetition").and_then(|r| u32::try_from(r).ok());
    Beyond { mode, repetitions }
}

/// Element `i` of a float array as an `f32` and as the bits of that `f32` (weights are packed
/// into those bits).
fn f32_at(a: &Array, i: usize) -> Option<f32> {
    match a {
        Array::F32(v) => v.get(i).copied(),
        other => other.f64_at(i).map(|x| x as f32),
    }
}

fn auto_tangent(
    (prev_time, time, next_time): (f64, f64, f64),
    (prev_value, value, next_value): (f64, f64, f64),
    (weight_left, weight_right): (f32, f32),
    auto_bias: f32,
    flags: u32,
) -> f32 {
    if flags & CLAMP != 0 && (prev_value - value).abs().min((next_value - value).abs()) <= CLAMP_THRESHOLD {
        return 0.0;
    }
    let mut slope = (next_value - prev_value) / (next_time - prev_time);
    if flags & TIME_INDEPENDENT == 0 {
        let slope_left = (value - prev_value) / (time - prev_time);
        let slope_right = (next_value - value) / (next_time - time);
        let delta = (time - prev_time) / (next_time - prev_time);
        slope = slope * 0.5 + (slope_left * (1.0 - delta) + slope_right * delta) * 0.5;
        let bias = f64::from(auto_bias);
        let bias_weight = bias.abs() / 100.0;
        if bias_weight > 0.0001 {
            let target = if bias > 0.0 { slope_right } else { slope_left };
            let bias_delta = target - slope;
            slope = slope * (1.0 - bias_weight) + target * bias_weight;
            // Beyond 500 the bias adds ((bias - 500) / 100)^2 * 40 outright.
            let extra = bias_weight - 5.0;
            if extra > 0.0 {
                let sign = if bias_delta.abs() > 0.00001 { bias_delta } else { bias };
                slope += extra * extra * if sign > 0.0 { 1.0 } else { -1.0 } * 40.0;
            }
        }
    }
    if flags & CLAMP_PROGRESSIVE != 0 {
        // No overshoot: the handles may not pass either neighbour's value.
        let sign = if slope >= 0.0 { 1.0 } else { -1.0 };
        let mut size = sign * slope;
        let left = f64::from(weight_left) * (time - prev_time);
        let right = f64::from(weight_right) * (next_time - time);
        let max_left = if left > 0.0 { sign * (value - prev_value) / left } else { 0.0 };
        let max_right = if right > 0.0 { sign * (next_value - value) / right } else { 0.0 };
        let max_left = if max_left > 0.0 { max_left } else { 0.0 };
        let max_right = if max_right > 0.0 { max_right } else { 0.0 };
        size = size.min(max_left).min(max_right);
        slope = sign * size;
    }
    slope as f32
}

/// The slope at an end key with one neighbour (`dt` and `dv` towards it, both signed so the
/// slope comes out right).
fn end_tangent(dt: f64, dv: f64, auto_bias: f32, flags: u32) -> f32 {
    if flags & CLAMP_PROGRESSIVE != 0 || (flags & CLAMP != 0 && dv.abs() <= CLAMP_THRESHOLD) {
        return 0.0;
    }
    let mut slope = dv / dt;
    if flags & TIME_INDEPENDENT == 0 {
        let bias = f64::from(auto_bias);
        let extra = bias.abs() / 100.0 - 5.0;
        if extra > 0.0 {
            slope += extra * extra * if bias > 0.0 { 1.0 } else { -1.0 } * 40.0;
        }
    }
    slope as f32
}

impl Curve {
    /// Reads an `AnimationCurve` record.
    pub fn read(node: &Node, ticks_per_second: f64) -> Result<Curve, Error> {
        let pre = beyond(node, "Pre-Extrapolation");
        let post = beyond(node, "Post-Extrapolation");
        let array = |name: &str| node.child_array(name).ok_or_else(|| Error::new(format!("no `{name}`")));
        let times = array("KeyTime")?;
        let values = array("KeyValueFloat")?;
        let flags = array("KeyAttrFlags")?;
        let attrs = array("KeyAttrDataFloat")?;
        let refs = array("KeyAttrRefCount")?;
        let n = times.len();
        if values.len() != n {
            return Err(Error::new("key times and values differ in number"));
        }
        if flags.len() != refs.len() || Some(attrs.len()) != refs.len().checked_mul(4) {
            return Err(Error::new("key attributes do not match their counts"));
        }
        let scale = if ticks_per_second > 0.0 { ticks_per_second } else { TICKS_PER_SECOND as f64 };
        let time = |i: usize| times.i64_at(i).map_or(0.0, |t| t as f64 / scale);
        let value = |i: usize| values.f64_at(i).filter(|v| v.is_finite()).unwrap_or(0.0);
        let mut keys = Vec::with_capacity(n.min(1 << 16));
        // The previous key's handles decide this key's left tangent.
        let mut slope_left = 0.0f32;
        let mut weight_left = DEFAULT_WEIGHT;
        let mut prev_time = 0.0;
        let mut next_time = if n > 0 { time(0) } else { 0.0 };
        let mut group = 0usize;
        let mut refs_left = if n > 0 { refs.i64_at(0).unwrap_or(0) } else { 0 };
        for i in 0..n {
            if refs_left <= 0 {
                return Err(Error::new("the key attributes run out before the keys"));
            }
            let key_time = next_time;
            let key_value = value(i);
            if i + 1 < n {
                next_time = time(i + 1);
            }
            let has_prev = i > 0 && key_time > prev_time;
            let has_next = i + 1 < n && next_time > key_time;
            let prev_value = if i > 0 { value(i - 1) } else { 0.0 };
            let next_value = if i + 1 < n { value(i + 1) } else { 0.0 };
            let f = flags.i64_at(group).unwrap_or(0) as u32;
            let attr = |k: usize| group.checked_mul(4).and_then(|g| f32_at(attrs, g + k)).unwrap_or(0.0);
            let attr = |k: usize| {
                let a = attr(k);
                if a.is_finite() { a } else { 0.0 }
            };
            let mut slope_right = attr(0);
            let mut weight_right = DEFAULT_WEIGHT;
            let mut next_slope_left = attr(1);
            let mut next_weight_left = DEFAULT_WEIGHT;
            if f & (WEIGHTED_RIGHT | WEIGHTED_NEXT_LEFT) != 0 {
                // Two decimal fixed-point weights (four digits after the point) packed into
                // the bits of the third float.
                let packed = group.checked_mul(4).and_then(|g| f32_at(attrs, g + 2)).unwrap_or(0.0).to_bits();
                if f & WEIGHTED_RIGHT != 0 {
                    weight_right = (packed & 0xffff) as f32 * 0.0001;
                }
                if f & WEIGHTED_NEXT_LEFT != 0 {
                    next_weight_left = (packed >> 16) as f32 * 0.0001;
                }
            }
            let interpolation;
            if f & CONSTANT != 0 {
                interpolation = if f & CONSTANT_NEXT != 0 {
                    Interpolation::ConstantNext
                } else {
                    Interpolation::ConstantPrevious
                };
                weight_right = DEFAULT_WEIGHT;
                next_weight_left = DEFAULT_WEIGHT;
                slope_right = 0.0;
                next_slope_left = 0.0;
            } else if f & CUBIC != 0 {
                interpolation = Interpolation::Cubic;
                if f & TANGENT_TCB != 0 {
                    let left = if has_prev { (key_value - prev_value) / (key_time - prev_time) } else { 0.0 };
                    let right =
                        if has_next { (next_value - key_value) / (next_time - key_time) } else { 0.0 };
                    let edge = !has_prev || !has_next;
                    let (tension, continuity, bias) =
                        (f64::from(attr(0)), f64::from(attr(1)), f64::from(attr(2)));
                    let factor = if edge { 1.0 } else { 0.5 };
                    let d00 = factor * (1.0 - tension) * (1.0 + bias) * (1.0 - continuity);
                    let d01 = factor * (1.0 - tension) * (1.0 - bias) * (1.0 + continuity);
                    let d10 = factor * (1.0 - tension) * (1.0 + bias) * (1.0 + continuity);
                    let d11 = factor * (1.0 - tension) * (1.0 - bias) * (1.0 - continuity);
                    slope_left = (d00 * left + d01 * right) as f32;
                    slope_right = (d10 * left + d11 * right) as f32;
                    next_slope_left = 0.0;
                    next_weight_left = DEFAULT_WEIGHT;
                } else if f & TANGENT_USER != 0 {
                    // User tangents: the slopes as stored.
                } else if has_prev && has_next {
                    let times3 = (prev_time, key_time, next_time);
                    let values3 = (prev_value, key_value, next_value);
                    let weights = (weight_left, weight_right);
                    if (slope_left + slope_right).abs() <= 0.0001 {
                        let s = auto_tangent(times3, values3, weights, slope_right, f);
                        slope_left = s;
                        slope_right = s;
                    } else {
                        let l = auto_tangent(times3, values3, weights, -slope_left, f);
                        slope_right = auto_tangent(times3, values3, weights, slope_right, f);
                        slope_left = l;
                    }
                } else if has_prev {
                    let s = end_tangent(key_time - prev_time, key_value - prev_value, -slope_left, f);
                    slope_left = s;
                    slope_right = s;
                } else if has_next {
                    let s = end_tangent(next_time - key_time, next_value - key_value, slope_right, f);
                    slope_left = s;
                    slope_right = s;
                } else {
                    slope_left = 0.0;
                    slope_right = 0.0;
                }
            } else {
                // Linear, or a mode not known: straight to the next key.
                interpolation = Interpolation::Linear;
                weight_right = DEFAULT_WEIGHT;
                next_weight_left = DEFAULT_WEIGHT;
                let s =
                    if has_next { ((next_value - key_value) / (next_time - key_time)) as f32 } else { 0.0 };
                slope_right = s;
                next_slope_left = s;
            }
            let finite = |x: f32| if x.is_finite() { x } else { 0.0 };
            let left = if key_time > prev_time {
                let dx = (f64::from(weight_left) * (key_time - prev_time)) as f32;
                Tangent { dx: finite(dx), dy: finite(dx * slope_left) }
            } else {
                Tangent::default()
            };
            let right = if next_time > key_time {
                let dx = (f64::from(weight_right) * (next_time - key_time)) as f32;
                Tangent { dx: finite(dx), dy: finite(dx * slope_right) }
            } else {
                Tangent::default()
            };
            keys.push(Key { time: key_time, value: key_value, interpolation, left, right });
            slope_left = next_slope_left;
            weight_left = next_weight_left;
            prev_time = key_time;
            refs_left -= 1;
            if refs_left == 0 {
                group += 1;
                refs_left = refs.i64_at(group).unwrap_or(0);
            }
        }
        if keys.windows(2).any(|w| w.get(1).map(|b| b.time) < w.first().map(|a| a.time)) {
            return Err(Error::new("the key times go backwards"));
        }
        Ok(Curve { keys, pre, post, ticks_per_second: scale })
    }

    /// Whether the curve has one value at all times: every key the same, flat tangents.
    pub fn is_constant(&self) -> bool {
        let Some(first) = self.keys.first() else { return true };
        self.keys.iter().all(|k| k.value == first.value && k.left.dy == 0.0 && k.right.dy == 0.0)
    }

    /// The first and the last key's time; `None` without keys.
    pub fn span(&self) -> Option<(f64, f64)> {
        Some((self.keys.first()?.time, self.keys.last()?.time))
    }

    /// The value at `time` (seconds); `default` without keys.
    pub fn evaluate(&self, time: f64, default: f64) -> f64 {
        self.evaluate_inner(time, default, true)
    }

    fn evaluate_inner(&self, time: f64, default: f64, extrapolate: bool) -> f64 {
        let keys = &self.keys;
        let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return default };
        if keys.len() == 1 {
            return first.value;
        }
        if extrapolate && (time < first.time || time > last.time) {
            return self.extrapolate(time);
        }
        let next = keys.partition_point(|k| k.time <= time);
        let (Some(prev), Some(n)) = (next.checked_sub(1).and_then(|p| keys.get(p)), keys.get(next)) else {
            return if next == 0 { first.value } else { last.value };
        };
        if prev.time == time {
            return prev.value;
        }
        let rcp = 1.0 / (n.time - prev.time);
        let t = (time - prev.time) * rcp;
        match prev.interpolation {
            Interpolation::ConstantPrevious => prev.value,
            Interpolation::ConstantNext => n.value,
            Interpolation::Linear => prev.value * (1.0 - t) + n.value * t,
            Interpolation::Cubic => {
                let x1 = f64::from(prev.right.dx) * rcp;
                let x2 = 1.0 - f64::from(n.left.dx) * rcp;
                let t = cubic_bezier_t(x1, x2, t);
                let (t2, t3) = (t * t, t * t * t);
                let u = 1.0 - t;
                let (u2, u3) = (u * u, u * u * u);
                let y0 = prev.value;
                let y3 = n.value;
                let y1 = y0 + f64::from(prev.right.dy);
                let y2 = y3 - f64::from(n.left.dy);
                u3 * y0 + 3.0 * (u2 * t * y1 + u * t2 * y2) + t3 * y3
            }
        }
    }

    fn extrapolate(&self, real_time: f64) -> f64 {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else { return 0.0 };
        let pre = real_time < first.time;
        let (key, ext) = if pre { (first, self.pre) } else { (last, self.post) };
        match ext.mode {
            Extrapolation::Constant => return key.value,
            Extrapolation::Slope => {
                let tangent = if pre { key.right } else { key.left };
                let v = key.value + f64::from(tangent.dy) * ((real_time - key.time) / f64::from(tangent.dx));
                return if v.is_finite() { v } else { key.value };
            }
            _ if ext.repetitions == Some(0) => return key.value,
            _ => {}
        }
        // In whole ticks, so a repeat lands on the frames exactly.
        let scale = self.ticks_per_second;
        let min_time = (first.time * scale).round();
        let max_time = (last.time * scale).round();
        let time = real_time * scale;
        let delta = if pre { min_time - time } else { time - max_time };
        let duration = max_time - min_time;
        if duration.is_nan() || duration < 1.0 {
            return key.value;
        }
        let rep = delta / duration;
        let mut rep_n = rep.floor();
        let mut rep_d = delta - rep_n * duration;
        if let Some(count) = ext.repetitions
            && rep_n >= f64::from(count)
        {
            rep_n = f64::from(count) - 1.0;
            rep_d = duration;
        }
        if ext.mode == Extrapolation::Mirror {
            let parity = rep_n * 0.5 - (rep_n * 0.5).floor();
            if parity <= 0.25 {
                rep_d = duration - rep_d;
            }
        }
        if pre {
            rep_d = duration - rep_d;
        }
        let new_time = (min_time + rep_d) / scale;
        let mut value = self.evaluate_inner(new_time, key.value, false);
        if ext.mode == Extrapolation::RepeatRelative {
            let d = last.value - first.value;
            value += if pre { -d } else { d } * (rep_n + 1.0);
        }
        if value.is_finite() { value } else { key.value }
    }
}

/// The Bezier parameter at which a segment with handles at `p1` and `p2` (as fractions of the
/// segment's time) reaches `x0` (Newton's method, as ufbx does it).
fn cubic_bezier_t(p1: f64, p2: f64, x0: f64) -> f64 {
    let (p1_3, p2_3) = (p1 * 3.0, p2 * 3.0);
    let a = p1_3 - p2_3 + 1.0;
    let b = p2_3 - p1_3 - p1_3;
    let c = p1_3;
    let (a_3, b_2) = (3.0 * a, 2.0 * b);
    let mut t = x0;
    let step = |t: &mut f64| {
        let (t2, t3) = (*t * *t, *t * *t * *t);
        let x1 = a * t3 + b * t2 + c * *t - x0;
        *t -= x1 / (a_3 * t2 + b_2 * *t + c);
        x1
    };
    let mut x1 = 0.0;
    for _ in 0..3 {
        x1 = step(&mut t);
    }
    const EPS: f64 = 8.881_784_197_001_252e-16;
    if x1.abs() <= EPS {
        return t;
    }
    for _ in 0..4 {
        step(&mut t);
        x1 = step(&mut t);
        if x1.abs() <= EPS {
            return t;
        }
    }
    if t.is_finite() { t } else { x0 }
}

/// A curve node: the value of one property over time, up to three components.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveNode {
    pub id: i64,
    /// The components' values where no curve drives them.
    pub default: [f64; 3],
    /// Index into [`Animation::curves`] per component.
    pub curves: [Option<usize>; 3],
}

/// A property a layer drives: `property` of object `target`, by curve node `value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Driven {
    pub target: i64,
    pub property: String,
    /// Index into [`Animation::curve_nodes`].
    pub value: usize,
}

/// How a layer combines with the layers before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blend {
    /// `BlendMode` 0: added, times the weight.
    #[default]
    Additive,
    /// 1: replaces, whatever the weight.
    Override,
    /// 2: mixed in by the weight.
    OverridePassthrough,
}

/// An `AnimationLayer`.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: i64,
    pub name: String,
    /// `Weight` / 100, in 0..1.
    pub weight: f64,
    pub blend: Blend,
    /// Rotations combine as quaternions (`RotationAccumulationMode` 0) rather than per angle.
    pub compose_rotation: bool,
    /// Scales multiply (`ScaleAccumulationMode` 0) rather than add.
    pub compose_scale: bool,
    /// What the layer drives, in connection order. The layer's own `Weight` may be one.
    pub driven: Vec<Driven>,
}

/// A take: an `AnimationStack` with its layers.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub id: i64,
    pub name: String,
    /// Seconds on the file's clock, from `LocalStart` and `LocalStop` (else `ReferenceStart` and
    /// `ReferenceStop`, else the keys' span).
    pub start: f64,
    pub stop: f64,
    /// Indices into [`Animation::layers`], in order.
    pub layers: Vec<usize>,
}

impl Take {
    pub fn duration(&self) -> f64 {
        (self.stop - self.start).max(0.0)
    }
}

/// Everything animated in a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animation {
    pub ticks_per_second: f64,
    /// From `TimeMode` (or `CustomFrameRate`); 24 when the file does not say.
    pub frame_rate: f64,
    pub takes: Vec<Take>,
    pub layers: Vec<Layer>,
    pub curve_nodes: Vec<CurveNode>,
    pub curves: Vec<Curve>,
}

/// Frames per second of each `TimeMode` (14 is custom).
const TIME_MODE_FPS: [f64; 18] = [
    30.0, 120.0, 100.0, 60.0, 50.0, 48.0, 30.0, 30.0, 29.97, 29.97, 25.0, 24.0, 1000.0, 23.976, 24.0, 96.0,
    72.0, 59.94,
];

/// Ticks per second as the file's header states them.
pub fn ticks_per_second(doc: &Document) -> f64 {
    let header = doc.tree.top("FBXHeaderExtension");
    let int = |n: Option<&Node>| n.and_then(|n| n.value(0)).and_then(Value::as_i64);
    let header_version = int(header.and_then(|h| h.child("FBXHeaderVersion"))).unwrap_or(0);
    let tc = int(header.and_then(|h| h.child("OtherFlags")).and_then(|o| o.child("TCDefinition")));
    let v7 = match tc {
        Some(tc) if header_version >= 1004 => tc == 127,
        _ => doc.version() < 8000,
    };
    if v7 { TICKS_PER_SECOND as f64 } else { TICKS_PER_SECOND_V8 as f64 }
}

impl Animation {
    /// Reads the takes, layers, curve nodes and curves of `doc`. A curve that cannot be read
    /// is left out with a warning; more curves or keys than `limits` allow is an error.
    pub fn read(doc: &Document, limits: &Limits, warnings: &mut Vec<String>) -> Result<Animation, Error> {
        let ticks = ticks_per_second(doc);
        let s = &doc.settings;
        let mode = s.int("TimeMode").filter(|m| (0..18).contains(m)).unwrap_or(11);
        let frame_rate = if mode == 14 {
            s.number("CustomFrameRate").filter(|r| *r > 0.0).unwrap_or(24.0)
        } else {
            TIME_MODE_FPS.get(mode as usize).copied().unwrap_or(24.0)
        };
        let mut anim = Animation { ticks_per_second: ticks, frame_rate, ..Animation::default() };
        let mut keys_left = limits.max_keys;
        let mut curve_index = std::collections::HashMap::new();
        let mut node_index = std::collections::HashMap::new();
        let mut layer_index = std::collections::HashMap::new();
        let mut bad_curves = 0usize;
        for o in doc.objects.iter().filter(|o| o.class == "AnimationCurve") {
            if anim.curves.len() >= limits.max_curves {
                return Err(Error::new(format!("more than {} animation curves", limits.max_curves)));
            }
            let Some(node) = doc.node(o) else { continue };
            let n = node.child_array("KeyTime").map_or(0, Array::len);
            keys_left = keys_left
                .checked_sub(n)
                .ok_or_else(|| Error::new(format!("more than {} animation keys", limits.max_keys)))?;
            match Curve::read(node, ticks) {
                Ok(c) => {
                    curve_index.insert(o.id, anim.curves.len());
                    anim.curves.push(c);
                }
                Err(_) => bad_curves += 1,
            }
        }
        if bad_curves > 0 {
            warnings.push(format!("{bad_curves} animation curves could not be read and are left out"));
        }
        for o in doc.objects.iter().filter(|o| o.class == "AnimationCurveNode") {
            let mut node = CurveNode { id: o.id, default: [0.0; 3], curves: [None; 3] };
            for (k, name) in [["X", "d|X"], ["Y", "d|Y"], ["Z", "d|Z"]].iter().enumerate() {
                for n in name {
                    if let (Some(v), Some(slot)) = (o.props.number(n), node.default.get_mut(k)) {
                        *slot = v;
                    }
                }
            }
            for c in doc.connections_into(o.id).filter(|c| c.kind == ConnectionKind::ObjectProperty) {
                let Some(&curve) = curve_index.get(&c.child) else { continue };
                let prop = c.property.as_deref().unwrap_or("");
                let k = match prop {
                    "Y" | "d|Y" => 1,
                    "Z" | "d|Z" => 2,
                    _ => 0,
                };
                if let (Some(v), Some(slot)) = (o.props.number(prop), node.default.get_mut(k)) {
                    *slot = v;
                }
                if let Some(slot) = node.curves.get_mut(k) {
                    *slot = Some(curve);
                }
            }
            node_index.insert(o.id, anim.curve_nodes.len());
            anim.curve_nodes.push(node);
        }
        for o in doc.objects.iter().filter(|o| o.class == "AnimationLayer") {
            let p = &o.props;
            let blend = match p.int("BlendMode").unwrap_or(0) {
                0 => Blend::Additive,
                2 => Blend::OverridePassthrough,
                _ => Blend::Override,
            };
            let weight = p.number("Weight").map_or(1.0, |w| {
                let w = (w / 100.0).max(0.0);
                if w > 0.99999 { 1.0 } else { w }
            });
            let mut driven = Vec::new();
            for cn in doc.sources(o.id, "AnimationCurveNode") {
                let Some(&value) = node_index.get(&cn.id) else { continue };
                for c in doc.connections_from(cn.id).filter(|c| c.kind == ConnectionKind::ObjectProperty) {
                    if let Some(property) = c.property.clone() {
                        driven.push(Driven { target: c.parent, property, value });
                    }
                }
            }
            layer_index.insert(o.id, anim.layers.len());
            anim.layers.push(Layer {
                id: o.id,
                name: o.name.clone(),
                weight,
                blend,
                compose_rotation: p.int("RotationAccumulationMode").unwrap_or(0) == 0,
                compose_scale: p.int("ScaleAccumulationMode").unwrap_or(0) == 0,
                driven,
            });
        }
        for o in doc.objects.iter().filter(|o| o.class == "AnimationStack") {
            let layers: Vec<usize> =
                doc.sources(o.id, "AnimationLayer").filter_map(|l| layer_index.get(&l.id).copied()).collect();
            let p = &o.props;
            let span = |a: &str, b: &str| Some((p.int(a)?, p.int(b)?));
            let (mut start, mut stop) = span("LocalStart", "LocalStop")
                .or_else(|| span("ReferenceStart", "ReferenceStop"))
                .map_or((0.0, 0.0), |(a, b)| (a as f64 / ticks, b as f64 / ticks));
            if stop <= start {
                // A take without a usable span: the span of its keys.
                let spans = layers
                    .iter()
                    .filter_map(|l| anim.layers.get(*l))
                    .flat_map(|l| &l.driven)
                    .filter_map(|d| anim.curve_nodes.get(d.value))
                    .flat_map(|n| n.curves.iter().flatten())
                    .filter_map(|c| anim.curves.get(*c)?.span());
                for (a, b) in spans {
                    if stop <= start {
                        (start, stop) = (a, b);
                    } else {
                        (start, stop) = (start.min(a), stop.max(b));
                    }
                }
            }
            anim.takes.push(Take { id: o.id, name: o.name.clone(), start, stop, layers });
        }
        Ok(anim)
    }

    /// Whether any take drives anything.
    pub fn is_empty(&self) -> bool {
        !self
            .takes
            .iter()
            .any(|t| t.layers.iter().any(|l| self.layers.get(*l).is_some_and(|l| !l.driven.is_empty())))
    }

    /// A curve node's value at `time`.
    pub fn value(&self, curve_node: usize, time: f64) -> [f64; 3] {
        let Some(n) = self.curve_nodes.get(curve_node) else { return [0.0; 3] };
        let mut out = n.default;
        for (o, c) in out.iter_mut().zip(n.curves) {
            if let Some(curve) = c.and_then(|c| self.curves.get(c)) {
                *o = curve.evaluate(time, *o);
            }
        }
        out
    }

    /// A layer's weight at `time`: its `Weight`, or that property's animation in the layer.
    fn weight(&self, layer: &Layer, time: f64) -> f64 {
        if layer.blend == Blend::Override {
            return layer.weight;
        }
        match layer.driven.iter().find(|d| d.target == layer.id && d.property == "Weight") {
            Some(d) => {
                let w = (self.value(d.value, time)[0] / 100.0).max(0.0);
                if w > 0.99999 { 1.0 } else { w }
            }
            None => layer.weight,
        }
    }

    /// Calls `f(layer position in the take, layer, driven, weight)` for everything take `take`
    /// drives, layer by layer.
    fn each_driven(&self, take: usize, time: f64, mut f: impl FnMut(usize, &Layer, &Driven, f64)) {
        let Some(t) = self.takes.get(take) else { return };
        for (pos, layer) in t.layers.iter().filter_map(|l| self.layers.get(*l)).enumerate() {
            let w = self.weight(layer, time);
            for d in &layer.driven {
                f(pos, layer, d, w);
            }
        }
    }

    /// Property `property` of object `target` at `time` in take `take`, starting from `base`
    /// (the value the file stores). `order` is the object's rotation order, for layers that
    /// mix rotations. `None` when the take does not drive it.
    pub fn property(
        &self,
        take: usize,
        target: i64,
        property: &str,
        time: f64,
        base: [f64; 3],
        order: RotationOrder,
    ) -> Option<[f64; 3]> {
        let mut out = base;
        let mut any = false;
        self.each_driven(take, time, |pos, layer, d, w| {
            if d.target == target && d.property == property {
                combine(pos, layer, w, property, order, &mut out, self.value(d.value, time));
                any = true;
            }
        });
        any.then_some(out)
    }

    /// The transform properties of every model of `hierarchy` at `time` (seconds on the file's
    /// clock) in take `take`; models the take does not drive keep the file's values.
    pub fn transform_props(&self, hierarchy: &Hierarchy, take: usize, time: f64) -> Vec<TransformProps> {
        let mut props: Vec<TransformProps> = hierarchy.nodes.iter().map(|n| n.props.clone()).collect();
        self.each_driven(take, time, |pos, layer, d, w| {
            let Some(p) = hierarchy.index_of(d.target).and_then(|i| props.get_mut(i)) else { return };
            let order = p.rotation_order;
            let field = match d.property.as_str() {
                "Lcl Translation" => &mut p.translation,
                "Lcl Rotation" => &mut p.rotation,
                "Lcl Scaling" => &mut p.scaling,
                "PreRotation" => &mut p.pre_rotation,
                "PostRotation" => &mut p.post_rotation,
                "RotationOffset" => &mut p.rotation_offset,
                "RotationPivot" => &mut p.rotation_pivot,
                "ScalingOffset" => &mut p.scaling_offset,
                "ScalingPivot" => &mut p.scaling_pivot,
                "GeometricTranslation" => &mut p.geometric_translation,
                "GeometricRotation" => &mut p.geometric_rotation,
                "GeometricScaling" => &mut p.geometric_scaling,
                _ => return,
            };
            combine(pos, layer, w, &d.property, order, field, self.value(d.value, time));
        });
        props
    }

    /// The world transforms of every model of `hierarchy` at `time` in take `take`.
    pub fn worlds(&self, hierarchy: &Hierarchy, take: usize, time: f64) -> Vec<NodeWorld> {
        hierarchy.evaluate(&self.transform_props(hierarchy, take, time))
    }

    /// Whether take `take` changes a transform property of a model of `hierarchy` over time.
    pub fn moves(&self, hierarchy: &Hierarchy, take: usize) -> bool {
        self.moves_any(hierarchy, take, |_| true)
    }

    /// Whether take `take` changes, over time, a transform property of a model of `hierarchy`
    /// for which `wanted(index)` holds. Curves that hold one value do not count; a layer
    /// whose weight changes counts for all it drives.
    pub fn moves_any(&self, hierarchy: &Hierarchy, take: usize, wanted: impl Fn(usize) -> bool) -> bool {
        let varies = |value: usize| {
            self.curve_nodes.get(value).is_some_and(|n| {
                n.curves.iter().flatten().any(|c| self.curves.get(*c).is_some_and(|c| !c.is_constant()))
            })
        };
        let mut any = false;
        self.each_driven(take, 0.0, |_, layer, d, _| {
            if !any && is_transform(&d.property) && hierarchy.index_of(d.target).is_some_and(&wanted) {
                let weight = layer
                    .driven
                    .iter()
                    .any(|w| w.target == layer.id && w.property == "Weight" && varies(w.value));
                any = varies(d.value) || weight;
            }
        });
        any
    }
}

/// Whether `property` is one of a model's transform properties.
pub fn is_transform(property: &str) -> bool {
    matches!(
        property,
        "Lcl Translation"
            | "Lcl Rotation"
            | "Lcl Scaling"
            | "PreRotation"
            | "PostRotation"
            | "RotationOffset"
            | "RotationPivot"
            | "ScalingOffset"
            | "ScalingPivot"
            | "GeometricTranslation"
            | "GeometricRotation"
            | "GeometricScaling"
    )
}

/// Puts layer `layer`'s `value` (at position `pos` in its take, weight `w`) onto `out`.
fn combine(
    pos: usize,
    layer: &Layer,
    w: f64,
    property: &str,
    order: RotationOrder,
    out: &mut [f64; 3],
    value: [f64; 3],
) {
    if pos == 0 || layer.blend == Blend::Override {
        *out = value;
        return;
    }
    let scale = layer.compose_scale && property == "Lcl Scaling";
    let rotation = layer.compose_rotation && property == "Lcl Rotation";
    match layer.blend {
        Blend::Additive if scale => {
            for (o, v) in out.iter_mut().zip(value) {
                *o *= pow_abs(v, w);
            }
        }
        Blend::Additive if rotation => {
            let b = slerp(IDENTITY_QUAT, quat_of(&euler(value, order)), w);
            let r = quat_mul(quat_of(&euler(*out, order)), b);
            *out = euler_of(&matrix_of(r), order);
        }
        Blend::Additive => {
            for (o, v) in out.iter_mut().zip(value) {
                *o += v * w;
            }
        }
        _ if scale => {
            for (o, v) in out.iter_mut().zip(value) {
                *o = pow_abs(*o, 1.0 - w) * pow_abs(v, w);
            }
        }
        _ if rotation => {
            let r = slerp(quat_of(&euler(*out, order)), quat_of(&euler(value, order)), w);
            *out = euler_of(&matrix_of(r), order);
        }
        _ => {
            for (o, v) in out.iter_mut().zip(value) {
                *o = *o * (1.0 - w) + v * w;
            }
        }
    }
}

/// `|v|^e` with `v`'s sign; `e` 0 gives 1 and `e` 1 gives `v`.
fn pow_abs(v: f64, e: f64) -> f64 {
    if e <= 0.0 {
        1.0
    } else if e >= 1.0 {
        v
    } else {
        v.signum() * v.abs().powf(e)
    }
}

/// A unit quaternion `[x, y, z, w]`.
type Quat = [f64; 4];
const IDENTITY_QUAT: Quat = [0.0, 0.0, 0.0, 1.0];

fn quat_mul(a: Quat, b: Quat) -> Quat {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn slerp(a: Quat, mut b: Quat, t: f64) -> Quat {
    let mut dot: f64 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    if dot < 0.0 {
        dot = -dot;
        b = b.map(|x| -x);
    }
    let omega = dot.clamp(0.0, 1.0).acos();
    if omega <= 1.175_494_351e-38 {
        return a;
    }
    let rcp = 1.0 / omega.sin();
    let (af, bf) = (((1.0 - t) * omega).sin() * rcp, (t * omega).sin() * rcp);
    let q =
        [0, 1, 2, 3].map(|k| af * a.get(k).copied().unwrap_or(0.0) + bf * b.get(k).copied().unwrap_or(0.0));
    let len = q.iter().map(|x| x * x).sum::<f64>().sqrt();
    if len > 0.0 && len.is_finite() { q.map(|x| x / len) } else { a }
}

/// The quaternion of a rotation matrix.
fn quat_of(m: &Mat3) -> Quat {
    let [[m00, m01, m02], [m10, m11, m12], [m20, m21, m22]] = *m;
    let trace = m00 + m11 + m22;
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s]
    };
    let len = q.iter().map(|x| x * x).sum::<f64>().sqrt();
    if len > 0.0 && len.is_finite() { q.map(|x| x / len) } else { IDENTITY_QUAT }
}

fn matrix_of(q: Quat) -> Mat3 {
    let [x, y, z, w] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// Euler angles in degrees, in `order`, of a rotation matrix: `euler(euler_of(m, o), o) == m`.
pub fn euler_of(m: &Mat3, order: RotationOrder) -> [f64; 3] {
    let [i, j, k] = order.axes();
    // An even permutation of x, y, z turns one way, an odd one the other.
    let s = if (j + 3 - i) % 3 == 1 { 1.0 } else { -1.0 };
    let at = |r: usize, c: usize| m.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0.0);
    let sin_j = (-s * at(k, i)).clamp(-1.0, 1.0);
    let cos_j = (at(i, i) * at(i, i) + at(j, i) * at(j, i)).sqrt();
    let mut out = [0.0; 3];
    let (ai, aj, ak);
    if cos_j > 1e-9 {
        ai = (s * at(k, j)).atan2(at(k, k));
        aj = sin_j.atan2(cos_j);
        ak = (s * at(j, i)).atan2(at(i, i));
    } else {
        // Gimbal lock: the first and the last turn share an axis; all of it goes to the first.
        ai = (-s * at(j, k)).atan2(at(j, j));
        aj = sin_j.atan2(cos_j);
        ak = 0.0;
    }
    for (axis, a) in [(i, ai), (j, aj), (k, ak)] {
        if let Some(o) = out.get_mut(axis) {
            *o = a.to_degrees();
        }
    }
    out
}
