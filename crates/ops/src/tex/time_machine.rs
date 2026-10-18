//! `tex.time_machine`: keeps the last frames of its input and shows, per pixel, one from as far
//! back as a second picture (the map) says: black is now, white is `length` frames ago. The
//! frames live in a [`Stack`] on the GPU, which can be kept smaller (resolution, 8 bits, every
//! n-th frame) where memory runs out before the wanted length.
//!
//! How many frames are kept is decided once per shape (input size, `scale`, `precision`,
//! `length`, `step`, `budget`) and not again until one of those changes: the wanted number,
//! cut to the budget, to what the GPU has free and to what one stack holds ([`plan`]). A
//! refused allocation is not retried on every cook either; asking for gigabytes the GPU does
//! not have, frame after frame, is what drives a driver into suspending the device.

use std::time::Duration;

use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::stack::{Look, MAX_FRAMES, Precision};
use penumbra_gpu::{Gpu, Stack, Tex, TexRing, Vram};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, waiting};

const KIND: &str = "tex.time_machine";
const MAX_STEP: i64 = 16;
const GIB: f64 = (1u64 << 30) as f64;
/// Share of the memory the GPU grants penumbra that the automatic budget takes.
const AUTO_SHARE: u64 = 2;
/// The automatic budget where the GPU does not say what it grants (not DX12).
const AUTO_FALLBACK: u64 = 2 << 30;
/// GPU memory left for everything else when a stack is sized by what is free: a tenth of the
/// budget, at least this much.
const MIN_RESERVE: u64 = 512 << 20;
/// How long a new stack waits for the old one's memory to come back.
const SETTLE: Duration = Duration::from_secs(2);

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let longest = f64::from(MAX_FRAMES) * MAX_STEP as f64;
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Per pixel, a frame from the past: the map says how far back (black now, white oldest)",
        )
        .stateful()
        .param(
            ParamSpec::int("length", 120)
                .clamp(1.0, longest)
                .range(1.0, 1800.0)
                .curve(Curve::Log)
                .unit(Unit::Frames)
                .doc("How far back white reaches, in input frames (a camera at 30 fps: 600 are 20 s)"),
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .clamp(0.0, longest)
                .range(0.0, 600.0)
                .unit(Unit::Frames)
                .doc("Frames back added everywhere; without a map, the delay of the whole picture"),
        )
        .param(ParamSpec::bool("blend", true).doc(
            "Mixes the two frames around a map value between them; off takes the nearer one (sharper, \
                 but a smooth map shows steps)",
        ))
        .param(
            ParamSpec::float("scale", 1.0).clamp(0.05, 1.0).range(0.1, 1.0).doc(
                "Resolution of the kept frames relative to the input; 0.5 needs a quarter of the memory",
            ),
        )
        .param(ParamSpec::choice("precision", &["16bit", "8bit"]).doc(
            "How the kept frames are stored: 16bit as they come, 8bit in half the memory with \
                 values above 1 clipped",
        ))
        .param(ParamSpec::int("step", 1).clamp(1.0, MAX_STEP as f64).range(1.0, 8.0).doc(
            "Keeps only every n-th input frame, for n times the length in the same memory; \
                 between kept frames the picture is blended, which smears fast motion",
        ))
        .param(ParamSpec::bool("record", true).doc("Take new frames; off holds what is kept"))
        .param(ParamSpec::float("budget", 0.0).clamp(0.0, 64.0).range(0.0, 20.0).more().doc(
            "Most GPU memory the kept frames take, in GiB; 0: automatic, half of what the GPU \
                 grants penumbra. Never more than the GPU has free; past either, fewer frames are \
                 kept, with a warning",
        ))
        .input("in", DataKind::Tex)
        .input("map", DataKind::Tex)
        .output("out", DataKind::Tex),
        || Box::new(TimeMachine::default()) as Box<dyn Operator>,
    ))
}

pub fn warm_up(gpu: &penumbra_gpu::Gpu) -> Result<(), Error> {
    gpu.stack_pipelines().map(|_| ())
}

/// What cut the number of kept frames below the wanted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Limit {
    /// Nothing: all wanted frames fit.
    Wanted,
    /// The `budget` param.
    Budget,
    /// The automatic budget (`budget` 0).
    Auto,
    /// What the GPU has free.
    Free,
    /// What one stack holds at this frame size.
    Capacity,
}

