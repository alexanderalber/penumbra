//! `chan` nodes: named channels on the CPU.
//!
//! Generators (constant, LFO, noise) produce one channel with one sample per show frame, at the
//! show's rate; math and merge combine channels; the filter smooths them over time, the trail
//! keeps their recent history as samples. The data on a `chan` connection is a [`Chans`]. Any
//! number param of any node can be bound to a channel.

use std::collections::{BTreeMap, VecDeque};

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Tick, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use crate::{Null, waiting};

mod filter;

/// Registers the `chan` node types.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new("chan.constant", "Numbers set by hand, each a named channel: values to bind params to, one setting that drives several things")
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
    phased(
        types,
        NodeDesc::new("chan.lfo", "A number that swings up and down by itself (sine, triangle, square, saw): regular motion, pulsing, blinking")
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
        "rate",
        |cx, cycles| {
            let x = (cycles + cx.float("phase")?).rem_euclid(1.0);
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
    phased(
        types,
        NodeDesc::new(
            "chan.noise",
            "A number that wanders smoothly at random: organic motion that never repeats",
        )
        .param(
            ParamSpec::float("frequency", 1.0)
                .range(0.01, 50.0)
                .curve(Curve::Log)
                .unit(Unit::Hertz)
                .doc("How fast the noise wanders: about this many rises and falls per second"),
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
                .doc("How much fine wiggle rides on the motion: 1 is smoothest. Each octave adds a layer at twice the frequency of the one before"),
        )
        .param(
            ParamSpec::float("roughness", 0.5)
                .range(0.0, 1.0)
                .more()
                .doc("Strength of each octave relative to the one before; higher is more jittery"),
        )
        .time_dependent(),
        "frequency",
        |cx, t| {
            let octaves = u32::try_from(cx.int("octaves")?.clamp(1, 8)).unwrap_or(1);
            let n = fbm(t, cx.int("seed")?, octaves, cx.float("roughness")?);
            Ok(cx.float("offset")? + cx.float("amplitude")? * n)
        },
    )?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.math",
            "Calculates with channels: adds, multiplies or compares two inputs, or one input and a number",
        )
        .param(
            ParamSpec::choice(
                "op",
                &["add", "subtract", "multiply", "divide", "min", "max", "power", "modulo", "floor", "equal"],
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
            .choice_doc("floor", "a down to a multiple of b (b = 1: whole numbers; b = 0 leaves a)")
            .choice_doc(
                "equal",
                "1 where a equals b (to within a millionth), else 0: whether the piece channel is 3",
            ),
        )
        .param(
            ParamSpec::float("b", 0.0)
                .range(-1.0, 1.0)
                .doc("The b side as a number, used while input b is unconnected"),
        )
        .input("a", DataKind::Chan)
        .input("b", DataKind::Chan)
        .describe_input("b", "Each channel of a meets b's channel of the same name, else b's first channel. Unconnected: the param `b`")
        .output("out", DataKind::Chan),
        || Box::new(Math) as Box<dyn Operator>,
    ))?;
    filter::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.integrator",
            "Turns a speed into a position: adds up each channel over time, for steady turning, scrolling or drifting",
        )
        .stateful()
        .param(
            ParamSpec::choice("limit", &["none", "loop", "clamp"])
                .doc("Whether the sum is kept within min and max")
                .choice_doc("none", "the sum grows without end")
                .choice_doc(
                    "loop",
                    "past max it starts again from min (and the other way round): an angle, a scroll",
                )
                .choice_doc("clamp", "the sum stops at min and max"),
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
        || Box::new(Integrator::default()) as Box<dyn Operator>,
    ))?;
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                "chan.derivative",
                "How fast each channel changes: a position in, a speed out, for reacting to movement or to a rising level",
            )
            .stateful()
            .param(
                ParamSpec::choice("per", &["second", "frame"])
                    .doc("What the change is counted over")
                    .choice_doc("second", "change per second, the same at any frame rate")
                    .choice_doc(
                        "frame",
                        "change per show frame (per sample for a channel with many samples)",
                    ),
            )
            .param(
                ParamSpec::choice("limit", &["none", "clamp"])
                    .doc("Whether the output is kept within min and max")
                    .choice_doc("none", "a jump in the input gives a large spike")
                    .choice_doc("clamp", "the output stops at min and max"),
            )
            .param(ParamSpec::float("min", -1.0).range(-10.0, 10.0).doc("Lowest output (limit clamp)"))
            .param(ParamSpec::float("max", 1.0).range(-10.0, 10.0).doc("Highest output (limit clamp)"))
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .time_dependent(),
            || Box::new(Derivative::default()) as Box<dyn Operator>,
        )
        .inactive(|node, desc, name| {
            let clamp = node.param(desc, "limit").and_then(Value::as_str) == Some("clamp");
            (matches!(name, "min" | "max") && !clamp).then_some("Only counts with limit clamp")
        }),
    )?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            CLOCK,
            "A stopwatch: counts seconds from when `run` turns on, once, looping or on and on. Gives each \
             piece of a show a clock that starts with it",
        )
        .stateful()
        .param(ParamSpec::float("run", 1.0).range(0.0, 1.0).doc(
            "On from 0.5 up (with `equals` set: while it equals that number). Each time it turns on the \
             clock starts over from 0; while it is off the clock stands at 0. Bind it to a key's gate, \
             or to the piece channel with `equals`",
        ))
        .param(ParamSpec::int("equals", -1).clamp(-1.0, 1e9).range(-1.0, 20.0).special(-1.0, "off").doc(
            "When set, `run` is on while it equals this number: bind `run` to the channel that \
                     says which piece plays and set this to the piece's number. While off, `run` is on \
                     from 0.5 up",
        ))
        .param(
            ParamSpec::float("length", 10.0)
                .clamp(CLOCK_MIN_LENGTH, 1e6)
                .range(0.1, 120.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("How long one run (once) or one round (loop) takes; `fraction` is the time over it"),
        )
        .param(
            ParamSpec::choice("mode", &["once", "loop", "free"])
                .doc("What the clock does once `length` has passed")
                .choice_doc("once", "it stops at the end and holds there")
                .choice_doc("loop", "it starts over from 0, round after round")
                .choice_doc("free", "it runs on: seconds grow without end, fraction past 1"),
        )
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`seconds` since it was switched on (once: at most `length`; loop: within the round), \
             `fraction` of `length` (0 to 1 for once and loop), `done` (1 once a full length has passed \
             since it was switched on, else 0). It cooks in every frame, shown or not, so it sees `run` \
             turn off while its piece is not shown; a seek back starts it over",
        )
        .time_dependent()
        .eager(),
        || Box::new(Clock::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.trail",
            "Records the recent past of each channel: a graph over time to look at, or a history to work with",
        )
        .stateful()
        .sampled()
        .param(
            ParamSpec::float("length", 5.0)
                .clamp(TRAIL_MIN_LENGTH, TRAIL_MAX_LENGTH)
                .range(0.1, 60.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("How far back the trail reaches once it is full; older values drop off its start"),
        )
        .param(
            ParamSpec::float("rate", 60.0)
                .clamp(TRAIL_MIN_RATE, TRAIL_MAX_RATE)
                .range(1.0, 240.0)
                .curve(Curve::Log)
                .unit(Unit::Hertz)
                .doc(
                    "Samples taken per second of show time, whatever the frame rate; length times \
                     rate is the number of samples of a full trail",
                ),
        )
        .param(ParamSpec::bool("active", true).doc(
            "Records while on. Off holds the trail as it is; recording resumes from the moment active \
             is on again",
        ))
        .input("in", DataKind::Chan)
        .output("out", DataKind::Chan)
        .describe_input(
            "in",
            "The channels to record. Of each channel the trail takes its current value (the last sample)",
        )
        .describe_output(
            "out",
            "The input's channels by name, the newest sample last. The trail starts with one sample \
             (when made, reset or sought back) and grows by one per sample taken, up to length times rate",
        )
        .time_dependent(),
        || Box::new(Trail::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.merge", "Puts the channels of its inputs together into one")
            .input_run("in", DataKind::Chan, 2, "The channels to put together, in0's first")
            .renamed_input("a", "in0")
            .renamed_input("b", "in1")
            .renamed_input("c", "in2")
            .renamed_input("d", "in3")
            .output("out", DataKind::Chan),
        || Box::new(Merge) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.select",
            "Picks some of the input's channels by name or pattern, in the order given",
        )
        .param(ParamSpec::text("channels", "*").doc(
            "Space-separated names, `*` matches any run of characters: `x y pos*`; `^name` leaves \
             channels out. To rename, add a chan.rename",
        ))
        .input("in", DataKind::Chan)
        .output("out", DataKind::Chan),
        || Box::new(Select) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "chan.rename",
            "Renames channels by `from=to` pairs, so they match what other nodes and bindings expect",
        )
        .param(ParamSpec::text("names", "").doc(
            "Space-separated `from=to` pairs, the first match wins: `tx=x ty=y`. With one `*` \
                 on each side the matched part carries over: `pos*=p*` makes pos_a into p_a",
        ))
        .input("in", DataKind::Chan)
        .output("out", DataKind::Chan),
        || Box::new(Rename) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("chan.null", "Passes its input on unchanged: a fixed point to look at, or to bind params to while the chain before it changes")
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
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
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
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
    }
}

