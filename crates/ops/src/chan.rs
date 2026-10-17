//! `chan` nodes: named channels on the CPU.
//!
//! Generators (constant, LFO, noise) produce one channel with one sample per show frame, at the
//! show's rate; math and merge combine channels; the filter smooths them over time, the trail
//! keeps their recent history as samples. The data on a `chan` connection is a [`Chans`]. Any
//! number param of any node can be bound to a channel.

use std::collections::{BTreeMap, VecDeque};

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, SampleAxis, Tick, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

/// Registers the `chan` node types.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new("chan.constant", "Fixed numbers: one channel per row of name and value")
            .param(constant_value("value0"))
            .param(ParamSpec::text("name0", "chan0").doc(CONSTANT_NAME_DOC))
            .param(
                ParamSpec::int("rows", 1)
                    .clamp(1.0, CONSTANT_MAX_ROWS as f64)
                    .more()
                    .doc("How many rows; row k is the params `name<k>` and `value<k>`, counted from 0"),
            )
            .output("out", DataKind::Chan)
            .extra_params(),
        || Box::new(Constant { rows: CONSTANT_MAX_ROWS }) as Box<dyn Operator>,
    ))?;
    generator(
        types,
        NodeDesc::new("chan.lfo", "Low-frequency oscillator: offset + amplitude * wave(time * rate + phase)")
            .param(
                ParamSpec::float("rate", 1.0).range(0.01, 100.0).curve(Curve::Log).unit(Unit::Hertz).doc(
                    "Full waves per second: 1 Hz is one up and down each second, 0.1 one every ten seconds",
                ),
            )
            .param(
                ParamSpec::choice("shape", &["sine", "triangle", "square", "saw"])
                    .doc("The form of one wave, from -1 to 1 before amplitude and offset")
                    .choice_doc("sine", "smoothly up and down, slow at the turns")
                    .choice_doc("triangle", "straight up and straight down at an even speed")
                    .choice_doc(
                        "square",
                        "1 for the first half of each wave, -1 for the second, jumping between",
                    )
                    .choice_doc("saw", "rising evenly from -1 to 1, then jumping back"),
            )
            .param(
                ParamSpec::float("amplitude", 1.0)
                    .range(0.0, 2.0)
                    .doc("Half the swing: the value goes from offset - amplitude to offset + amplitude"),
            )
            .param(
                ParamSpec::float("offset", 0.0)
                    .range(-1.0, 1.0)
                    .doc("The centre the wave swings around; 0.5 with amplitude 0.5 gives 0 to 1"),
            )
            .param(ParamSpec::float("phase", 0.0).range(0.0, 1.0).more().doc(
                "Shift along the wave, in waves: 0.25 is a quarter wave ahead. Sets lfos of one rate apart",
            ))
            .time_dependent(),
        |cx| {
            let cycles = cx.time().seconds() * cx.float("rate")? + cx.float("phase")?;
            let x = cycles.rem_euclid(1.0);
            let wave = match cx.text("shape")? {
                "triangle" => 1.0 - 4.0 * (x - 0.5).abs(),
                "square" => {
                    if x < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                "saw" => 2.0 * x - 1.0,
                _ => (x * std::f64::consts::TAU).sin(),
            };
            Ok(cx.float("offset")? + cx.float("amplitude")? * wave)
        },
    )?;
    generator(
        types,
        NodeDesc::new(
            "chan.noise",
            "Smooth random motion over time: offset + amplitude * noise, the same on every machine",
        )
        .param(
            ParamSpec::float("frequency", 1.0)
                .range(0.01, 50.0)
                .curve(Curve::Log)
                .unit(Unit::Hertz)
                .doc("How fast it wanders: about this many rises and falls per second"),
        )
        .param(
            ParamSpec::float("amplitude", 1.0)
                .range(0.0, 2.0)
                .doc("Half the swing: the value stays within about offset - amplitude to offset + amplitude"),
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .range(-1.0, 1.0)
                .doc("The centre the noise wanders around"),
        )
        .param(
            ParamSpec::int("seed", 0)
                .range(0.0, 100.0)
                .doc("Picks another motion of the same character; one seed always gives the same motion"),
        )
        .param(
            ParamSpec::int("octaves", 2)
                .clamp(1.0, 8.0)
                .more()
                .doc("Layers of ever faster wiggle added up, each twice the frequency of the one before; 1 is smoothest"),
        )
        .param(
            ParamSpec::float("roughness", 0.5)
                .range(0.0, 1.0)
                .more()
                .doc("Strength of each octave relative to the one before; higher is more jittery"),
        )
        .time_dependent(),
        |cx| {
            let t = cx.time().seconds() * cx.float("frequency")?;
            let octaves = u32::try_from(cx.int("octaves")?.clamp(1, 8)).unwrap_or(1);
            let n = fbm(t, cx.int("seed")?, octaves, cx.float("roughness")?);
            Ok(cx.float("offset")? + cx.float("amplitude")? * n)
        },
    )?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.math",
            "a op b per channel; b's channel of the same name, else its first; `b` while input b is \
             unconnected",
        )
        .param(
            ParamSpec::choice(
                "op",
                &["add", "subtract", "multiply", "divide", "min", "max", "power", "modulo", "floor"],
            )
            .doc("What is done with a and b, sample by sample")
            .choice_doc("add", "a + b")
            .choice_doc("subtract", "a - b")
            .choice_doc("multiply", "a times b: b scales a")
            .choice_doc("divide", "a divided by b")
            .choice_doc("min", "the smaller of a and b: b is a ceiling")
            .choice_doc("max", "the larger of a and b: b is a floor")
            .choice_doc("power", "a to the power b")
            .choice_doc("modulo", "a wrapped into 0 to b (b = 0 leaves a)")
            .choice_doc("floor", "a down to a multiple of b (b = 1: whole numbers; b = 0 leaves a)"),
        )
        .param(
            ParamSpec::float("b", 0.0)
                .range(-1.0, 1.0)
                .doc("The b side as a number, used while input b is unconnected"),
        )
        .input("a", DataKind::Chan)
        .input("b", DataKind::Chan)
        .output("out", DataKind::Chan),
        || Box::new(Math) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.filter", "Smooths each channel over time, rising and falling at their own pace")
            .stateful()
            .param(
                ParamSpec::float("rise", 0.2)
                    .clamp(0.0, 3600.0)
                    .range(0.0, 5.0)
                    .curve(Curve::Power(2.0))
                    .unit(Unit::Seconds)
                    .doc("Time to cover 63 % of a step up; 0 follows at once"),
            )
            .param(
                ParamSpec::float("fall", 0.2)
                    .clamp(0.0, 3600.0)
                    .range(0.0, 5.0)
                    .curve(Curve::Power(2.0))
                    .unit(Unit::Seconds)
                    .doc("Time to cover 63 % of a step down"),
            )
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .time_dependent(),
        || Box::new(Filter::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.speed",
            "Adds up each channel over time, as a rate per second: a speed becomes a position",
        )
        .stateful()
        .param(
            ParamSpec::choice("limit", &["none", "loop", "clamp"])
                .doc("Whether the sum is kept within min and max")
                .choice_doc("none", "it grows without end")
                .choice_doc(
                    "loop",
                    "past max it starts again from min (and the other way round): an angle, a scroll",
                )
                .choice_doc("clamp", "it stops at min and max"),
        )
        .param(
            ParamSpec::float("min", 0.0)
                .range(-10.0, 10.0)
                .doc("Lower end of the range the sum stays in (limit loop or clamp)"),
        )
        .param(
            ParamSpec::float("max", 1.0)
                .range(-10.0, 10.0)
                .doc("Upper end of the range the sum stays in (limit loop or clamp)"),
        )
        .param(ParamSpec::bool("reset", false).doc("Hold the sum at 0 (or min) while on"))
        .input("in", DataKind::Chan)
        .output("out", DataKind::Chan)
        .time_dependent(),
        || Box::new(Speed::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.trail",
            "The recent history of each input channel as samples, oldest first: a time graph to look \
             at or to feed on",
        )
        .stateful()
        .param(
            ParamSpec::float("length", 5.0)
                .clamp(TRAIL_MIN_LENGTH, TRAIL_MAX_LENGTH)
                .range(0.1, 60.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("How far back the trail reaches; older values drop off its start"),
        )
        .param(
            ParamSpec::float("rate", 60.0)
                .clamp(TRAIL_MIN_RATE, TRAIL_MAX_RATE)
                .range(1.0, 240.0)
                .curve(Curve::Log)
                .unit(Unit::Hertz)
                .doc(
                    "Samples taken per second of show time, whatever the frame rate; length times \
                     rate is the number of samples",
                ),
        )
        .param(ParamSpec::bool("active", true).doc(
            "Records while on; off holds the trail as it is, and recording resumes from the moment \
             it is on again",
        ))
        .input("in", DataKind::Chan)
        .output("out", DataKind::Chan)
        .describe_input("in", "The channels to record; of each, its value now (its last sample) is taken")
        .describe_output(
            "out",
            "The input's channels by name, each with length times rate samples, the newest last; \
             before the trail is full, the first value fills its start",
        )
        .time_dependent(),
        || Box::new(Trail::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.merge", "The channels of all inputs side by side")
            .input("a", DataKind::Chan)
            .input("b", DataKind::Chan)
            .input("c", DataKind::Chan)
            .input("d", DataKind::Chan)
            .output("out", DataKind::Chan),
        || Box::new(Merge) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.select", "The input's channels matching patterns, in the patterns' order")
            .param(ParamSpec::text("channels", "*").doc(
                "Space-separated names, `*` matches any run of characters: `x y pos*`; `^name` leaves \
             channels out. Renaming is chan.rename's job",
            ))
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan),
        || Box::new(Select) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.rename", "The input's channels, renamed by `from=to` pairs")
            .param(ParamSpec::text("names", "").doc(
                "Space-separated `from=to` pairs, the first match wins: `tx=x ty=y`. With one `*` \
                 on each side the matched part carries over: `pos*=p*` makes pos_a into p_a",
            ))
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan),
        || Box::new(Rename) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.null", "The input, unchanged: a fixed point to refer to or to bind to")
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan),
        || Box::new(Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The kind of the constant, whose rows the param panel edits.
pub const CONSTANT: &str = "chan.constant";
/// Most rows of a `chan.constant`.
pub const CONSTANT_MAX_ROWS: usize = 64;

const CONSTANT_NAME_DOC: &str = "Name of the channel this row makes, as other nodes and bindings see it";

/// The value of a `chan.constant` row.
fn constant_value(name: &str) -> ParamSpec {
    ParamSpec::float(name, 0.0)
        .range(-1.0, 1.0)
        .doc("The channel's value; the slider spans -1 to 1, any number can be typed")
}

/// The params of row `k` (from 0) of a `chan.constant`: `name0` and `value0`, `name1` and
/// `value1`, ... Row 0's are the type's own; the others are declared by the node
/// ([`NodeDesc::extra_params`]).
pub fn constant_row(k: usize) -> (String, String) {
    (format!("name{k}"), format!("value{k}"))
}

/// The specs of row `k` of a `chan.constant`: the channel named `chan<k>` by default.
pub fn constant_row_specs(k: usize) -> [ParamSpec; 2] {
    let (name, value) = constant_row(k);
    [ParamSpec::text(&name, &format!("chan{k}")).doc(CONSTANT_NAME_DOC), constant_value(&value)]
}

/// `chan.constant`: one channel per row. `rows` is the count the node last cooked with (the
/// most rows before its first cook), so bindings of every row's value resolve.
struct Constant {
    rows: usize,
}

impl Operator for Constant {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let rows = usize::try_from(cx.int("rows")?.clamp(1, CONSTANT_MAX_ROWS as i64)).unwrap_or(1);
        let mut channels = Vec::with_capacity(rows);
        for k in 0..rows {
            let (name_param, value_param) = constant_row(k);
            let name = match cx.param(&name_param).and_then(Value::as_str) {
                Some(n) => n.to_string(),
                None => format!("chan{k}"),
            };
            let value = match cx.param(&value_param) {
                None => 0.0,
                Some(v) => v
                    .as_f64()
                    .ok_or_else(|| CookError::failed(format!("`{value_param}` must be a number, got {v}")))?,
            };
            let name = Name::new(name.trim()).map_err(|e| e.context(format!("`{name_param}`")))?;
            channels.push(Channel { name, samples: vec![value] });
        }
        // Values of rows past `rows` are kept (a row taken away by lowering `rows` comes back
        // as it was) but say so.
        let past: Vec<&str> = cx
            .def()
            .params
            .keys()
            .map(Name::as_str)
            .filter(|n| row_of(n).is_some_and(|k| k >= rows))
            .collect();
        if !past.is_empty() {
            cx.warn(format!("{} belong to rows past `rows` ({rows}) and are not used", past.join(", ")));
        }
        let unknown: Vec<&str> =
            cx.def().params.keys().map(Name::as_str).filter(|n| !is_constant_param(n)).collect();
        if !unknown.is_empty() {
            cx.warn(format!("`chan.constant` has no param {}", unknown.join(", ")));
        }
        // Bindings of rows the node did not know of before this cook resolve on the next one.
        let known = self.rows;
        self.rows = rows;
        if rows > known && cx.def().binds.keys().any(|b| row_of(b.param.as_str()).is_some_and(|k| k >= known))
        {
            cx.waker().wake();
        }
        out(Chans::new(cx.rate().as_f64(), channels)?)
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        Some((1..self.rows).flat_map(constant_row_specs).collect())
    }
}

