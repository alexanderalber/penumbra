//! Channels to a texture and back: `tex.from_chan` and `chan.from_tex`.
//!
//! `tex.from_chan` uploads a [`Chans`] as a picture: one row per channel (samples across), or up
//! to four channels as the red, green, blue and alpha of one row. With many samples per channel
//! (an audio spectrum) that is a texture a shader can look values up in. A `chan.trail` that is
//! still filling up gives a picture as wide as a full one, black where it has no samples yet,
//! so the texture keeps its size. Its default layout,
//! `auto`, takes channels named exactly `r`, `g`, `b` (and `a`), as `chan.from_tex` names them,
//! as a colour, so a mean colour read at 1 x 1 comes back as a colour swatch.
//!
//! `chan.from_tex` reduces its input to `columns` x `rows` areas on the GPU, reads that small
//! picture back and hands it out as channels: one per row and colour component, a sample per
//! column. Each value is the mean, sum, minimum or maximum over the area's pixels, or the share
//! of them above a threshold. At 1 x 1 the mean is the mean colour of the picture, ready to be
//! bound to a parameter. The reduction reads whole texels with `textureLoad` and adds them up in
//! 32-bit floats where the GPU renders those: every pixel counts once, so the sum of a picture
//! of 0 and 1 is an exact count up to 2^24 (16.7 million) pixels. Without 32-bit float targets
//! one step writes half floats (exact for sums of 0 and 1, about three digits otherwise) and the
//! rest is added up on the CPU in 64-bit floats.
//! The readback waits up to [`WAIT`] within the cook, so with a quick GPU the channels belong to
//! the frame that just arrived; when it takes longer the node cooks again once the copy is
//! there, and a frame arriving in between is skipped rather than queued.
//! While the clock waits for every frame (stepping, recording), the cook waits for the copy
//! instead, so every frame's channels belong to that frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use half::slice::HalfFloatSliceExt;
use penumbra_core::chan::{MAX_CHANNELS, MAX_SAMPLES};
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Value};
use penumbra_gpu::{Gpu, PassInput, PendingRead, ReadPool, ReadState, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, waiting};
use crate::on_gpu;

pub const FROM_CHAN: &str = "tex.from_chan";
pub const FROM_TEX: &str = "chan.from_tex";
const REDUCE: &str = include_str!("reduce.wgsl");
/// How long a cook waits for the readback it just started.
const WAIT: Duration = Duration::from_millis(4);
/// A readback older than this is given up.
const STALE: Duration = Duration::from_secs(2);

