//! `tex.timemachine`: keeps the last frames of its input and shows, per pixel, one from as far
//! back as a second picture (the map) says: black is now, white is `length` frames ago. The
//! frames live in a [`Stack`] on the GPU, which can be kept smaller (resolution, 8 bits, every
//! n-th frame) where memory runs out before the wanted length.

use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::stack::{Look, MAX_FRAMES, Precision};
use penumbra_gpu::{Stack, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, waiting};

const KIND: &str = "tex.timemachine";
const MAX_STEP: i64 = 16;
const GIB: f64 = (1u64 << 30) as f64;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let longest = f64::from(MAX_FRAMES) * MAX_STEP as f64;
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Per pixel, a frame from the past: the map says how far back (black now, white oldest)",
        )
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
        .param(
            ParamSpec::float("budget", 4.0)
                .clamp(0.05, 64.0)
                .range(0.5, 20.0)
                .more()
                .doc("Most GPU memory the kept frames take, in GiB; past it fewer are kept, with a warning"),
        )
        .input("in", DataKind::Tex)
        .input("map", DataKind::Tex)
        .output("out", DataKind::Tex),
        || Box::new(TimeMachine::default()) as Box<dyn Operator>,
    ))
}

pub fn warm_up(gpu: &penumbra_gpu::Gpu) -> Result<(), Error> {
    gpu.stack_pipelines().map(|_| ())
}

#[derive(Default)]
struct TimeMachine {
    stack: Option<Stack>,
    ring: TexRing,
    /// The input frame taken last; the same frame again is not taken twice.
    input: Option<Data>,
    /// New input frames seen while recording, for `step`.
    seen: u64,
}

impl Operator for TimeMachine {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let data = cx.input(0).ok_or_else(|| waiting(cx))?;
        let src = data.get::<Tex>().ok_or_else(|| CookError::failed("not a texture"))?;
        let (w, h) = src.size();
        let scale = cx.float("scale")?.clamp(0.05, 1.0);
        let side = |s: u32| ((f64::from(s) * scale).round() as u32).max(1);
        let (sw, sh) = (side(w), side(h));
        let precision = if cx.text("precision")? == "8bit" { Precision::Byte } else { Precision::Half };
        let step = cx.int("step")?.clamp(1, MAX_STEP) as u64;
        let length = u64::try_from(cx.int("length")?).unwrap_or(1).max(1);

        // Stored frames for `length` input frames back, plus the newest.
        let wanted = length.div_ceil(step) + 1;
        let per = Stack::bytes_for(sw, sh, 1, precision).max(1);
        let budget = (cx.float("budget")?.clamp(0.05, 64.0) * GIB) as u64;
        let fits = (budget / per).max(1);
        let frames = wanted.min(fits).min(u64::from(MAX_FRAMES));
        if frames < wanted {
            let gib = |n: u64| (n * per) as f64 / GIB;
            let why = if fits < wanted {
                format!("{:.1} GiB, more than the budget", gib(wanted))
            } else {
                format!("more than the {MAX_FRAMES} a stack holds")
            };
            cx.warn(format!(
                "{wanted} frames of {sw}x{sh} need {why}; keeping {frames} ({:.1} GiB), so white reaches {} \
                 frames back; lower scale, 8bit or a larger step keep more",
                gib(frames),
                (frames - 1) * step
            ));
        }
        let frames = u32::try_from(frames).unwrap_or(MAX_FRAMES);

        let fits_shape =
            |s: &Stack| s.size() == (sw, sh) && s.frames() == frames && s.precision() == precision;
        if !self.stack.as_ref().is_some_and(fits_shape) {
            // The old frames go first, so their memory is free for the new ones.
            self.stack = None;
            self.stack = Some(Stack::new(gpu, sw, sh, frames, precision)?);
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

        let look = Look {
            reach: (length as f64 / step as f64).min(f64::from(frames - 1)) as f32,
            offset: (cx.float("offset")? / step as f64) as f32,
            blend: cx.bool("blend")?,
        };
        let target = self.ring.next(gpu, w, h, KIND)?;
        stack.read(gpu, cx.input_as::<Tex>(1), &target, look)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}