/// The row `name<k>` or `value<k>` belongs to.
fn row_of(param: &str) -> Option<usize> {
    let digits = param.strip_prefix("name").or_else(|| param.strip_prefix("value"))?;
    let k: usize = digits.parse().ok()?;
    let canonical = digits == "0" || !digits.starts_with('0');
    (canonical && digits.len() <= 3).then_some(k)
}

fn is_constant_param(name: &str) -> bool {
    name == "rows" || row_of(name).is_some_and(|k| k < CONSTANT_MAX_ROWS)
}

/// Passes its input on as it is.
struct Null;

impl Operator for Null {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        match cx.input(0) {
            Some(data) => Ok(Cooked::Outputs(vec![data.clone()])),
            None => Err(waiting(cx)),
        }
    }
}

/// Pending while a wired input has nothing yet, else asks for an input.
pub(crate) fn waiting(cx: &CookContext<'_>) -> CookError {
    if cx.def().inputs.iter().any(Option::is_some) {
        CookError::Pending
    } else {
        CookError::failed("connect an input")
    }
}

/// Parses `chan.select`'s `channels`: `(keep, leave out, refused renames)`.
fn picks(text: &str) -> (Vec<&str>, Vec<&str>, Vec<&str>) {
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    let mut renames = Vec::new();
    for token in text.split_whitespace() {
        if token.contains('=') {
            renames.push(token);
        } else if let Some(p) = token.strip_prefix('^') {
            drop.push(p);
        } else {
            keep.push(token);
        }
    }
    (keep, drop, renames)
}

