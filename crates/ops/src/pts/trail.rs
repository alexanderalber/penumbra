//! `pts.trail`: a line behind every point through the places it had, TouchDesigner's Trail POP.
//!
//! Each step the node puts every point at the head of its trail and moves the older places one
//! back; the oldest falls off the tail. A trail is `length` points long, the head included. The
//! points are matched across steps by an attribute (`id`, as `pts.particles` gives it), or by
//! their index where they lack it. A new point starts with a trail of its own place, which
//! grows from there; a point that is gone takes its trail with it.
//!
//! What comes out: `length` points per input point, head first, as lines (`Strips::Regular`)
//! that `geo.from_pts` draws in lines mode, with `trail` from 0 at the head to 1 at the tail
//! and the attributes named in `attributes`. Time follows the show clock: a step per frame,
//! none while the show stands still, so the same steps give the same trails.

use penumbra_core::{Curve, Error, ParamSpec, Tick};
use penumbra_gpu::points::trail::{self, Key, Settings, Trailer};
use penumbra_gpu::points::{AttrType, Points, canonical};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::wake;
use crate::tex::fluid::step;
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.trail";

/// Most points per trail.
pub const MAX_LENGTH: u32 = 1024;

/// The default of `max`.
pub const DEFAULT_MAX: u32 = 100_000;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Trail::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(trail::warm_up)),
    )
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "A trail behind every point: a line through the places it had over the last frames, newest first. geo.from_pts draws it in lines mode",
    )
    .stateful()
    .time_dependent()
    .param(
        ParamSpec::int("length", 16)
            .clamp(2.0, f64::from(MAX_LENGTH))
            .range(2.0, 128.0)
            .curve(Curve::Log)
            .doc("Points in each trail, the point itself included: how many places back it reaches"),
    )
    .param(
        ParamSpec::int("every", 1)
            .clamp(1.0, 1000.0)
            .range(1.0, 30.0)
            .doc("Frames between two places kept. 2 keeps every second frame: a trail twice as long for the same `length`. The head follows the point every frame"),
    )
    .param(
        ParamSpec::text("match", "id")
            .doc("The attribute that tells a point from frame to frame, as pts.particles gives `id`. Points without it keep their trail by their place in the list: right for a set whose points stay in order")
            .more(),
    )
    .param(
        ParamSpec::text("attributes", "color width scale")
            .doc("Attributes the trail carries besides position, separated by spaces: each place keeps the value the point had then. The ones the points lack are left out")
            .more(),
    )
    .param(
        ParamSpec::int("max", i64::from(DEFAULT_MAX))
            .clamp(1.0, 64.0 * 1024.0 * 1024.0)
            .range(1.0, 1_000_000.0)
            .curve(Curve::Log)
            .doc("The most trails: points past this many have none. Memory: the trails are sized for as many points as the input may hold (a particle node's `max`) up to this many, times `length`, times the attributes carried, twice. Too many for memory: fewer points get a trail, with a warning")
            .more(),
    )
    .param(ParamSpec::bool("reset", false).doc("Trails start over at every frame while on: each a single place"))
    .input("in", DataKind::Pts)
    .describe_input("in", "The points that leave trails. Their number may change from frame to frame, as particles do")
    .output("out", DataKind::Pts)
    .describe_output(
        "out",
        "`length` points per input point, head first, joined into lines: position, `trail` (0 at the head, 1 at the tail), the matching attribute and the ones in `attributes`",
    )
}

#[derive(Default)]
struct Trail {
    trailer: Trailer,
    /// The trails the last step gave out.
    before: Option<Points>,
    /// Steps taken since the start, for `every`.
    steps: u64,
    last: Option<Tick>,
}

impl Trail {
    /// What the step left out, in words: points past `max`, or past what memory allows, and
    /// trails lost to keys that do not rise.
    fn speak(&mut self, cx: &CookContext<'_>, input: &Points, set: &Settings) {
        let max = set.max;
        if let Some(why) = self.trailer.cut() {
            cx.warn(why);
        } else if let Some(n) = input.count().seen().filter(|n| *n > max) {
            cx.warn(format!(
                "{n} points and `max` keeps {max} trails: the points past it have none. Raise `max`"
            ));
        }
        if let Key::Attr(name) = &set.key
            && self.trailer.unordered(&set.key)
        {
            cx.warn(format!(
                "the points' `{name}` values do not rise along the list, so trails cannot find their points and start over every frame. This happens after pts.merge of particle systems whose ids overlap, after a node that writes `{name}`, and for a trail of trails. Set `match` empty to follow the points by their index"
            ));
        }
    }
}

impl Operator for Trail {
    fn gpu_bytes(&self) -> u64 {
        self.trailer.bytes()
    }

    fn trim(&mut self) {
        self.trailer.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let Some(input) = cx.input_as::<Points>(0) else {
            return Err(if cx.def().inputs.first().is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect the points that leave trails to `in`")
            });
        };
        let now = cx.time();
        let period = 1.0 / cx.rate().as_f64().max(1.0);
        let dt = step(now.tick, self.last, period);
        let len = u32::try_from(cx.int("length")?.clamp(2, i64::from(MAX_LENGTH))).unwrap_or(16);
        let every = u64::try_from(cx.int("every")?.max(1)).unwrap_or(1);
        let max = u32::try_from(cx.int("max")?.clamp(1, i64::from(u32::MAX))).unwrap_or(DEFAULT_MAX);
        let wanted = canonical(cx.text("match")?).to_string();
        let key = match input.attr(&wanted).map(|a| a.ty()) {
            _ if wanted.is_empty() => Key::Index,
            Some(AttrType::U32 | AttrType::I32) => Key::Attr(wanted),
            Some(ty) => {
                cx.warn(format!(
                    "`{wanted}` holds {} numbers, and trails match by whole numbers (uint or int), so they follow the points by their index",
                    ty.wgsl()
                ));
                Key::Index
            }
            None => Key::Index,
        };
        let attrs: Vec<String> = cx
            .text("attributes")?
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| !w.is_empty())
            .map(|w| canonical(w).to_string())
            .collect();

        if cx.bool("reset")? {
            self.before = None;
            self.steps = 0;
            self.last = Some(now.tick);
            let set = Settings { len, max, key, shift: true, attrs };
            let out = self.trailer.step(gpu, input, None, &set, Some(wake(cx)))?;
            self.speak(cx, input, &set);
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]));
        }
        if dt == 0.0
            && self.before.is_some()
            && !cx.params_changed()
            && !cx.inputs_changed()
            && !cx.first_cook()
        {
            // The show stands still: so do the trails.
            return Ok(Cooked::Unchanged);
        }
        // Time moves: a step, which keeps a place every `every` of them. Time that stands
        // (a param or the points changed meanwhile) moves only the heads.
        let steps = if dt > 0.0 { self.steps + 1 } else { self.steps };
        let shift = dt > 0.0 && (steps - 1).is_multiple_of(every);
        let set = Settings { len, max, key, shift, attrs };
        let out = self.trailer.step(gpu, input, self.before.as_ref(), &set, Some(wake(cx)))?;
        self.speak(cx, input, &set);
        self.steps = steps;
        self.last = Some(now.tick);
        self.before = Some(out.clone());
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