/// The channel name every generator declares last.
fn name_param() -> ParamSpec {
    ParamSpec::text("name", "value").doc("Name of the channel it makes, as bindings and other nodes see it")
}

fn out(chans: Chans) -> Result<Cooked, CookError> {
    Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
}

/// The longest step between two cooks that a [`Cycles`] runs across; a longer one (the node was
/// not cooked for a while) starts over at the steady position.
const MAX_CYCLES_STEP: f64 = 0.5;

/// How far a generator has run along its wave, in waves (or noise cells), for a rate that may
/// change while it runs. Each cook advances it by the rate times the time since the last cook,
/// so a new rate bends the wave instead of jumping. It starts at `time * rate`, the position a
/// steady rate gives: on the first cook, after a seek backwards, after a step of more than
/// [`MAX_CYCLES_STEP`]. A recook at the same time (paused) keeps the position. For a steady rate it gives
/// `time * rate`, so a render from the start matches a live run.
#[derive(Default)]
struct Cycles {
    /// The time and rate of the last cook, and how far the position there was ahead of
    /// `time * rate`. Kept as that offset, so a steady rate gives the formula exactly, without
    /// the rounding a sum of steps would gather.
    last: Option<(Tick, f64, f64)>,
}

impl Cycles {
    fn advance(&mut self, now: Tick, rate: f64) -> f64 {
        let offset = match self.last {
            // Recooked at the same time (paused): the position stays where it was, whatever
            // changed, so a paused edit does not jump.
            Some((at, last_rate, offset)) if now.0 == at.0 => {
                at.seconds() * last_rate + offset - now.seconds() * rate
            }
            Some((at, last_rate, offset))
                if now.0 > at.0 && Tick(now.0 - at.0).seconds() <= MAX_CYCLES_STEP =>
            {
                if last_rate == rate {
                    offset
                } else {
                    let then = at.seconds() * last_rate + offset;
                    then + rate * Tick(now.0 - at.0).seconds() - now.seconds() * rate
                }
            }
            _ => 0.0,
        };
        let offset = if offset.is_finite() { offset } else { 0.0 };
        self.last = Some((now, rate, offset));
        now.seconds() * rate + offset
    }
}