/// How many frames to keep and why not more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    frames: u64,
    limit: Limit,
}

/// The frames to keep: `wanted`, cut to `budget` bytes (`auto` says whose), to `free` bytes
/// where known, and to `capacity` frames; at least one. Frames are `per` bytes each.
fn plan(wanted: u64, per: u64, budget: u64, auto: bool, free: Option<u64>, capacity: u64) -> Plan {
    let per = per.max(1);
    let fits = |bytes: u64| (bytes / per).max(1);
    let budget_limit = if auto { Limit::Auto } else { Limit::Budget };
    let mut best = Plan { frames: wanted.max(1), limit: Limit::Wanted };
    let candidates = [
        Some((fits(budget), budget_limit)),
        free.map(|f| (fits(f), Limit::Free)),
        Some((capacity.max(1), Limit::Capacity)),
    ];
    for (frames, limit) in candidates.into_iter().flatten() {
        if frames < best.frames {
            best = Plan { frames, limit };
        }
    }
    best
}

/// The shape a stack was sized for. While it stays the same, neither the size nor a refused
/// allocation is looked at again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    size: (u32, u32),
    precision: Precision,
    wanted: u64,
    budget: f64,
}

#[derive(Default)]
struct TimeMachine {
    stack: Option<Stack>,
    shape: Option<Shape>,
    /// The warning for the current shape, repeated on every cook.
    warning: Option<String>,
    /// Why no stack could be made for the current shape.
    failed: Option<String>,
    ring: TexRing,
    /// The input frame taken last; the same frame again is not taken twice.
    input: Option<Data>,
    /// New input frames seen while recording, for `step`.
    seen: u64,
}

impl TimeMachine {
    /// Drops the old stack and makes one for `shape`, noting a warning or the failure.
    fn reshape(&mut self, gpu: &Gpu, shape: Shape, step: u64) {
        // The old frames go first, so their memory is free for the new ones.
        if self.stack.take().is_some() {
            gpu.settle(SETTLE);
        }
        self.shape = Some(shape);
        self.warning = None;
        self.failed = None;
        let (sw, sh) = shape.size;
        let per = Stack::bytes_for(sw, sh, 1, shape.precision).max(1);
        let capacity = match Stack::capacity(gpu, sw, sh, shape.precision) {
            Ok(c) => u64::from(c),
            Err(e) => {
                self.failed = Some(e.0);
                return;
            }
        };
        let vram = gpu.vram();
        let auto = shape.budget <= 0.0;
        let budget = match (auto, vram) {
            (true, Some(v)) => v.budget / AUTO_SHARE,
            (true, None) => AUTO_FALLBACK,
            (false, _) => (shape.budget * GIB) as u64,
        };
        let free = vram.map(|v| v.creatable(reserve(&v)));
        let plan = plan(shape.wanted, per, budget, auto, free, capacity);
        let frames = u32::try_from(plan.frames).unwrap_or(MAX_FRAMES);
        let again = "tried again when a param or the input size changes";
        match Stack::new(gpu, sw, sh, frames, shape.precision) {
            Ok(stack) => {
                let kept = u64::from(stack.frames());
                if kept < shape.wanted {
                    let gib = |n: u64| (n * per) as f64 / GIB;
                    let why = match (stack.shortfall(), plan.limit) {
                        (Some(_), _) => format!("the GPU refused more memory after {kept} frames ({again})"),
                        (None, Limit::Budget) => format!("the budget is {:.1} GiB", budget as f64 / GIB),
                        (None, Limit::Auto) => match vram {
                            Some(v) => format!(
                                "the automatic budget is {:.1} GiB, half of the {:.1} GiB the GPU grants",
                                budget as f64 / GIB,
                                v.budget as f64 / GIB
                            ),
                            None => format!("the automatic budget is {:.1} GiB", budget as f64 / GIB),
                        },
                        (None, Limit::Free) => {
                            format!("the GPU has {:.1} GiB free for it", free.unwrap_or(0) as f64 / GIB)
                        }
                        (None, Limit::Capacity | Limit::Wanted) => {
                            format!("one stack holds at most {capacity} frames of this size")
                        }
                    };
                    let bits = if shape.precision == Precision::Byte { 8 } else { 16 };
                    self.warning = Some(format!(
                        "{} frames of {sw}x{sh} at {bits} bit want {:.1} GiB; keeping {kept} ({:.1} GiB) \
                         because {why}, so white reaches {} frames back; lower scale, 8bit or a larger \
                         step keep more",
                        shape.wanted,
                        gib(shape.wanted),
                        gib(kept),
                        (kept - 1) * step
                    ));
                }
                if let Some(s) = stack.shortfall() {
                    log::warn!("{KIND}: {s}");
                }
                self.stack = Some(stack);
            }
            Err(e) => {
                log::warn!("{KIND}: {e}");
                self.failed = Some(format!("{e} ({again})"));
            }
        }
    }
}

