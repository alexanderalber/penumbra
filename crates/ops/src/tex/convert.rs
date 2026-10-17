//! Channels to a texture and back: `tex.from_chan` and `chan.from_tex`.
//!
//! `tex.from_chan` uploads a [`Chans`] as a picture: one row per channel (samples across), or up
//! to four channels as the red, green, blue and alpha of one row. With many samples per channel
//! (an audio spectrum) that is a texture a shader can look values up in. Its default layout,
//! `auto`, takes channels named exactly `r`, `g`, `b` (and `a`), as `chan.from_tex` names them,
//! as a colour, so a mean colour read at 1 x 1 comes back as a colour swatch.
//!
//! `chan.from_tex` averages its input down to `columns` x `rows` pixels on the GPU, reads that
//! small picture back and hands it out as channels: one per row and colour component, a sample
//! per column. At 1 x 1 it is the mean colour of the picture, ready to be bound to a parameter.
//! The readback waits up to [`WAIT`] within the cook, so with a quick GPU the channels belong to
//! the frame that just arrived; when it takes longer the node cooks again once the copy is
//! there, and a frame arriving in between is skipped rather than queued.

use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use half::slice::HalfFloatSliceExt;
use penumbra_core::chan::{MAX_CHANNELS, MAX_SAMPLES};
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec};
use penumbra_gpu::{Gpu, PassInput, PendingRead, ReadPool, ReadState, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, waiting};

pub const FROM_CHAN: &str = "tex.from_chan";
pub const FROM_TEX: &str = "chan.from_tex";
const SHRINK: &str = include_str!("shrink.wgsl");
/// How long a cook waits for the readback it just started.
const WAIT: Duration = Duration::from_millis(4);
/// A readback older than this is given up.
const STALE: Duration = Duration::from_secs(2);
/// Most a shrink pass reduces by along each axis while still averaging every texel
/// (`pn_area` takes up to 8 bilinear samples, each the mean of 2 texels across).
const SHRINK_STEP: u32 = 16;
/// Largest half float; larger channel values are clipped to it rather than becoming infinite.
const HALF_MAX: f64 = 65504.0;
/// Rec. 709 weights for `luminance`, on linear values.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(FROM_CHAN, "Channels as a picture: a row per channel, a pixel per sample; r, g, b as a colour")
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
            "A picture as channels: averaged down to a few pixels, one channel per row and colour",
        )
        .param(
            ParamSpec::int("columns", 1)
                .clamp(1.0, 4096.0)
                .range(1.0, 256.0)
                .doc("Samples per channel: the picture is averaged down to this many columns"),
        )
        .param(
            ParamSpec::int("rows", 1)
                .clamp(1.0, 1024.0)
                .range(1.0, 64.0)
                .doc("Rows averaged to; more than one numbers the channels by row from the top (r0, r1, ..)"),
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
        .output("out", DataKind::Chan),
        || Box::new(FromTex::default()) as Box<dyn Operator>,
    ))
}

fn shrink_pass(gpu: &Gpu) -> Result<Arc<penumbra_gpu::Pass>, Error> {
    gpu.cached_pass(FROM_TEX, SHRINK, &[])
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    shrink_pass(gpu).map(|_| ())
}

#[derive(Default)]
struct FromChan {
    ring: TexRing,
}