/// Whether `name` matches `pattern` (`*` any run of characters, the rest literal), by the
/// usual linear wildcard walk.
pub(crate) fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while let Some(nc) = n.get(j) {
        match p.get(i) {
            Some('*') => {
                star = Some((i, j));
                i += 1;
            }
            Some(c) if c == nc => {
                i += 1;
                j += 1;
            }
            _ => match star {
                Some((si, sj)) => {
                    i = si + 1;
                    j = sj + 1;
                    star = Some((si, sj + 1));
                }
                None => return false,
            },
        }
    }
    p.iter().skip(i).all(|c| *c == '*')
}

/// The new name of `name` matched by `pattern`: `rename` with its `*` replaced by what the
/// pattern's one `*` matched, or `rename` as it is.
fn renamed(pattern: &str, rename: &str, name: &str) -> String {
    if let (Some((pre, post)), true) = (pattern.split_once('*'), rename.contains('*'))
        && !post.contains('*')
        && name.len() >= pre.len() + post.len()
    {
        let middle = name.get(pre.len()..name.len() - post.len()).unwrap_or("");
        return rename.replacen('*', middle, 1);
    }
    rename.to_string()
}

struct Select;

impl Operator for Select {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let input = cx.input_as::<Chans>(0).ok_or_else(|| waiting(cx))?;
        let (keep, drop, renames) = picks(cx.text("channels")?);
        let mut taken = vec![false; input.channels().len()];
        let mut channels: Vec<Channel> = Vec::new();
        let mut unmatched = Vec::new();
        for pattern in &keep {
            let mut any = false;
            for (i, ch) in input.channels().iter().enumerate() {
                let name = ch.name.as_str();
                let Some(seen) = taken.get_mut(i) else { continue };
                if *seen || !glob(pattern, name) || drop.iter().any(|d| glob(d, name)) {
                    continue;
                }
                *seen = true;
                any = true;
                channels.push(ch.clone());
            }
            if !any {
                unmatched.push(*pattern);
            }
        }
        let mut warnings = Vec::new();
        if !renames.is_empty() {
            warnings.push(format!(
                "{} ignored: chan.select only selects; rename with a chan.rename after it",
                renames.join(", ")
            ));
        }
        if !unmatched.is_empty() {
            warnings.push(format!("nothing matches {}", unmatched.join(", ")));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        out(Chans::new(input.rate(), channels)?.with_axis(input.axis()))
    }
}

/// `chan.rename`: every channel, in the input's order, renamed by the first `from=to` pair
/// whose `from` matches it; the others keep their names.
struct Rename;