/// GPU memory left to the rest of the app when sizing by what is free.
fn reserve(v: &Vram) -> u64 {
    (v.budget / 10).max(MIN_RESERVE)
}

impl Operator for TimeMachine {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let data = cx.input(0).ok_or_else(|| waiting(cx))?;
        let src = data.get::<Tex>().ok_or_else(|| CookError::failed("not a texture"))?;
        let (w, h) = src.size();
        let scale = cx.float("scale")?.clamp(0.05, 1.0);
        let side = |s: u32| ((f64::from(s) * scale).round() as u32).max(1);
        let precision = if cx.text("precision")? == "8bit" { Precision::Byte } else { Precision::Half };
        let step = cx.int("step")?.clamp(1, MAX_STEP) as u64;
        let length = u64::try_from(cx.int("length")?).unwrap_or(1).max(1);
        let budget = cx.float("budget")?.clamp(0.0, 64.0);

        // Stored frames for `length` input frames back, plus the newest.
        let wanted = length.div_ceil(step) + 1;
        let shape = Shape { size: (side(w), side(h)), precision, wanted, budget };
        if self.shape != Some(shape) {
            self.reshape(gpu, shape, step);
        }
        if let Some(w) = &self.warning {
            cx.warn(w.clone());
        }
        if let Some(e) = &self.failed {
            return Err(CookError::failed(e.clone()));
        }
        let Some(stack) = self.stack.as_mut() else { return Err(waiting(cx)) };

        if cx.bool("record")? {
            let new = !self.input.as_ref().is_some_and(|d| d.same(data));
            if new || stack.count() == 0 {
                if new {
                    self.seen += 1;
                }
                if stack.count() == 0 || (self.seen - 1).is_multiple_of(step) {
                    stack.push(gpu, src)?;
                }
                self.input = Some(data.clone());
            }
        }

        let frames = stack.frames();
        let look = Look {
            reach: (length as f64 / step as f64).min(f64::from(frames.saturating_sub(1))) as f32,
            offset: (cx.float("offset")? / step as f64) as f32,
            blend: cx.bool("blend")?,
        };
        let target = self.ring.next(gpu, w, h, KIND)?;
        stack.read(gpu, cx.input_as::<Tex>(1), &target, look)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;

    #[test]
    fn the_plan_takes_the_tightest_limit() {
        let hd = 16 * MB;
        // The user's case: 801 frames of 1080p want 12.5 GiB.
        assert_eq!(plan(801, hd, 4 * GB, false, None, 512), Plan { frames: 256, limit: Limit::Budget });
        assert_eq!(
            plan(801, hd, 11 * GB, true, Some(20 * GB), 512),
            Plan { frames: 512, limit: Limit::Capacity }
        );
        assert_eq!(plan(801, hd, 11 * GB, true, Some(2 * GB), 512), Plan { frames: 128, limit: Limit::Free });
        assert_eq!(plan(801, hd, 2 * GB, true, Some(20 * GB), 512), Plan { frames: 128, limit: Limit::Auto });
        assert_eq!(
            plan(121, hd, 4 * GB, false, Some(20 * GB), 512),
            Plan { frames: 121, limit: Limit::Wanted }
        );
        // Never zero, even with nothing free or a frame larger than the budget.
        assert_eq!(plan(10, hd, 0, false, Some(0), 512).frames, 1);
        assert_eq!(plan(0, 0, 0, false, None, 0).frames, 1);
    }
}