/// A generator driven by a rate param: computes one number per frame from the position along
/// its wave ([`Cycles`]) into channel `name`.
struct Phased<F> {
    rate: &'static str,
    cycles: Cycles,
    f: F,
}

impl<F: Fn(&CookContext<'_>, f64) -> Result<f64, CookError> + Send> Operator for Phased<F> {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let cycles = self.cycles.advance(cx.time().tick, cx.float(self.rate)?);
        let v = (self.f)(cx, cycles)?;
        let name = Name::new(cx.text("name")?).map_err(|e| e.context("`name`"))?;
        out(Chans::single(name, v, cx.rate().as_f64())?)
    }
}

/// Registers a [`Phased`] generator whose speed is the float param `rate`.
fn phased<F>(types: &mut NodeTypes, desc: NodeDesc, rate: &'static str, f: F) -> Result<(), Error>
where
    F: Fn(&CookContext<'_>, f64) -> Result<f64, CookError> + Clone + Send + Sync + 'static,
{
    let mut desc = desc.output("out", DataKind::Chan);
    desc.params.push(name_param());
    types.register(FnNodeType::new(desc, move || {
        Box::new(Phased { rate, cycles: Cycles::default(), f: f.clone() }) as Box<dyn Operator>
    }))
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
pub(crate) fn hash(i: i64, seed: i64) -> u32 {
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
            "equal" => |a, b| if equal(a, b) { 1.0 } else { 0.0 },
            _ => |a, b| a + b,
        };
        // The layout (names, length, rate) comes from a, else from b.
        let shape = a.or(b);
        let (layout, rate) = match shape {
            Some(c) => {
                (c.channels().iter().map(|ch| (ch.name.clone(), ch.samples.len())).collect(), c.rate())
            }
            None => (vec![(Name::new("value")?, 1)], cx.rate().as_f64()),
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
        let made = Chans::new(rate, channels)?;
        out(match shape {
            Some(c) => made.shaped_like(c),
            None => made,
        })
    }
}

/// Sample `i`, or the last one when the channel is shorter (a single value applies to all).
fn sample(c: &Channel, i: usize) -> f64 {
    c.samples.get(i).or(c.samples.last()).copied().unwrap_or(0.0)
}

/// Integrates each channel: per-frame channels by the time since the last cook (like
/// `chan.filter`), channels with many samples by their sample period.
#[derive(Default)]
struct Integrator {
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

impl Operator for Integrator {
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
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
    }
}

/// The change of each channel: per-frame channels over the time since the last cook (like
/// [`Integrator`]), channels with many samples from sample to sample. The first value of a
/// channel gives 0; a cook at the same time as the last gives the last output again.
#[derive(Default)]
struct Derivative {
    /// Last input and last output per channel.
    state: BTreeMap<Name, (f64, f64)>,
    last: Option<Tick>,
}

impl Operator for Derivative {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let Some(input) = cx.input_as::<Chans>(0) else {
            self.state.clear();
            self.last = None;
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        let per_frame = cx.text("per")? == "frame";
        let clamp = cx.text("limit")? == "clamp";
        let (lo, hi) = (cx.float("min")?, cx.float("max")?);
        // Going back in time (a seek) starts over.
        let dt = match self.last {
            Some(last) if now.0 >= last.0 => Tick(now.0 - last.0).seconds(),
            _ => {
                self.state.clear();
                0.0
            }
        };
        self.last = Some(now);
        let per_sample = input.len() > 1;
        let step = if per_sample { 1.0 / input.rate() } else { dt };
        // What one unit of the output counts over, in seconds.
        let unit = match (per_frame, per_sample) {
            (false, _) => 1.0,
            (true, true) => 1.0 / input.rate(),
            (true, false) => 1.0 / cx.rate().as_f64(),
        };
        let mut state = BTreeMap::new();
        let mut channels = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut prev = self.state.get(&ch.name).copied();
            let mut samples = Vec::with_capacity(ch.samples.len());
            for &x in &ch.samples {
                let y = match prev {
                    None => 0.0,
                    Some((_, y)) if step <= 0.0 => y,
                    Some((p, _)) => {
                        let d = (x - p) / step * unit;
                        if clamp { d.clamp(lo.min(hi), hi.max(lo)) } else { d }
                    }
                };
                prev = Some((x, y));
                samples.push(y);
            }
            if let Some(p) = prev {
                state.insert(ch.name.clone(), p);
            }
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        self.state = state;
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
    }
}

/// The kind of the restartable clock.
pub const CLOCK: &str = "chan.clock";
/// Shortest `length` of a `chan.clock`, in seconds.
const CLOCK_MIN_LENGTH: f64 = 0.001;

/// Whether `a` and `b` count as equal for `chan.math`'s `equal` and `chan.clock`'s `equals`:
/// within a millionth, so numbers that went through a sum or a binding still match.
fn equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6
}

/// `chan.clock`: the show time when it was switched on, and the time it was last computed.
/// It cooks in every pass ([`penumbra_graph::NodeDesc::eager`]), so `run` turning off is seen
/// even while nothing shows the clock. (Starting over after half a second without a cook
/// instead never happened while the editor's thumbnails cooked every node, and would strike
/// mid-piece after a frame late by that much.)
#[derive(Default)]
struct Clock {
    start: Option<Tick>,
    last: Option<Tick>,
}

/// What a clock shows `elapsed` seconds after it was switched on: seconds, fraction, done.
fn clock_values(elapsed: f64, length: f64, mode: &str) -> [f64; 3] {
    let length = length.max(CLOCK_MIN_LENGTH);
    let elapsed = elapsed.max(0.0);
    let done = if elapsed >= length { 1.0 } else { 0.0 };
    let seconds = match mode {
        "loop" => elapsed.rem_euclid(length),
        "free" => elapsed,
        _ => elapsed.min(length),
    };
    [seconds, seconds / length, done]
}

impl Operator for Clock {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let run = cx.float("run")?;
        let equals = cx.int("equals")?;
        let on = if equals >= 0 { equal(run, equals as f64) } else { run >= 0.5 };
        // A seek back starts it over.
        let back = self.last.is_some_and(|last| now.0 < last.0);
        self.last = Some(now);
        if !on {
            self.start = None;
        } else if self.start.is_none() || back {
            self.start = Some(now);
        }
        let elapsed = self.start.map_or(0.0, |s| Tick(now.0.saturating_sub(s.0)).seconds());
        let [seconds, fraction, done] = clock_values(elapsed, cx.float("length")?, cx.text("mode")?);
        let rate = cx.rate().as_f64();
        let channel = |name: &str, v: f64| -> Result<Channel, CookError> {
            Ok(Channel { name: Name::new(name)?, samples: vec![v] })
        };
        let channels =
            vec![channel("seconds", seconds)?, channel("fraction", fraction)?, channel("done", done)?];
        out(Chans::new(rate, channels)?)
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
/// time; the show's frame rate and the trail's rate need not match. A new trail (and one reset
/// or sought back) starts with one sample and grows until it holds `length` times `rate`, then
/// drops its oldest sample for each new one. Its output names the full length as its window
/// ([`Chans::with_window`]), so a plot puts a growing trail at its place in time.
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
        // The channels as the input has them now: known ones keep their history, a new one
        // starts flat at its value as far back as the others reach, gone ones are dropped.
        let filled = self.channels.first().map_or(0, |(_, h)| h.len()).min(len);
        let mut known = std::mem::take(&mut self.channels);
        self.channels = input
            .channels()
            .iter()
            .map(|ch| {
                let history = match known.iter().position(|(n, _)| *n == ch.name) {
                    Some(i) => known.swap_remove(i).1,
                    None => VecDeque::from(vec![ch.value(); filled]),
                };
                (ch.name.clone(), history)
            })
            .collect();
        // A shorter trail drops its oldest samples; a longer one grows into the room.
        for (_, h) in &mut self.channels {
            while h.len() > len {
                h.pop_front();
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
        // A trail with nothing yet (just made, reset, or paused before its first sample) still
        // says its input's value now.
        let due = usize::try_from(due).unwrap_or(0).min(len).max(usize::from(filled == 0));
        for ((_, h), ch) in self.channels.iter_mut().zip(input.channels()) {
            let v = ch.value();
            for _ in 0..due {
                if h.len() >= len {
                    h.pop_front();
                }
                h.push_back(v);
            }
        }
        let channels = self
            .channels
            .iter()
            .map(|(name, h)| Channel { name: name.clone(), samples: h.iter().copied().collect() })
            .collect();
        out(Chans::new(rate, channels)?.with_window(len))
    }
}

struct Merge;

impl Operator for Merge {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let inputs: Vec<&Chans> =
            (0..cx.def().inputs.len()).filter_map(|i| cx.input_as::<Chans>(i)).collect();
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
        out(Chans::new(first.rate(), channels)?.shaped_like(first))
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
    fn lfo_bends_on_a_rate_change_and_starts_over_on_a_seek() {
        let types = builtin_types().unwrap();
        let mut f = F::new(&[create("/lfo", "chan.lfo"), set("/lfo.shape", "saw"), set("/lfo.rate", 0.5)]);
        // A steady rate gives the old formula: 0.5 Hz at frame k is k / 120 waves.
        let saw = |cycles: f64| 2.0 * cycles.rem_euclid(1.0) - 1.0;
        for k in 1..=30 {
            f.clock.advance(1);
            assert!((f.value("/lfo") - saw(k as f64 / 120.0)).abs() < 1e-9, "frame {k}");
        }
        // Four times the rate: the next step is four steps of the old rate, not a jump to the
        // value that rate gives at this time.
        let before = f.value("/lfo");
        f.net = Arc::new(f.net.apply(&types, &[set("/lfo.rate", 2.0)]).unwrap().network);
        f.clock.advance(1);
        let after = f.value("/lfo");
        let step = 2.0 * 2.0 / 60.0;
        assert!((after - before - step).abs() < 1e-9, "{before} -> {after}");
        // A seek back starts over at time * rate.
        f.clock.seek(10);
        assert!((f.value("/lfo") - saw(10.0 * 2.0 / 60.0)).abs() < 1e-9);
        // A phase change shifts at once.
        f.net = Arc::new(f.net.apply(&types, &[set("/lfo.phase", 0.25)]).unwrap().network);
        f.clock.advance(1);
        assert!((f.value("/lfo") - saw(11.0 * 2.0 / 60.0 + 0.25)).abs() < 1e-9);
        // Paused, a recook at the same time keeps the value, whatever changed.
        f.net = Arc::new(f.net.apply(&types, &[set("/lfo.rate", 1.0)]).unwrap().network);
        assert!((f.value("/lfo") - saw(11.0 * 2.0 / 60.0 + 0.25)).abs() < 1e-9);
        // Running on, it continues from there at the new rate.
        f.clock.advance(1);
        assert!((f.value("/lfo") - saw(11.0 * 2.0 / 60.0 + 1.0 / 60.0 + 0.25)).abs() < 1e-9);
        // A jump of more than half a second forward starts over too.
        f.clock.advance(60);
        assert!((f.value("/lfo") - saw(72.0 / 60.0 + 0.25)).abs() < 1e-9);
    }

    #[test]
    fn noise_frequency_change_keeps_the_value_continuous() {
        let types = builtin_types().unwrap();
        let mut f = F::new(&[create("/n", "chan.noise"), set("/n.frequency", 0.5)]);
        let mut g = F::new(&[create("/n", "chan.noise"), set("/n.frequency", 0.5)]);
        f.clock.advance(120);
        f.value("/n");
        f.clock.advance(1);
        let before = f.value("/n");
        f.net = Arc::new(f.net.apply(&types, &[set("/n.frequency", 20.0)]).unwrap().network);
        f.clock.advance(1);
        let after = f.value("/n");
        // The position runs on by 20 Hz times one frame from where 0.5 Hz left it.
        let want = super::fbm(121.0 / 60.0 * 0.5 + 20.0 / 60.0, 0, 2, 0.5);
        assert!((after - want).abs() < 1e-9, "{before} -> {after}, want {want}");
        g.clock.advance(121);
        assert_eq!(g.value("/n"), before, "a steady frequency matches a fresh run");
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

    impl F {
        fn edit(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply(&builtin_types().unwrap(), ops).unwrap().network);
        }

        /// The filter `/f` behind constant `/c` (0), after `/c` steps to 1 and `seconds` pass,
        /// once frame by frame and once in one jump.
        fn step_response(ops: &[Op], seconds: f64) -> (f64, f64) {
            let frames = (seconds * 60.0).round() as u64;
            let mut both = [0.0; 2];
            for (jump, v) in [false, true].into_iter().zip(&mut both) {
                let mut base =
                    vec![create("/c", "chan.constant"), create("/f", "chan.filter"), connect("/c", "/f", 0)];
                base.extend_from_slice(ops);
                let mut f = F::new(&base);
                assert_eq!(f.value("/f"), 0.0);
                f.edit(&[set("/c.value0", 1.0)]);
                if jump {
                    f.clock.advance(frames);
                } else {
                    for _ in 0..frames {
                        f.clock.advance(1);
                        f.value("/f");
                    }
                }
                *v = f.value("/f");
            }
            (both[0], both[1])
        }
    }

    #[test]
    fn filter_types_keep_their_time() {
        let t63 = 1.0 - (-1.0f64).exp();
        for kind in ["critical", "bessel", "butterworth"] {
            for order in 2..=4_i64 {
                let ops = [set("/f.type", kind), set("/f.order", order), set("/f.rise", 0.5)];
                let (frames, jump) = F::step_response(&ops, 0.5);
                assert!((frames - t63).abs() < 1e-9, "{kind} {order}: {frames}");
                assert!((jump - t63).abs() < 1e-9, "{kind} {order}: {jump}");
            }
        }
        // The steady wobble of the window's own period cancels in box; half the window is half way.
        let (frames, _) = F::step_response(&[set("/f.type", "box"), set("/f.window", 1.0)], 0.5);
        assert!((frames - 0.5).abs() < 1e-9, "{frames}");
        let (frames, _) = F::step_response(&[set("/f.type", "box"), set("/f.window", 1.0)], 2.0);
        assert!((frames - 1.0).abs() < 1e-9, "{frames}");
        let (frames, _) = F::step_response(&[set("/f.type", "gaussian"), set("/f.window", 1.0)], 0.25);
        assert!(frames > 0.3 && frames < 0.9, "{frames}");
        // Slew: 2 per second for a quarter second.
        let (frames, jump) = F::step_response(&[set("/f.type", "slew"), set("/f.up", 2.0)], 0.25);
        assert!((frames - 0.5).abs() < 1e-9 && (jump - 0.5).abs() < 1e-9, "{frames} {jump}");
        // one_euro without beta is a first-order lag with time constant rise.
        let (frames, _) =
            F::step_response(&[set("/f.type", "one_euro"), set("/f.beta", 0.0), set("/f.rise", 0.5)], 0.5);
        assert!((frames - t63).abs() < 1e-9, "{frames}");
        // And follows faster with beta.
        let (fast, _) =
            F::step_response(&[set("/f.type", "one_euro"), set("/f.beta", 5.0), set("/f.rise", 0.5)], 0.5);
        assert!(fast > frames + 0.05, "{fast}");
    }

    #[test]
    fn filter_switching_type_carries_on() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            create("/f", "chan.filter"),
            set("/f.rise", 1.0),
            // A step is a fast move to one_euro, which then catches up at once.
            set("/f.beta", 0.0),
            connect("/c", "/f", 0),
        ]);
        f.value("/f");
        f.edit(&[set("/c.value0", 1.0)]);
        f.clock.advance(30);
        let before = f.value("/f");
        for kind in ["butterworth", "box", "one_euro", "slew", "bessel", "first_order"] {
            f.edit(&[set("/f.type", kind)]);
            f.clock.advance(1);
            let v = f.value("/f");
            assert!((v - before).abs() < 0.1, "{kind}: {before} to {v}");
            assert!(v >= before - 1e-9, "{kind} went back from {before} to {v}");
        }
    }

    #[test]
    fn derivative_counts_change_per_second_or_frame() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 2.0),
            create("/i", "chan.integrator"),
            connect("/c", "/i", 0),
            create("/d", "chan.derivative"),
            connect("/i", "/d", 0),
        ]);
        assert_eq!(f.value("/d"), 0.0);
        f.clock.advance(10);
        assert!((f.value("/d") - 2.0).abs() < 1e-9);
        // A second cook at the same time keeps the value.
        assert!((f.value("/d") - 2.0).abs() < 1e-9);
        f.edit(&[set("/d.per", "frame")]);
        f.clock.advance(3);
        assert!((f.value("/d") - 2.0 / 60.0).abs() < 1e-9);
        f.edit(&[set("/d.per", "second"), set("/d.limit", "clamp"), set("/d.max", 1.5)]);
        f.clock.advance(1);
        assert_eq!(f.value("/d"), 1.5);
    }

    #[test]
    fn integrator_adds_up_loops_and_resets() {
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 1.0),
            create("/s", "chan.integrator"),
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

    #[test]
    fn clock_values_per_mode() {
        use super::clock_values;
        assert_eq!(clock_values(2.0, 8.0, "once"), [2.0, 0.25, 0.0]);
        assert_eq!(clock_values(9.0, 8.0, "once"), [8.0, 1.0, 1.0]);
        assert_eq!(clock_values(10.0, 8.0, "loop"), [2.0, 0.25, 1.0]);
        assert_eq!(clock_values(10.0, 8.0, "free"), [10.0, 1.25, 1.0]);
        // A length of 0 cannot divide by zero; time before the start is the start.
        assert!(clock_values(1.0, 0.0, "loop").iter().all(|v| v.is_finite()));
        assert_eq!(clock_values(-1.0, 8.0, "once"), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn clock_starts_over_with_its_piece() {
        let mut f = F::new(&[
            create("/piece", "chan.constant"),
            create("/clock", "chan.clock"),
            set("/clock.equals", 2_i64),
            set("/clock.length", 1.0),
            Op::Bind {
                param: ParamPath::parse("/clock.run").unwrap(),
                component: None,
                from: "/piece".parse().unwrap(),
                channel: None,
            },
        ]);
        let seconds = |f: &mut F| f.chans("/clock").value(Some("seconds")).unwrap();
        // Another piece plays: the clock stands at 0.
        assert_eq!(seconds(&mut f), 0.0);
        f.clock.advance(15);
        assert_eq!(seconds(&mut f), 0.0);
        // Its piece: from 0, a quarter second per 15 frames.
        apply(&mut f, &[set("/piece.value0", 2.0)]);
        f.clock.advance(15);
        assert_eq!(seconds(&mut f), 0.0);
        f.clock.advance(15);
        assert!((seconds(&mut f) - 0.25).abs() < 1e-9);
        // Once: it holds at the end and says it is done.
        for _ in 0..8 {
            f.clock.advance(15);
            seconds(&mut f);
        }
        let c = f.chans("/clock");
        assert_eq!((c.value(Some("seconds")), c.value(Some("done"))), (Some(1.0), Some(1.0)));
        // Another piece and back again: from 0.
        apply(&mut f, &[set("/piece.value0", 1.0)]);
        f.clock.advance(15);
        assert_eq!(seconds(&mut f), 0.0);
        apply(&mut f, &[set("/piece.value0", 2.0)]);
        f.clock.advance(15);
        assert_eq!(seconds(&mut f), 0.0);
        assert_eq!(f.chans("/clock").value(Some("done")), Some(0.0));
        // A second later it has run on (it no longer starts over after a pause between cooks).
        f.clock.advance(15);
        assert!((seconds(&mut f) - 0.25).abs() < 1e-9);
        f.clock.advance(60);
        assert_eq!(seconds(&mut f), 1.0);
        // Looping, and on from 0.5 up without `equals` (off for a frame first).
        apply(&mut f, &[set("/piece.value0", 0.0)]);
        seconds(&mut f);
        apply(&mut f, &[set("/clock.mode", "loop"), set("/clock.equals", -1_i64), set("/piece.value0", 1.0)]);
        seconds(&mut f);
        for _ in 0..5 {
            f.clock.advance(15);
            seconds(&mut f);
        }
        let c = f.chans("/clock");
        assert!((c.value(Some("seconds")).unwrap() - 0.25).abs() < 1e-9, "{c:?}");
        assert!((c.value(Some("fraction")).unwrap() - 0.25).abs() < 1e-9);
        apply(&mut f, &[set("/piece.value0", 0.25)]);
        f.clock.advance(15);
        assert_eq!(seconds(&mut f), 0.0);
    }

    #[test]
    fn math_equal() {
        let mut f =
            F::new(&[create("/c", "chan.constant"), create("/m", "chan.math"), set("/m.op", "equal")]);
        apply(&mut f, &[connect("/c", "/m", 0), set("/c.value0", 3.0), set("/m.b", 3.0)]);
        assert_eq!(f.value("/m"), 1.0);
        apply(&mut f, &[set("/m.b", 2.0)]);
        assert_eq!(f.value("/m"), 0.0);
        apply(&mut f, &[set("/m.b", 3.0 + 1e-9)]);
        assert_eq!(f.value("/m"), 1.0);
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
        assert_eq!(samples(&t, "chan0"), [1.0], "a new trail starts with one sample");
        assert_eq!(t.window(), 10, "and says how long it grows");
        // Half a second in one jump (nobody asked meanwhile) is five samples, all holding the
        // value now.
        apply(&mut f, &[set("/c.value0", 2.0)]);
        f.clock.advance(30);
        let s = samples(&f.chans("/t"), "chan0");
        assert_eq!(s, [vec![1.0], vec![2.0; 5]].concat());
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
        assert_eq!(s.len(), 8);
        // Full: from then on the oldest drop off.
        f.clock.advance(60);
        let t = f.chans("/t");
        assert_eq!((t.len(), t.window()), (10, 10));
        assert_eq!(samples(&t, "chan0"), [4.0; 10]);
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
        assert_eq!(t.len(), 1);
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
        // Longer: grows into the room; shorter: the oldest drop off.
        apply(&mut f, &[set("/t.length", 1.0)]);
        f.clock.advance(15);
        assert_eq!((f.chans("/t").len(), f.chans("/t").window()), (3, 4));
        f.clock.advance(15);
        assert_eq!(f.chans("/t").len(), 4);
        apply(&mut f, &[set("/t.length", 0.25)]);
        f.clock.advance(15);
        assert_eq!(f.chans("/t").len(), 1);
        // Capped memory.
        apply(&mut f, &[set("/t.length", 3600.0), set("/t.rate", 1000.0)]);
        f.clock.advance(1);
        assert_eq!(f.chans("/t").window(), super::TRAIL_MAX_SAMPLES);
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
        assert_eq!(s.len(), 30, "one sample per frame since the first cook");
        assert!(s.windows(2).all(|w| w[1] > w[0]), "the saw rises: {s:?}");
        let id = f.net.resolve(&"/t".parse().unwrap()).unwrap();
        assert_eq!(f.cooker.reset(id), penumbra_graph::ResetOutcome::Reset);
        let now = f.chans("/t").value(None).unwrap();
        assert_eq!(samples(&f.chans("/t"), "value"), [now]);
        f.clock.advance(30);
        f.chans("/t");
        f.clock.seek(0);
        let s = samples(&f.chans("/t"), "value");
        assert_eq!(s.len(), 1, "a seek back starts over: {s:?}");
    }
}