/// How long a cook waits for its readback: [`WAIT`] in a live show, where a late result shows a
/// frame later rather than holding up the frame; up to [`STALE`] while the clock waits for every
/// frame, so the result belongs to the frame that asked for it on every run.
fn wait_for(cx: &CookContext<'_>) -> Duration {
    if cx.exact_frames() { STALE } else { WAIT }
}
/// Most a reduction step reduces by along each axis: a texel of its output reads at most
/// 16 x 16 texels.
const REDUCE_STEP: u32 = 16;
/// Largest half float; larger channel values are clipped to it rather than becoming infinite.
const HALF_MAX: f64 = 65504.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(FROM_CHAN, "Turns channels into a picture: a row per channel, a pixel per sample. Numbers for shaders and geo.tex_instance")
            .param(
                ParamSpec::choice("layout", &["auto", "rows", "rgba"])
                    .doc("How the channels become pixels")
                    .choice_doc(
                        "auto",
                        "rgba when the channels are exactly r, g, b (and a), as chan.from_tex names them, else rows",
                    )
                    .choice_doc("rows", "one grey row per channel, as many pixels wide as there are samples")
                    .choice_doc(
                        "rgba",
                        "the first four channels as red, green, blue and alpha of one row (alpha 1 where missing)",
                    ),
            )
            .param(
                ParamSpec::float("low", 0.0)
                    .range(-1.0, 1.0)
                    .doc("Channel value that becomes black (0); lower values go below 0"),
            )
            .param(
                ParamSpec::float("high", 1.0)
                    .range(0.0, 2.0)
                    .doc("Channel value that becomes white (1); higher values go above 1"),
            )
            .input("in", DataKind::Chan)
            .output("out", DataKind::Tex),
        || Box::new(FromChan::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            FROM_TEX,
            "Turns a picture into numbers: its average colour, the sum, smallest or largest value, or how much of it is lit, as channels to drive things with. Also per area",
        )
        .param(
            ParamSpec::choice("measure", &["mean", "sum", "min", "max", "coverage"])
                .doc("What each channel says about the pixels of its area")
                .choice_doc("mean", "the average value")
                .choice_doc(
                    "sum",
                    "all values added up: in a picture of 0 and 1, the number of pixels that are 1, exact up to 16 million",
                )
                .choice_doc("min", "the smallest value")
                .choice_doc("max", "the largest value")
                .choice_doc("coverage", "the share of pixels above `threshold`: 0 for none, 1 for all"),
        )
        .param(
            ParamSpec::float("threshold", 0.5)
                .range(0.0, 1.0)
                .doc("A pixel counts for coverage when its value is above this")
                .applies_when("measure", &["coverage"]),
        )
        .param(
            ParamSpec::int("columns", 1)
                .clamp(1.0, 4096.0)
                .range(1.0, 256.0)
                .doc("Samples of each channel. The picture is cut into this many columns, and each channel gets one sample per column, left to right. A binding reads the last sample: the right-hand column. For a channel per area, use rows"),
        )
        .param(
            ParamSpec::int("rows", 1)
                .clamp(1.0, 1024.0)
                .range(1.0, 64.0)
                .doc("Separate channels. The picture is cut into this many rows, and each row gives its own channels, numbered from the top: r0, r1, ... (g0, lum0 likewise). To read several pixels as channels of their own, stack them in one column and set rows to their number"),
        )
        .param(
            ParamSpec::choice("components", &["rgba", "rgb", "red", "luminance"])
                .doc("Which channels a row gives; values are linear and premultiplied, as the picture holds them")
                .choice_doc("rgba", "r, g, b and a")
                .choice_doc("rgb", "r, g and b")
                .choice_doc("red", "r only")
                .choice_doc("luminance", "lum, the brightness as the eye weighs it (Rec. 709)"),
        )
        .input("in", DataKind::Tex)
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "Usually the values of the frame that just cooked. When the GPU is slow they come a frame \
             or two later; while stepping or recording, always at once. A binding from them into the \
             loop they measure goes through a chan.feedback",
        ),
        || Box::new(FromTex::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// The params of `reduce.wgsl`, set by the node for each step.
fn reduce_params() -> [ParamSpec; 7] {
    [
        ParamSpec::int("cells_x", 1),
        ParamSpec::int("cells_y", 1),
        ParamSpec::int("mode", 0),
        ParamSpec::int("first", 0),
        ParamSpec::int("luminance", 0),
        ParamSpec::int("above", 0),
        ParamSpec::float("threshold", 0.5),
    ]
}

/// The reduction pass, rendering into 32-bit floats when `exact` (the GPU must support it,
/// [`Gpu::float_targets`]), else into half floats.
fn reduce_pass(gpu: &Gpu, exact: bool) -> Result<Arc<penumbra_gpu::Pass>, Error> {
    let format = if exact { penumbra_gpu::FLOAT_FORMAT } else { penumbra_gpu::FORMAT };
    gpu.cached_pass_in(FROM_TEX, REDUCE, &reduce_params(), format)
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    reduce_pass(gpu, gpu.float_targets()).map(|_| ())
}

#[derive(Default)]
struct FromChan {
    ring: TexRing,
}

impl Operator for FromChan {
    fn gpu_bytes(&self) -> u64 {
        self.ring.bytes()
    }

    fn trim(&mut self) {
        self.ring.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let chans = cx.input_as::<Chans>(0).ok_or_else(|| waiting(cx))?;
        let layout = match cx.text("layout")? {
            "rgba" => Layout::Rgba,
            "rows" => Layout::Rows,
            _ => auto_layout(chans),
        };
        let (low, high) = (cx.float("low")?, cx.float("high")?);
        let max = gpu.max_texture_size().max(1) as usize;
        let picture = picture(chans, layout, low, high, max);
        if let Some(w) = &picture.warning {
            cx.warn(w.clone());
        }
        let target = self.ring.next(gpu, picture.width, picture.height, FROM_CHAN)?;
        let mut bytes = picture.bytes;
        gpu.write_rgba16f(&target, &mut bytes)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Layout {
    Rows,
    Rgba,
}

/// What `auto` picks: a colour for channels named exactly `r`, `g`, `b` (and `a`) in that order,
/// rows for anything else.
fn auto_layout(chans: &Chans) -> Layout {
    let names: Vec<&str> = chans.channels().iter().map(|c| c.name.as_str()).collect();
    match names.as_slice() {
        ["r", "g", "b"] | ["r", "g", "b", "a"] => Layout::Rgba,
        _ => Layout::Rows,
    }
}

/// Half-float RGBA rows ready for upload, and why they show less than the input.
struct Picture {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    warning: Option<String>,
}

/// The channels as a picture at most `max` pixels on a side; the newest samples and the first
/// channels are kept where they do not fit. The picture is as wide as the input's window, so a
/// `chan.trail` that is still growing keeps its size and fills in from the right, black before.
fn picture(chans: &Chans, layout: Layout, low: f64, high: f64, max: usize) -> Picture {
    let channels = chans.channels();
    let len = chans.len();
    let mut warning = Vec::new();
    if channels.is_empty() {
        warning.push("the input has no channels".to_string());
    }
    let width = chans.window().min(max);
    if width < len {
        warning.push(format!("{len} samples, the newest {width} shown (the widest texture the GPU allows)"));
    }
    let rows = match layout {
        Layout::Rows => {
            if channels.len() > max {
                warning.push(format!("{} channels, the first {max} shown", channels.len()));
            }
            channels.len().clamp(1, max)
        }
        Layout::Rgba => {
            if channels.len() > 4 {
                warning.push(format!("{} channels, the first four shown", channels.len()));
            }
            1
        }
    };
    let span = high - low;
    let map = |v: f64| -> f32 {
        let t = if span.abs() > f64::EPSILON { (v - low) / span } else { 0.0 };
        t.clamp(-HALF_MAX, HALF_MAX) as f32
    };
    // Pixel `x` is sample `x + len - width` of the channel: the newest sample at the right edge,
    // nothing left of the oldest.
    let sample = |c: usize, x: usize| {
        let i = (x + len).checked_sub(width)?;
        channels.get(c).and_then(|ch| ch.samples.get(i)).map(|v| map(*v))
    };
    let mut floats = Vec::with_capacity(width * rows * 4);
    for y in 0..rows {
        for x in 0..width {
            match layout {
                Layout::Rows => {
                    let v = sample(y, x).unwrap_or(0.0);
                    floats.extend([v, v, v, 1.0]);
                }
                Layout::Rgba => floats.extend([
                    sample(0, x).unwrap_or(0.0),
                    sample(1, x).unwrap_or(0.0),
                    sample(2, x).unwrap_or(0.0),
                    sample(3, x).unwrap_or(1.0),
                ]),
            }
        }
    }
    let mut halves = vec![f16::ZERO; floats.len()];
    halves.convert_from_f32_slice(&floats);
    Picture {
        width: width as u32,
        height: rows as u32,
        bytes: halves.iter().flat_map(|h| h.to_le_bytes()).collect(),
        warning: (!warning.is_empty()).then(|| warning.join("; ")),
    }
}

#[derive(Default)]
struct FromTex {
    /// The reduction pass, and whether it renders into 32-bit floats.
    pass: Option<(bool, Arc<penumbra_gpu::Pass>)>,
    /// The reduction steps' targets, largest first; the last is read back.
    levels: Vec<TexRing>,
    pool: ReadPool,
    /// The readback in flight, when it started and what it was started for.
    pending: Option<(PendingRead, Instant, Request)>,
    out: Option<Data>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Components {
    Rgba,
    Rgb,
    Red,
    Luminance,
}

impl Components {
    fn parse(s: &str) -> Self {
        match s {
            "rgb" => Components::Rgb,
            "red" => Components::Red,
            "luminance" => Components::Luminance,
            _ => Components::Rgba,
        }
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            Components::Rgba => &["r", "g", "b", "a"],
            Components::Rgb => &["r", "g", "b"],
            Components::Red => &["r"],
            Components::Luminance => &["lum"],
        }
    }
}

/// What each channel says about the pixels of its area (`measure`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Measure {
    Mean,
    Sum,
    Min,
    Max,
    Coverage,
}

impl Measure {
    fn parse(s: &str) -> Self {
        match s {
            "sum" => Measure::Sum,
            "min" => Measure::Min,
            "max" => Measure::Max,
            "coverage" => Measure::Coverage,
            _ => Measure::Mean,
        }
    }

    /// The reduction's `mode`: 0 adds up, 1 keeps the smallest, 2 the largest.
    fn mode(self) -> i64 {
        match self {
            Measure::Min => 1,
            Measure::Max => 2,
            _ => 0,
        }
    }
}

/// What a readback was started for: enough to turn the small picture into channels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    components: Components,
    measure: Measure,
    /// Columns and rows.
    cells: (u32, u32),
    /// The input's size.
    src: (u32, u32),
    /// Texels per area in the picture read back (1 x 1 when every step ran on the GPU).
    per: (u32, u32),
}

impl Operator for FromTex {
    fn gpu_bytes(&self) -> u64 {
        self.levels.iter().map(TexRing::bytes).sum::<u64>() + self.pool.bytes()
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let src = cx.input_as::<Tex>(0).ok_or_else(|| waiting(cx))?;
        // A readback that never arrives (a lost device) must not stop the node for good.
        if self.pending.as_ref().is_some_and(|(_, started, _)| started.elapsed() > STALE) {
            self.pending = None;
        }
        let wanted = cx.inputs_changed() || cx.params_changed() || cx.first_cook() || self.out.is_none();
        if wanted && self.pending.is_none() {
            self.start(cx, gpu, src)?;
        }
        let ready = match &self.pending {
            Some((read, _, _)) => crate::readback::wait(cx, gpu, read, wait_for(cx)),
            None => ReadState::Waiting,
        };
        match ready {
            ReadState::Ready => {
                if let Some((read, _, request)) = self.pending.take() {
                    let chans = chans_of(&read, &request, cx.rate().as_f64());
                    self.pool = read.recycle();
                    let out = Data::new(DataKind::Chan, chans.map_err(CookError::failed)?);
                    self.out = Some(out.clone());
                    return Ok(Cooked::Outputs(vec![out]));
                }
            }
            ReadState::Failed => {
                self.pending = None;
                return Err(CookError::failed("the GPU could not read the picture back"));
            }
            ReadState::Waiting => {}
        }
        match &self.out {
            // Shown at the next frame or once the readback arrives.
            Some(_) => Ok(Cooked::Unchanged),
            None => Err(CookError::Pending),
        }
    }

    fn job_status(&self) -> Option<penumbra_graph::JobStatus> {
        (self.out.is_none() && self.pending.is_some())
            .then_some(penumbra_graph::JobStatus::Running { progress: None })
    }
}

impl FromTex {
    /// The reduction pass, rendering into 32-bit floats when `exact`.
    fn pass(&mut self, gpu: &Gpu, exact: bool) -> Result<Arc<penumbra_gpu::Pass>, Error> {
        if let Some((e, p)) = &self.pass
            && *e == exact
        {
            return Ok(p.clone());
        }
        let p = reduce_pass(gpu, exact)?;
        self.pass = Some((exact, p.clone()));
        Ok(p)
    }

    /// Reduces the input on the GPU and starts reading the result back; the node is woken when
    /// it arrives. With 32-bit float targets every step runs on the GPU and one texel per area
    /// comes back. Without them the first step writes half floats and the rest is done here.
    fn start(&mut self, cx: &CookContext<'_>, gpu: &Gpu, src: &Tex) -> Result<(), CookError> {
        let side =
            |name: &str| -> Result<u32, CookError> { Ok(u32::try_from(cx.int(name)?).unwrap_or(1).max(1)) };
        let (columns, rows) = (side("columns")?, side("rows")?);
        let components = Components::parse(cx.text("components")?);
        let measure = Measure::parse(cx.text("measure")?);
        let threshold = cx.float("threshold")?;
        let per_row = components.names().len();
        let channels = rows as usize * per_row;
        if channels > MAX_CHANNELS || channels * columns as usize > MAX_SAMPLES {
            return Err(CookError::failed(format!(
                "{rows} rows of {columns} columns give {channels} channels of {columns} samples, more than \
                 fit ({MAX_CHANNELS} channels, {MAX_SAMPLES} samples)"
            )));
        }
        let exact = gpu.float_targets();
        let format = if exact { penumbra_gpu::FLOAT_FORMAT } else { penumbra_gpu::FORMAT };
        let mut plan = reduce_plan(src.size(), (columns, rows));
        if !exact {
            plan.truncate(1);
        }
        self.levels.resize_with(plan.len(), TexRing::new);
        let pass = self.pass(gpu, exact)?;
        let mut from = src.clone();
        let mut per = (1, 1);
        for (i, (ring, step)) in self.levels.iter_mut().zip(&plan).enumerate() {
            per = *step;
            let target = ring.next_in(gpu, columns * per.0, rows * per.1, format, FROM_TEX)?;
            let param = |name: &str| -> Option<Value> {
                Some(match name {
                    "cells_x" => Value::Int(i64::from(columns)),
                    "cells_y" => Value::Int(i64::from(rows)),
                    "mode" => Value::Int(measure.mode()),
                    "first" => Value::Int(i64::from(i == 0)),
                    "luminance" => Value::Int(i64::from(components == Components::Luminance)),
                    "above" => Value::Int(i64::from(measure == Measure::Coverage)),
                    "threshold" => Value::Float(threshold),
                    _ => return None,
                })
            };
            pass.render(
                gpu,
                &target,
                &PassInput { inputs: &[Some(&from)], seconds: 0.0, frame: 0, param: &param },
            )?;
            from = target;
        }
        let waker = cx.waker();
        let ready: Option<Arc<dyn Fn() + Send + Sync>> = Some(Arc::new(move || waker.wake()));
        let read = if exact {
            gpu.read_async_exact_notify(&[&from], &mut self.pool, ready)?
        } else {
            gpu.read_async_notify(&[&from], &mut self.pool, ready)?
        };
        let request = Request { components, measure, cells: (columns, rows), src: src.size(), per };
        self.pending = Some((read, Instant::now(), request));
        Ok(())
    }
}

/// Texels per area in the output of each reduction step, first step first. Each step reduces
/// by at most [`REDUCE_STEP`] along each axis, and the last leaves one texel per area.
fn reduce_plan(src: (u32, u32), cells: (u32, u32)) -> Vec<(u32, u32)> {
    let widest = |s: u32, c: u32| s.div_ceil(c.max(1)).max(1);
    let mut span = (widest(src.0, cells.0), widest(src.1, cells.1));
    let mut plan = Vec::new();
    loop {
        let per = (span.0.div_ceil(REDUCE_STEP), span.1.div_ceil(REDUCE_STEP));
        plan.push(per);
        if per == (1, 1) || plan.len() > 16 {
            break;
        }
        span = per;
    }
    plan
}

/// How many source texels area `cell` of `cells` covers along an axis of `src` texels, as the
/// reduction pass counts them: at least one.
fn area_len(cell: u32, src: u32, cells: u32) -> u64 {
    let (cell, src, cells) = (u64::from(cell), u64::from(src.max(1)), u64::from(cells.max(1)));
    let a = (cell * src / cells).min(src - 1);
    let b = ((cell + 1) * src / cells).max(a + 1);
    b - a
}

/// The readback as channels: per row of areas (top first) one channel per component, a sample
/// per column. Partial results of an area are combined here in 64-bit floats.
fn chans_of(read: &PendingRead, request: &Request, rate: f64) -> Result<Chans, String> {
    let (w, h) = read.size(0).ok_or("nothing was read back")?;
    let (columns, rows) = request.cells;
    let (px, py) = request.per;
    if (w, h) != (columns * px, rows * py) {
        return Err(format!("read back {w} x {h} texels, expected {} x {}", columns * px, rows * py));
    }
    let float = read.pixel_bytes(0) == Some(16);
    let mut texels: Vec<[f32; 4]> = Vec::with_capacity(w as usize * h as usize);
    read.rows(0, |row: &[u8]| -> Result<(), String> {
        if float {
            for px in row.as_chunks::<16>().0 {
                let (f, _) = px.as_chunks::<4>();
                texels.push(std::array::from_fn(|i| f.get(i).map_or(0.0, |b| f32::from_le_bytes(*b))));
            }
        } else {
            for px in row.as_chunks::<8>().0 {
                let (f, _) = px.as_chunks::<2>();
                texels
                    .push(std::array::from_fn(|i| f.get(i).map_or(0.0, |b| f16::from_le_bytes(*b).to_f32())));
            }
        }
        Ok(())
    })
    .map_err(|e| match e {
        penumbra_gpu::RowsError::Gpu(e) => e.to_string(),
        penumbra_gpu::RowsError::Sink(e) => e,
    })?;
    let measure = request.measure;
    let area = |cx: u32, cy: u32| -> [f64; 4] {
        let start = match measure {
            Measure::Min => f64::INFINITY,
            Measure::Max => f64::NEG_INFINITY,
            _ => 0.0,
        };
        let mut acc = [start; 4];
        for y in cy * py..(cy + 1) * py {
            for x in cx * px..(cx + 1) * px {
                let Some(t) = texels.get(y as usize * w as usize + x as usize) else { continue };
                for (a, v) in acc.iter_mut().zip(t) {
                    let v = f64::from(*v);
                    *a = match measure {
                        Measure::Min => a.min(v),
                        Measure::Max => a.max(v),
                        _ => *a + v,
                    };
                }
            }
        }
        let count = (area_len(cx, request.src.0, columns) * area_len(cy, request.src.1, rows)) as f64;
        acc.map(|a| match measure {
            Measure::Mean | Measure::Coverage => a / count,
            _ => a,
        })
    };
    let names = request.components.names();
    let mut channels = Vec::with_capacity(rows as usize * names.len());
    for cy in 0..rows {
        let values: Vec<[f64; 4]> = (0..columns).map(|cx| area(cx, cy)).collect();
        for (i, name) in names.iter().enumerate() {
            let name = if rows == 1 { (*name).to_string() } else { format!("{name}{cy}") };
            channels.push(Channel {
                name: Name::new(&name).map_err(|e| e.to_string())?,
                samples: values.iter().map(|v| v.get(i).copied().unwrap_or(0.0)).collect(),
            });
        }
    }
    // Samples are pixels along a row, not moments.
    Chans::new(rate, channels)
        .map(|c| c.with_axis(penumbra_core::SampleAxis::Index))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chans(rows: &[(&str, &[f64])]) -> Chans {
        let channels =
            rows.iter().map(|(n, s)| Channel { name: Name::new(n).unwrap(), samples: s.to_vec() }).collect();
        Chans::new(60.0, channels).unwrap()
    }

    fn floats(p: &Picture) -> Vec<f32> {
        p.bytes.as_chunks::<2>().0.iter().map(|b| f16::from_le_bytes(*b).to_f32()).collect()
    }

    #[test]
    fn rows_put_a_channel_on_each_row() {
        let c = chans(&[("a", &[0.0, 0.5]), ("b", &[1.0, 2.0])]);
        let p = picture(&c, Layout::Rows, 0.0, 1.0, 64);
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(
            floats(&p),
            [0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 1.0]
        );
        assert!(p.warning.is_none());
    }

    #[test]
    fn a_window_not_yet_full_fills_in_from_the_right() {
        let c = chans(&[("a", &[0.5, 1.0])]).with_window(4);
        let p = picture(&c, Layout::Rows, 0.0, 1.0, 64);
        assert_eq!((p.width, p.height), (4, 1));
        assert_eq!(
            floats(&p),
            [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0]
        );
        assert!(p.warning.is_none());
        // A window wider than the widest texture keeps its newest samples.
        let p = picture(&c, Layout::Rows, 0.0, 1.0, 3);
        assert_eq!(floats(&p)[4..], [0.5, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn rgba_takes_four_channels_and_maps_the_range() {
        let c = chans(&[("r", &[2.0]), ("g", &[0.0]), ("b", &[-2.0])]);
        let p = picture(&c, Layout::Rgba, -2.0, 2.0, 64);
        assert_eq!((p.width, p.height), (1, 1));
        assert_eq!(floats(&p), [1.0, 0.5, 0.0, 1.0], "alpha 1 without a fourth channel");
        let five = chans(&[("a", &[0.0]), ("b", &[0.0]), ("c", &[0.0]), ("d", &[0.0]), ("e", &[0.0])]);
        assert!(picture(&five, Layout::Rgba, 0.0, 1.0, 64).warning.is_some());
    }

    #[test]
    fn auto_takes_r_g_b_as_a_colour_and_anything_else_as_rows() {
        let rgb = chans(&[("r", &[0.1]), ("g", &[0.2]), ("b", &[0.3])]);
        assert_eq!(auto_layout(&rgb), Layout::Rgba);
        let rgba = chans(&[("r", &[0.1]), ("g", &[0.2]), ("b", &[0.3]), ("a", &[1.0])]);
        assert_eq!(auto_layout(&rgba), Layout::Rgba);
        for other in [
            chans(&[("r", &[0.1])]),
            chans(&[("lum", &[0.1])]),
            chans(&[("r0", &[0.1]), ("g0", &[0.2]), ("b0", &[0.3])]),
            chans(&[("g", &[0.1]), ("r", &[0.2]), ("b", &[0.3])]),
        ] {
            assert_eq!(auto_layout(&other), Layout::Rows, "{other:?}");
        }
    }

    #[test]
    fn too_many_samples_keep_the_newest_and_huge_values_stay_finite() {
        let c = chans(&[("a", &[1.0, 2.0, 1e9])]);
        let p = picture(&c, Layout::Rows, 0.0, 1.0, 2);
        assert_eq!((p.width, p.height), (2, 1));
        let f = floats(&p);
        assert_eq!(f[0], 2.0);
        assert!(f[4].is_finite() && f[4] > 60000.0);
        assert!(p.warning.is_some());
        let empty = Chans::empty(60.0).unwrap();
        let p = picture(&empty, Layout::Rows, 0.0, 1.0, 64);
        assert_eq!((p.width, p.height), (1, 1));
        assert!(p.warning.is_some());
        assert_eq!(
            picture(&c, Layout::Rows, 1.0, 1.0, 64).bytes.len(),
            3 * 8,
            "an empty range gives 0, not NaN"
        );
    }

    #[test]
    fn reducing_takes_steps_of_at_most_sixteen() {
        assert_eq!(reduce_plan((1920, 1080), (1, 1)), [(120, 68), (8, 5), (1, 1)]);
        assert_eq!(reduce_plan((64, 64), (4, 4)), [(1, 1)]);
        assert_eq!(reduce_plan((8, 8), (32, 1)), [(1, 1)], "more columns than pixels is one step");
        assert_eq!(reduce_plan((4096, 1), (1, 1)), [(256, 1), (16, 1), (1, 1)]);
        assert_eq!(reduce_plan((1000, 600), (4, 2)), [(16, 19), (1, 2), (1, 1)]);
    }

    #[test]
    fn areas_cover_every_pixel_once() {
        for (src, cells) in [(1000, 7), (1920, 1), (5, 5), (17, 4)] {
            let total: u64 = (0..cells).map(|c| area_len(c, src, cells)).sum();
            assert_eq!(total, u64::from(src), "{src} pixels in {cells} areas");
        }
        // More areas than pixels: each still covers one.
        assert!((0..8).all(|c| area_len(c, 3, 8) == 1));
    }
}