impl Operator for Rename {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let input = cx.input_as::<Chans>(0).ok_or_else(|| waiting(cx))?;
        let text = cx.text("names")?;
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        let mut bad = Vec::new();
        for token in text.split_whitespace() {
            match token.split_once('=') {
                Some((from, to)) if !from.is_empty() && !to.is_empty() => pairs.push((from, to)),
                _ => bad.push(token),
            }
        }
        let mut used = vec![false; pairs.len()];
        let mut channels: Vec<Channel> = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let name = ch.name.as_str();
            let hit = pairs.iter().position(|(from, _)| glob(from, name));
            let new = match hit.and_then(|i| Some((i, pairs.get(i)?))) {
                Some((i, (from, to))) => {
                    if let Some(u) = used.get_mut(i) {
                        *u = true;
                    }
                    Name::new(&renamed(from, to, name))
                        .map_err(|e| e.context(format!("renaming `{name}` by `{from}={to}`")))?
                }
                None => ch.name.clone(),
            };
            channels.push(Channel { name: new, samples: ch.samples.clone() });
        }
        let mut warnings = Vec::new();
        if !bad.is_empty() {
            warnings.push(format!("{} ignored: write `from=to`", bad.join(", ")));
        }
        let idle: Vec<&str> =
            pairs.iter().zip(&used).filter(|(_, u)| !**u).map(|((from, _), _)| *from).collect();
        if !idle.is_empty() {
            warnings.push(format!("nothing matches {}", idle.join(", ")));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        out(Chans::new(input.rate(), channels)?.with_axis(input.axis()))
    }
}

/// The channel name every generator declares last.
fn name_param() -> ParamSpec {
    ParamSpec::text("name", "value").doc("Name of the channel it makes, as bindings and other nodes see it")
}

fn out(chans: Chans) -> Result<Cooked, CookError> {
    Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
}

/// A stateless node computing one number per frame into channel `name`.
struct Generator<F>(F);

impl<F: Fn(&CookContext<'_>) -> Result<f64, CookError> + Send> Operator for Generator<F> {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let v = (self.0)(cx)?;
        let name = Name::new(cx.text("name")?).map_err(|e| e.context("`name`"))?;
        out(Chans::single(name, v, cx.rate().as_f64())?)
    }
}