impl Operator for FromChan {
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
/// channels are kept where they do not fit.
fn picture(chans: &Chans, layout: Layout, low: f64, high: f64, max: usize) -> Picture {
    let channels = chans.channels();
    let len = chans.len();
    let mut warning = Vec::new();
    if channels.is_empty() {
        warning.push("the input has no channels".to_string());
    }
    let width = len.min(max);
    if width < len {
        warning.push(format!("{len} samples, the newest {width} shown (the widest texture)"));
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
    // Sample `x` of the picture is sample `skip + x` of the channel.
    let skip = len - width;
    let sample =
        |c: usize, x: usize| channels.get(c).and_then(|ch| ch.samples.get(skip + x)).map(|v| map(*v));
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
    /// The shrink passes' targets, largest first; the last is read back.
    levels: Vec<TexRing>,
    pool: ReadPool,
    /// The readback in flight, when it started and the components it was started for.
    pending: Option<(PendingRead, Instant, Components)>,
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

    fn values(self, [r, g, b, a]: [f32; 4]) -> [f32; 4] {
        match self {
            Components::Luminance => [LUMA[0] * r + LUMA[1] * g + LUMA[2] * b, 0.0, 0.0, 0.0],
            _ => [r, g, b, a],
        }
    }
}

impl Operator for FromTex {
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
            Some((read, _, _)) => read.wait(gpu, WAIT),
            None => ReadState::Waiting,
        };
        match ready {
            ReadState::Ready => {
                if let Some((read, _, components)) = self.pending.take() {
                    let chans = chans_of(&read, components, cx.rate().as_f64());
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
    /// Averages the input down in as few passes as keep every texel counted and starts reading
    /// the result back; the node is woken when it arrives.
    fn start(&mut self, cx: &CookContext<'_>, gpu: &Gpu, src: &Tex) -> Result<(), CookError> {
        let side =
            |name: &str| -> Result<u32, CookError> { Ok(u32::try_from(cx.int(name)?).unwrap_or(1).max(1)) };
        let (columns, rows) = (side("columns")?, side("rows")?);
        let components = Components::parse(cx.text("components")?);
        let per_row = components.names().len();
        let channels = rows as usize * per_row;
        if channels > MAX_CHANNELS || channels * columns as usize > MAX_SAMPLES {
            return Err(CookError::failed(format!(
                "{rows} rows of {columns} columns give {channels} channels of {columns} samples, more than \
                 fit ({MAX_CHANNELS} channels, {MAX_SAMPLES} samples)"
            )));
        }
        let sizes = shrink_sizes(src.size(), (columns, rows));
        self.levels.resize_with(sizes.len(), TexRing::new);
        let pass = shrink_pass(gpu)?;
        let mut from = src.clone();
        for (ring, (w, h)) in self.levels.iter_mut().zip(&sizes) {
            let target = ring.next(gpu, *w, *h, FROM_TEX)?;
            pass.render(
                gpu,
                &target,
                &PassInput { inputs: &[Some(&from)], seconds: 0.0, frame: 0, param: &|_| None },
            );
            from = target;
        }
        let waker = cx.waker();
        let read = gpu.read_async_notify(&[&from], &mut self.pool, Some(Arc::new(move || waker.wake())))?;
        self.pending = Some((read, Instant::now(), components));
        Ok(())
    }
}

/// The sizes of the shrink passes from `from` down to `to`, each at most [`SHRINK_STEP`] times
/// smaller than the one before along each axis; always at least one pass (the last is `to`).
fn shrink_sizes(from: (u32, u32), to: (u32, u32)) -> Vec<(u32, u32)> {
    let step =
        |cur: u32, target: u32| if cur > target * SHRINK_STEP { cur.div_ceil(SHRINK_STEP) } else { target };
    let mut sizes = Vec::new();
    let mut cur = from;
    loop {
        cur = (step(cur.0, to.0), step(cur.1, to.1));
        sizes.push(cur);
        if cur == to || sizes.len() > 16 {
            break;
        }
    }
    sizes
}

/// The readback as channels: per row (top first) one channel per component, a sample per
/// column.
fn chans_of(read: &PendingRead, components: Components, rate: f64) -> Result<Chans, String> {
    let (w, h) = read.size(0).ok_or("nothing was read back")?;
    let names = components.names();
    let mut channels = Vec::with_capacity(h as usize * names.len());
    let mut halves = Vec::with_capacity(w as usize * 4);
    let mut floats = vec![0f32; w as usize * 4];
    let mut y = 0usize;
    read.rows(0, |row: &[u8]| -> Result<(), String> {
        halves.clear();
        halves.extend(row.as_chunks::<2>().0.iter().map(|b| f16::from_le_bytes(*b)));
        floats.resize(halves.len(), 0.0);
        halves.convert_to_f32_slice(&mut floats);
        let pixels: Vec<[f32; 4]> = floats.as_chunks::<4>().0.iter().map(|p| components.values(*p)).collect();
        for (i, name) in names.iter().enumerate() {
            let name = if h == 1 { (*name).to_string() } else { format!("{name}{y}") };
            channels.push(Channel {
                name: Name::new(&name).map_err(|e| e.to_string())?,
                samples: pixels.iter().map(|p| f64::from(p.get(i).copied().unwrap_or(0.0))).collect(),
            });
        }
        y += 1;
        Ok(())
    })
    .map_err(|e| match e {
        penumbra_gpu::RowsError::Gpu(e) => e.to_string(),
        penumbra_gpu::RowsError::Sink(e) => e,
    })?;
    Chans::new(rate, channels).map_err(|e| e.to_string())
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
    fn shrinking_takes_steps_of_at_most_sixteen() {
        assert_eq!(shrink_sizes((1920, 1080), (1, 1)), [(120, 68), (8, 5), (1, 1)]);
        assert_eq!(shrink_sizes((64, 64), (4, 4)), [(4, 4)]);
        assert_eq!(shrink_sizes((8, 8), (32, 1)), [(32, 1)], "growing is one pass");
        assert_eq!(shrink_sizes((4096, 1), (1, 1)), [(256, 1), (16, 1), (1, 1)]);
    }
}