fn generator<F>(types: &mut NodeTypes, desc: NodeDesc, f: F) -> Result<(), Error>
where
    F: Fn(&CookContext<'_>) -> Result<f64, CookError> + Clone + Send + Sync + 'static,
{
    let mut desc = desc.output("out", DataKind::Chan);
    desc.params.push(name_param());
    types.register(FnNodeType::new(desc, move || Box::new(Generator(f.clone())) as Box<dyn Operator>))
}

/// 1D gradient noise in about -1..1, integer hashes only (deterministic everywhere).
fn noise(t: f64, seed: i64) -> f64 {
    let i = t.floor();
    let f = t - i;
    // Saturating `as`; time stays far below where this matters.
    let i = i as i64;
    let grad = |k: i64| {
        let h = hash(k, seed);
        // -1..1 from the top 24 bits.
        f64::from(h >> 8) / f64::from(1u32 << 23) - 1.0
    };
    let a = grad(i) * f;
    let b = grad(i.wrapping_add(1)) * (f - 1.0);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    // A 1D gradient noise peaks at 0.5; scaled to reach about 1.
    2.0 * (a + (b - a) * s)
}

/// Fractal sum of `octaves` noises, normalised to about -1..1.
fn fbm(t: f64, seed: i64, octaves: u32, roughness: f64) -> f64 {
    let (mut sum, mut norm, mut amp, mut freq) = (0.0, 0.0, 1.0, 1.0);
    for o in 0..octaves {
        sum += amp * noise(t * freq, seed.wrapping_add(i64::from(o) * 1013));
        norm += amp;
        amp *= roughness;
        freq *= 2.0;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// PCG-style integer hash of a lattice point and a seed.
fn hash(i: i64, seed: i64) -> u32 {
    // Truncating casts on purpose: only the bits matter.
    let mut x = (i as u64 ^ (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) as u32
        ^ ((i as u64 >> 32) as u32).wrapping_mul(0x85EB_CA6B);
    x = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((x >> ((x >> 28).wrapping_add(4))) ^ x).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

struct Math;

impl Operator for Math {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let a = cx.input_as::<Chans>(0);
        let b = cx.input_as::<Chans>(1);
        let b_param = cx.float("b")?;
        let op: fn(f64, f64) -> f64 = match cx.text("op")? {
            "subtract" => |a, b| a - b,
            "multiply" => |a, b| a * b,
            "divide" => |a, b| a / b,
            "min" => f64::min,
            "max" => f64::max,
            "power" => f64::powf,
            "modulo" => |a, b| if b == 0.0 { a } else { a.rem_euclid(b) },
            "floor" => |a, b| if b == 0.0 { a } else { (a / b).floor() * b },
            _ => |a, b| a + b,
        };
        // The layout (names, length, rate) comes from a, else from b.
        let (layout, rate, axis) = match (a, b) {
            (Some(c), _) | (None, Some(c)) => (
                c.channels().iter().map(|ch| (ch.name.clone(), ch.samples.len())).collect(),
                c.rate(),
                c.axis(),
            ),
            (None, None) => (vec![(Name::new("value")?, 1)], cx.rate().as_f64(), SampleAxis::Time),
        };
        let channels = layout
            .into_iter()
            .map(|(name, len)| {
                let a_ch = a.and_then(|a| a.get(name.as_str()));
                let b_ch = b.and_then(|b| b.get(name.as_str()).or_else(|| b.channels().first()));
                let samples = (0..len)
                    .map(|i| {
                        let x = a_ch.map_or(0.0, |c| sample(c, i));
                        let y = match (b, b_ch) {
                            (None, _) => b_param,
                            (Some(_), Some(c)) => sample(c, i),
                            (Some(_), None) => 0.0,
                        };
                        op(x, y)
                    })
                    .collect();
                Channel { name, samples }
            })
            .collect();
        out(Chans::new(rate, channels)?.with_axis(axis))
    }
}

/// Sample `i`, or the last one when the channel is shorter (a single value applies to all).
fn sample(c: &Channel, i: usize) -> f64 {
    c.samples.get(i).or(c.samples.last()).copied().unwrap_or(0.0)
}

/// One-pole smoothing with separate rise and fall times. Per-frame channels advance by the time
/// since the last cook, so skipped frames and an idle editor (nothing asked for the node) still
/// give the right curve; channels with many samples advance by their sample period.
#[derive(Default)]
struct Filter {
    state: BTreeMap<Name, f64>,
    last: Option<Tick>,
}

impl Operator for Filter {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let Some(input) = cx.input_as::<Chans>(0) else {
            self.state.clear();
            self.last = None;
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        let (rise, fall) = (cx.float("rise")?, cx.float("fall")?);
        // Going back in time (a seek) starts over, like a new node.
        let dt = match self.last {
            Some(last) if now.0 >= last.0 => Tick(now.0 - last.0).seconds(),
            _ => {
                self.state.clear();
                0.0
            }
        };
        self.last = Some(now);
        let per_sample = input.len() > 1;
        let mut state = BTreeMap::new();
        let mut channels = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut y = self.state.get(&ch.name).copied();
            let mut samples = Vec::with_capacity(ch.samples.len());
            for &x in &ch.samples {
                let step = if per_sample { 1.0 / input.rate() } else { dt };
                let v = match y {
                    None => x,
                    Some(prev) => {
                        let tau = if x > prev { rise } else { fall };
                        if tau <= 0.0 { x } else { prev + (x - prev) * (1.0 - (-step / tau).exp()) }
                    }
                };
                y = Some(v);
                samples.push(v);
            }
            if let Some(v) = y {
                state.insert(ch.name.clone(), v);
            }
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        self.state = state;
        out(Chans::new(input.rate(), channels)?.with_axis(input.axis()))
    }
}

/// Integrates each channel: per-frame channels by the time since the last cook (like
/// [`Filter`]), channels with many samples by their sample period.
#[derive(Default)]
struct Speed {
    state: BTreeMap<Name, f64>,
    last: Option<Tick>,
}

/// `v` kept within `lo..hi` by `limit`.
fn limited(v: f64, limit: &str, lo: f64, hi: f64) -> f64 {
    match limit {
        "loop" if hi > lo => lo + (v - lo).rem_euclid(hi - lo),
        "loop" | "clamp" => v.clamp(lo.min(hi), hi.max(lo)),
        _ => v,
    }
}

impl Operator for Speed {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let Some(input) = cx.input_as::<Chans>(0) else {
            self.state.clear();
            self.last = None;
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        let limit = cx.text("limit")?;
        let (lo, hi) = (cx.float("min")?, cx.float("max")?);
        let start = limited(0.0, limit, lo, hi);
        // Going back in time (a seek) or a reset starts over.
        let dt = match self.last {
            Some(last) if now.0 >= last.0 => Tick(now.0 - last.0).seconds(),
            _ => {
                self.state.clear();
                0.0
            }
        };
        self.last = Some(now);
        let reset = cx.bool("reset")?;
        if reset {
            self.state.clear();
        }
        let per_sample = input.len() > 1;
        let mut state = BTreeMap::new();
        let mut channels = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut sum = self.state.get(&ch.name).copied().unwrap_or(start);
            let mut samples = Vec::with_capacity(ch.samples.len());
            for &x in &ch.samples {
                let step = if per_sample { 1.0 / input.rate() } else { dt };
                if !reset {
                    sum = limited(sum + x * step, limit, lo, hi);
                }
                samples.push(sum);
            }
            state.insert(ch.name.clone(), sum);
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        self.state = state;
        out(Chans::new(input.rate(), channels)?.with_axis(input.axis()))
    }
}

/// Shortest and longest `length` of a `chan.trail`, in seconds.
const TRAIL_MIN_LENGTH: f64 = 0.01;
const TRAIL_MAX_LENGTH: f64 = 3600.0;
/// Lowest and highest `rate` of a `chan.trail`, in samples per second.
const TRAIL_MIN_RATE: f64 = 0.1;
const TRAIL_MAX_RATE: f64 = 1000.0;
/// Most samples a `chan.trail` keeps per channel (half an hour at 60 Hz is 108 000); together
/// they also stay within [`penumbra_core::chan::MAX_SAMPLES`].
const TRAIL_MAX_SAMPLES: usize = 1 << 17;

/// `chan.trail`: per channel a ring of samples on a grid of show time. A cook takes as many
/// samples as grid points have passed since the last one, all holding the input's value now, so
/// a late frame or a node that was not asked for a while leaves a step rather than squeezing
/// time; the show's frame rate and the trail's rate need not match.
#[derive(Default)]
struct Trail {
    /// In the input's order.
    channels: Vec<(Name, VecDeque<f64>)>,
    /// The time of the next sample on the grid; `None` before the first sample and while not
    /// recording.
    next: Option<Tick>,
    last: Option<Tick>,
}

/// The samples of a `chan.trail` per channel for `length` and `rate`, and whether the caps cut
/// them, with `channels` channels.
fn trail_len(length: f64, rate: f64, channels: usize) -> (usize, bool) {
    let wanted = (length * rate).round();
    let cap = (penumbra_core::chan::MAX_SAMPLES / channels.max(1)).clamp(1, TRAIL_MAX_SAMPLES);
    // `as` saturates and maps NaN to 0; the clamp below takes care of both ends.
    let wanted = if wanted.is_finite() { wanted.max(1.0) as usize } else { 1 };
    (wanted.min(cap), wanted > cap)
}

impl Operator for Trail {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let rate = cx.float("rate")?.clamp(TRAIL_MIN_RATE, TRAIL_MAX_RATE);
        let length = cx.float("length")?.clamp(TRAIL_MIN_LENGTH, TRAIL_MAX_LENGTH);
        let active = cx.bool("active")?;
        let Some(input) = cx.input_as::<Chans>(0) else {
            *self = Self::default();
            return out(Chans::empty(rate)?);
        };
        // Going back in time (a seek) starts over, like a new node.
        if self.last.is_some_and(|last| now < last) {
            self.channels.clear();
            self.next = None;
        }
        self.last = Some(now);
        let (len, capped) = trail_len(length, rate, input.channels().len());
        if capped {
            cx.warn(format!(
                "{len} samples per channel at most ({} channels); shorten `length` or lower `rate`",
                input.channels().len()
            ));
        }
        // The channels as the input has them now: known ones keep their history, new ones
        // start flat at their value, gone ones are dropped.
        let mut known = std::mem::take(&mut self.channels);
        self.channels = input
            .channels()
            .iter()
            .map(|ch| {
                let history = match known.iter().position(|(n, _)| *n == ch.name) {
                    Some(i) => known.swap_remove(i).1,
                    None => VecDeque::from(vec![ch.value(); len]),
                };
                (ch.name.clone(), history)
            })
            .collect();
        for (_, h) in &mut self.channels {
            while h.len() > len {
                h.pop_front();
            }
            let oldest = h.front().copied().unwrap_or(0.0);
            while h.len() < len {
                h.push_front(oldest);
            }
        }
        let period = Tick::from_seconds(1.0 / rate).0.max(1);
        let due = match (active, self.next) {
            (false, _) => {
                self.next = None;
                0
            }
            (true, None) => {
                self.next = Some(Tick(now.0.saturating_add(period)));
                1
            }
            (true, Some(next)) if now >= next => {
                let k = (now.0 - next.0) / period + 1;
                self.next = Some(Tick(next.0.saturating_add(k.saturating_mul(period))));
                k
            }
            (true, Some(_)) => 0,
        };
        let due = usize::try_from(due).unwrap_or(0).min(len);
        if due > 0 {
            for ((_, h), ch) in self.channels.iter_mut().zip(input.channels()) {
                let v = ch.value();
                for _ in 0..due {
                    h.pop_front();
                    h.push_back(v);
                }
            }
        }
        let channels = self
            .channels
            .iter()
            .map(|(name, h)| Channel { name: name.clone(), samples: h.iter().copied().collect() })
            .collect();
        out(Chans::new(rate, channels)?)
    }
}

struct Merge;

impl Operator for Merge {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let inputs: Vec<&Chans> = (0..4).filter_map(|i| cx.input_as::<Chans>(i)).collect();
        let Some(first) = inputs.first() else {
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        let same_len = inputs.iter().all(|c| c.len() == first.len());
        if !same_len {
            cx.warn("the inputs differ in length; only their last samples are merged");
        }
        let mut channels: Vec<Channel> = Vec::new();
        let mut dropped = Vec::new();
        for ch in inputs.iter().flat_map(|c| c.channels()) {
            if channels.iter().any(|c| c.name == ch.name) {
                dropped.push(ch.name.to_string());
                continue;
            }
            let samples = if same_len { ch.samples.clone() } else { vec![ch.value()] };
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        if !dropped.is_empty() {
            cx.warn(format!("names taken by an earlier input, left out: {}", dropped.join(", ")));
        }
        out(Chans::new(first.rate(), channels)?.with_axis(first.axis()))
    }
}

#[cfg(test)]
mod tests {
    use crate::builtin_types;
    use std::sync::Arc;

    use penumbra_core::{Chans, LatePolicy, NodePath, ParamPath, Rate, ShowClock, Value};
    use penumbra_graph::{Cooker, JobPool, Network, Op, Port};

    struct F {
        net: Arc<Network>,
        cooker: Cooker,
        clock: ShowClock,
    }

    impl F {
        fn new(ops: &[Op]) -> Self {
            let types = Arc::new(builtin_types().unwrap());
            let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
            let cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
            Self { net, cooker, clock: ShowClock::new(Rate::FPS_60, LatePolicy::Realtime) }
        }

        fn chans(&mut self, path: &str) -> Chans {
            let id = self.net.resolve(&path.parse().unwrap()).unwrap();
            self.cooker.cook(&self.net, self.clock.now(), &[id], &[]);
            if let Some(s) = self.cooker.status(id) {
                assert!(s.error.is_none(), "{path}: {:?}", s.error);
            }
            self.cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone()
        }

        fn value(&mut self, path: &str) -> f64 {
            self.chans(path).value(None).unwrap()
        }
    }

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    fn connect(from: &str, to: &str, input: u32) -> Op {
        Op::Connect { from: from.parse().unwrap(), output: 0, to: to.parse().unwrap(), input }
    }

    #[test]
    fn lfo_and_math() {
        let mut f = F::new(&[
            create("/lfo", "chan.lfo"),
            create("/c", "chan.constant"),
            create("/m", "chan.math"),
            set("/lfo.shape", "saw"),
            set("/lfo.rate", 0.5),
            set("/c.value0", 10.0),
            set("/m.op", "multiply"),
            connect("/lfo", "/m", 0),
            connect("/c", "/m", 1),
        ]);
        // 1 s at 0.5 Hz is half a cycle: the saw is at 0, times 10.
        f.clock.advance(60);
        assert!(f.value("/m").abs() < 1e-9);
        f.clock.advance(30);
        assert!((f.value("/m") - 5.0).abs() < 1e-9);
        let c = f.chans("/m");
        assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["value"]);
        assert_eq!(c.rate(), 60.0);
    }

    #[test]
    fn math_without_inputs_and_with_named_channels() {
        let mut f = F::new(&[
            create("/m", "chan.math"),
            set("/m.b", 3.0),
            set("/m.op", "subtract"),
            create("/x", "chan.constant"),
            create("/y", "chan.constant"),
            create("/both", "chan.merge"),
            create("/k", "chan.constant"),
            create("/m2", "chan.math"),
            set("/x.name0", "x"),
            set("/x.value0", 1.0),
            set("/y.name0", "y"),
            set("/y.value0", 2.0),
            set("/k.value0", 10.0),
            set("/m2.op", "multiply"),
            connect("/x", "/both", 0),
            connect("/y", "/both", 1),
            connect("/both", "/m2", 0),
            connect("/k", "/m2", 1),
        ]);
        assert_eq!(f.value("/m"), -3.0);
        // b has no channel x or y: its first channel multiplies both.
        let c = f.chans("/m2");
        assert_eq!((c.value(Some("x")), c.value(Some("y"))), (Some(10.0), Some(20.0)));
        // Division by zero gives 0, not infinity.
        let mut g = F::new(&[create("/m", "chan.math"), set("/m.op", "divide")]);
        assert_eq!(g.value("/m"), 0.0);
    }

    #[test]
    fn constant_rows_select_rename_and_null() {
        // Rows from 0; row k > 0 are params the node declares itself.
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 0.5),
            set("/c.rows", 3_i64),
            set("/c.name0", "x"),
            set("/c.value1", 2.0),
            set("/c.name2", "pos_a"),
            set("/c.value2", -1.0),
            create("/lfo", "chan.lfo"),
            create("/s", "chan.select"),
            set("/s.channels", "pos* ^chan1 x"),
            connect("/c", "/s", 0),
            create("/r", "chan.rename"),
            set("/r.names", "pos*=p* x=gain"),
            connect("/s", "/r", 0),
            create("/n", "chan.null"),
            connect("/r", "/n", 0),
        ]);
        let c = f.chans("/c");
        let names: Vec<&str> = c.names().map(|n| n.as_str()).collect();
        assert_eq!(names, ["x", "chan1", "pos_a"]);
        assert_eq!(c.value(Some("chan1")), Some(2.0));
        let s = f.chans("/s");
        assert_eq!(s.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["pos_a", "x"], "pattern order");
        let n = f.chans("/n");
        assert_eq!(n.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["p_a", "gain"], "renamed");
        assert_eq!(n.value(Some("p_a")), Some(-1.0));
        // Renaming in a select is refused with a pointer to chan.rename.
        let id = f.net.resolve(&"/s".parse().unwrap()).unwrap();
        f.net =
            Arc::new(f.net.apply(&builtin_types().unwrap(), &[set("/s.channels", "x=y")]).unwrap().network);
        assert!(f.chans("/s").is_empty());
        let w = f.cooker.status(id).unwrap().warning.unwrap();
        assert!(w.contains("chan.rename"), "{w}");
        // A row's value binds like any param.
        let bind = Op::Bind {
            param: ParamPath::parse("/c.value1").unwrap(),
            component: None,
            from: "/lfo".parse().unwrap(),
            channel: None,
        };
        f.net = Arc::new(
            f.net
                .apply(
                    &builtin_types().unwrap(),
                    &[bind, set("/lfo.offset", 0.25), set("/lfo.amplitude", 0.0)],
                )
                .unwrap()
                .network,
        );
        f.clock.advance(1);
        assert_eq!(f.chans("/c").value(Some("chan1")), Some(0.25));
        // Fewer rows: the rest is kept but unused.
        f.net = Arc::new(f.net.apply(&builtin_types().unwrap(), &[set("/c.rows", 1_i64)]).unwrap().network);
        assert_eq!(f.chans("/c").channels().len(), 1);
    }

    #[test]
    fn globs() {
        use super::glob;
        for (p, n, want) in [
            ("*", "x", true),
            ("x", "x", true),
            ("x", "xy", false),
            ("pos*", "pos_a", true),
            ("*_a", "pos_a", true),
            ("p*s*a", "pos_a", true),
            ("p*z", "pos_a", false),
            ("**", "", true),
            ("a*b*c", "aXbYbZc", true),
        ] {
            assert_eq!(glob(p, n), want, "{p} {n}");
        }
    }

    #[test]
    fn merge_leaves_out_taken_names() {
        let mut f = F::new(&[
            create("/a", "chan.constant"),
            create("/b", "chan.constant"),
            create("/m", "chan.merge"),
            connect("/a", "/m", 0),
            connect("/b", "/m", 2),
        ]);
        assert_eq!(f.chans("/m").channels().len(), 1);
        let id = f.net.resolve(&"/m".parse().unwrap()).unwrap();
        assert!(f.cooker.status(id).unwrap().warning.unwrap().contains("left out: chan0"));
    }

    #[test]
    fn noise_is_smooth_bounded_and_seeded() {
        let mut f = F::new(&[create("/n", "chan.noise"), create("/s", "chan.noise"), set("/s.seed", 7)]);
        let (mut prev, mut max, mut differs) = (None::<f64>, 0.0f64, false);
        for _ in 0..600 {
            f.clock.advance(1);
            let v = f.value("/n");
            differs |= v != f.value("/s");
            max = max.max(v.abs());
            if let Some(p) = prev {
                assert!((v - p).abs() < 0.2, "jump from {p} to {v}");
            }
            prev = Some(v);
        }
        assert!(max > 0.3 && max <= 1.0, "{max}");
        assert!(differs);
        // The same time gives the same value in a fresh cooker.
        let mut g = F::new(&[create("/n", "chan.noise")]);
        g.clock.advance(600);
        assert_eq!(g.value("/n"), prev.unwrap());
    }

    #[test]
    fn filter_follows_with_its_time_constant() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            create("/f", "chan.filter"),
            set("/f.rise", 1.0),
            set("/f.fall", 0.0),
            connect("/c", "/f", 0),
        ]);
        assert_eq!(f.value("/f"), 0.0);
        f.net = Arc::new(f.net.apply(&builtin_types().unwrap(), &[set("/c.value0", 1.0)]).unwrap().network);
        // One second later, in one jump (as when the node was not asked for meanwhile): 63 %.
        f.clock.advance(60);
        let v = f.value("/f");
        assert!((v - (1.0 - (-1.0f64).exp())).abs() < 1e-9, "{v}");
        // Frame by frame gives the same curve.
        for _ in 0..60 {
            f.clock.advance(1);
            f.value("/f");
        }
        let v = f.value("/f");
        assert!((v - (1.0 - (-2.0f64).exp())).abs() < 1e-9, "{v}");
        // Falling with time 0 follows at once.
        f.net = Arc::new(f.net.apply(&builtin_types().unwrap(), &[set("/c.value0", 0.0)]).unwrap().network);
        f.clock.advance(1);
        assert_eq!(f.value("/f"), 0.0);
    }

    #[test]
    fn modulo_and_floor() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", -3.5),
            create("/mod", "chan.math"),
            set("/mod.op", "modulo"),
            set("/mod.b", 3.0),
            create("/floor", "chan.math"),
            set("/floor.op", "floor"),
            set("/floor.b", 2.0),
            connect("/c", "/mod", 0),
            connect("/c", "/floor", 0),
        ]);
        assert!((f.value("/mod") - 2.5).abs() < 1e-12);
        assert_eq!(f.value("/floor"), -4.0);
    }

    #[test]
    fn speed_adds_up_loops_and_resets() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 1.0),
            create("/s", "chan.speed"),
            set("/s.limit", "loop"),
            set("/s.max", 3.0),
            connect("/c", "/s", 0),
        ]);
        assert_eq!(f.value("/s"), 0.0);
        // 2 s at 1 per second.
        f.clock.advance(120);
        assert!((f.value("/s") - 2.0).abs() < 1e-9);
        // Frames nobody asked for still count; past max it starts again from min.
        f.clock.advance(90);
        assert!((f.value("/s") - 0.5).abs() < 1e-9);
        f.net = Arc::new(f.net.apply(&builtin_types().unwrap(), &[set("/s.reset", true)]).unwrap().network);
        f.clock.advance(1);
        assert_eq!(f.value("/s"), 0.0);
        f.net = Arc::new(
            f.net
                .apply(&builtin_types().unwrap(), &[set("/s.reset", false), set("/s.limit", "clamp")])
                .unwrap()
                .network,
        );
        f.clock.advance(600);
        assert_eq!(f.value("/s"), 3.0);
    }

    fn apply(f: &mut F, ops: &[Op]) {
        f.net = Arc::new(f.net.apply(&builtin_types().unwrap(), ops).unwrap().network);
    }

    fn samples(c: &Chans, name: &str) -> Vec<f64> {
        c.get(name).unwrap().samples.clone()
    }

    #[test]
    fn trail_samples_on_a_grid_of_show_time() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 1.0),
            create("/t", "chan.trail"),
            set("/t.length", 1.0),
            set("/t.rate", 10.0),
            connect("/c", "/t", 0),
        ]);
        let t = f.chans("/t");
        assert_eq!(t.rate(), 10.0);
        assert_eq!(samples(&t, "chan0"), vec![1.0; 10], "the first value fills the trail");
        // Half a second in one jump (nobody asked meanwhile) is five samples, all holding the
        // value now.
        apply(&mut f, &[set("/c.value0", 2.0)]);
        f.clock.advance(30);
        let s = samples(&f.chans("/t"), "chan0");
        assert_eq!(s, [vec![1.0; 5], vec![2.0; 5]].concat());
        // Frame by frame: one sample every six frames at 60 fps, none in between.
        apply(&mut f, &[set("/c.value0", 3.0)]);
        for _ in 0..5 {
            f.clock.advance(1);
            assert_eq!(samples(&f.chans("/t"), "chan0"), s, "no grid point passed yet");
        }
        f.clock.advance(1);
        assert_eq!(samples(&f.chans("/t"), "chan0").last(), Some(&3.0));
        // Paused (not active): held, and resuming does not fill the gap.
        apply(&mut f, &[set("/t.active", false), set("/c.value0", 4.0)]);
        f.clock.advance(60);
        let held = samples(&f.chans("/t"), "chan0");
        assert_eq!(held.last(), Some(&3.0));
        apply(&mut f, &[set("/t.active", true)]);
        f.clock.advance(60);
        let s = samples(&f.chans("/t"), "chan0");
        assert_eq!(s.iter().filter(|v| **v == 4.0).count(), 1, "{s:?}");
    }

    #[test]
    fn trail_follows_its_input_s_channels_and_its_length() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.rows", 2),
            set("/c.value0", 1.0),
            set("/c.value1", 5.0),
            create("/t", "chan.trail"),
            set("/t.length", 0.5),
            set("/t.rate", 4.0),
            connect("/c", "/t", 0),
        ]);
        let t = f.chans("/t");
        assert_eq!(t.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["chan0", "chan1"]);
        assert_eq!(t.len(), 2);
        // A channel goes away, another is renamed: the kept one keeps its history, the new one
        // starts flat.
        apply(&mut f, &[set("/c.value0", 2.0)]);
        f.clock.advance(15);
        assert_eq!(samples(&f.chans("/t"), "chan0"), [1.0, 2.0]);
        apply(&mut f, &[set("/c.rows", 1)]);
        f.clock.advance(15);
        let t = f.chans("/t");
        assert_eq!(t.channels().len(), 1);
        apply(&mut f, &[set("/c.rows", 2), set("/c.name1", "other"), set("/c.value1", 7.0)]);
        f.clock.advance(15);
        let t = f.chans("/t");
        assert_eq!(samples(&t, "chan0"), [2.0, 2.0]);
        assert_eq!(samples(&t, "other"), [7.0, 7.0]);
        // Longer: the oldest value fills the new start; shorter: the oldest drop off.
        apply(&mut f, &[set("/t.length", 1.0)]);
        f.clock.advance(15);
        assert_eq!(f.chans("/t").len(), 4);
        apply(&mut f, &[set("/t.length", 0.25)]);
        f.clock.advance(15);
        assert_eq!(f.chans("/t").len(), 1);
        // Capped memory.
        apply(&mut f, &[set("/t.length", 3600.0), set("/t.rate", 1000.0)]);
        f.clock.advance(1);
        assert_eq!(f.chans("/t").len(), super::TRAIL_MAX_SAMPLES);
        assert_eq!(super::trail_len(f64::NAN, 10.0, 1), (1, false));
        assert_eq!(super::trail_len(1.0, 10.0, 0), (10, false));
        assert!(super::trail_len(1e9, 1e9, 3).0 * 3 <= penumbra_core::chan::MAX_SAMPLES);
    }

    #[test]
    fn trail_starts_over_on_reset_and_seek() {
        let mut f = F::new(&[
            create("/lfo", "chan.lfo"),
            set("/lfo.shape", "saw"),
            create("/t", "chan.trail"),
            set("/t.length", 1.0),
            set("/t.rate", 60.0),
            connect("/lfo", "/t", 0),
        ]);
        let types = builtin_types().unwrap();
        assert!(types.desc("chan.trail").unwrap().stateful);
        for _ in 0..30 {
            f.clock.advance(1);
            f.chans("/t");
        }
        let s = samples(&f.chans("/t"), "value");
        assert_eq!(s.len(), 60);
        assert!(s.windows(2).skip(30).all(|w| w[1] > w[0]), "the saw rises: {s:?}");
        let id = f.net.resolve(&"/t".parse().unwrap()).unwrap();
        assert_eq!(f.cooker.reset(id), penumbra_graph::ResetOutcome::Reset);
        let now = f.chans("/t").value(None).unwrap();
        assert_eq!(samples(&f.chans("/t"), "value"), vec![now; 60]);
        f.clock.advance(30);
        f.chans("/t");
        f.clock.seek(0);
        let s = samples(&f.chans("/t"), "value");
        assert!(s.iter().all(|v| *v == s[0]), "a seek back starts over: {s:?}");
    }
}
