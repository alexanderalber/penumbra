//! `tex.shapes`: the shape layer. Tagged rectangles, ellipses and polygons in surface coordinates,
//! stored as text in `shapes` (see [`crate::shapes`]) and drawn in the viewer, become a mask and a
//! signed distance field of the tags `tag` names, outlines of all of them in their tag colours (to
//! see them on the wall while placing them), a table of every shape, and per pixel which of those
//! shapes it belongs to (what `tex.shape_content` animates).
//!
//! With its `table` input connected, the shapes come from that table instead (any table with
//! the columns the `table` output has), so a second node can mask other tags of the same shapes.
//!
//! Traced shapes come two ways. `shapes.trace` (the trace row in its params) writes the outlines
//! of a mask or label texture into `shapes` as `poly` lines ([`crate::trace`]), to be edited by
//! hand like drawn ones. With the `trace` input connected, the node traces that texture itself,
//! as its `trace_*` params say, whenever the texture or those params change: nothing of it is
//! stored, so a project file written by a script gets its traced shapes from the connection and
//! the params alone. The texture is read back and traced off the cook thread; the shapes traced
//! last are drawn until the next ones are done.

use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::{PendingRead, ReadPool, ReadState, ShapeMode, ShapePoly, Shapes as Drawer, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Job, JobStatus, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

use super::{MAX_SIDE, gpu, side};
use crate::on_gpu;
use crate::shapes::{self, Instance, MAX_INSTANCES, Shape};
use crate::trace::{self, Channel, MAX_PIXELS, Options};

pub const SHAPES: &str = "tex.shapes";

/// The section of the params that say how the `trace` input is traced.
pub const TRACE_GROUP: &str = "trace";
/// The tag of shapes traced from the `trace` input unless `trace_tag` names another.
pub const TRACE_TAG: &str = "trace";
/// Index of the `trace` input.
pub const TRACE_INPUT: usize = 1;
/// Index of the `table` output.
pub const TABLE_OUTPUT: u32 = 3;
/// A readback older than this is given up (a lost device must not stop the node for good).
const STALE: Duration = Duration::from_secs(5);
/// A picture traced again sooner than this after the last trace started changes all the time.
const RESTLESS: Duration = Duration::from_secs(1);

fn desc() -> NodeDesc {
    NodeDesc::new(
        SHAPES,
        "Shapes drawn on the surface (rectangles, ellipses, polygons) with tags: masks and outlines to light parts of a facade or object on their own",
    ).block(penumbra_graph::Block::ScanAndShapes)
    .param(
        ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels).doc(
            "Width of the surface in pixels (the mappers' input); shape coordinates are shares of it, 0 to 1 across",
        ),
    )
    .param(
        ParamSpec::int("height", 1080)
            .clamp(1.0, MAX_SIDE)
            .curve(Curve::Log)
            .unit(Unit::Pixels)
            .doc("Height of the surface in pixels; shape coordinates are shares of it, 0 to 1 down"),
    )
    .param(super::keep_size())
    .param(ParamSpec::text("shapes", "").multiline().doc(
        "One per line: `tag rect x,y w,h`, `tag ellipse x,y w,h` or `tag poly x,y x,y ...`, \
             optionally `repeat CxR dx,dy`; surface coordinates, 0 to 1 from the top left (draw them in the viewer)",
    ))
    .param(
        ParamSpec::text("tag", "")
            .doc("Tags in the mask and the distance field, separated by spaces; empty: all"),
    )
    .param(
        ParamSpec::float("outline", 2.0)
            .clamp(0.0, 64.0)
            .range(0.5, 8.0)
            .unit(Unit::Pixels)
            .more()
            .doc("Width of the lines in the outline output, in surface pixels"),
    )
    .param(
        ParamSpec::text("trace_tag", TRACE_TAG)
            .group(TRACE_GROUP)
            .doc("Tag of the shapes traced from the `trace` input (one word: letters, digits, `_`)"),
    )
    .param(
        ParamSpec::choice("trace_channel", &Channel::NAMES)
            .group(TRACE_GROUP)
            .doc("The channel of the `trace` input that is traced")
            .choice_doc("auto", "alpha when the picture has transparent pixels, else luma")
            .choice_doc("red", "the red channel (label pictures often keep their labels there)")
            .choice_doc("green", "the green channel")
            .choice_doc("blue", "the blue channel")
            .choice_doc("alpha", "the alpha channel")
            .choice_doc("luma", "the brightness (Rec. 709 weights on the linear values)"),
    )
    .param(ParamSpec::float("trace_threshold", 0.5).range(0.0, 1.0).group(TRACE_GROUP).doc(
        "Inside a shape is above this value (below with `trace_invert`); not used with `trace_labels`",
    ))
    .param(
        ParamSpec::bool("trace_invert", false)
            .group(TRACE_GROUP)
            .doc("Inside is below the threshold: dark objects on a bright wall"),
    )
    .param(ParamSpec::bool("trace_labels", false).group(TRACE_GROUP).doc(
        "A label picture: every value above 0 is a region of its own (one shape per label), 0 is the \
         background; the threshold is not used",
    ))
    .param(
        ParamSpec::float("trace_tolerance", 1.0)
            .clamp(0.0, 1000.0)
            .range(0.0, 8.0)
            .unit(Unit::Pixels)
            .group(TRACE_GROUP)
            .doc("How far a traced line may stray from the pixel edges to save points, in pixels of the picture"),
    )
    .param(
        ParamSpec::float("trace_min_area", 64.0)
            .clamp(0.0, 1e12)
            .range(0.0, 10000.0)
            .curve(Curve::Power(2.0))
            .group(TRACE_GROUP)
            .doc("Regions of fewer pixels of the picture are specks and dropped; holes in regions are filled"),
    )
    .input("table", DataKind::Table)
    .input("trace", DataKind::Tex)
    .output("mask", DataKind::Tex)
    .output("sdf", DataKind::Tex)
    .output("outline", DataKind::Tex)
    .output("table", DataKind::Table)
    .output("id", DataKind::Tex)
    .describe_output("mask", "White inside the shapes of the tags in `tag`")
    .describe_output("sdf", "Signed distance to the edge of those shapes")
    .describe_output("outline", "Outlines of all shapes in their tag colours, to see them while placing them")
    .describe_output("table", "Every shape as a row; another tex.shapes can read it at its `table` input")
    .describe_output(
        "id",
        "Per pixel, the shape (of the tags in `tag`) the pixel is inside, else the nearest shape. Red: the \
         shape's id (the table's `id`). Green: a random value 0..1 per shape. Blue: where along the \
         outline, 0..1 clockwise. Alpha: signed distance to the edge in pixels. Data for \
         tex.shape_content, not something to look at",
    )
    .describe_input("table", "Shapes from a table (as the `table` output has them) instead of the node's own")
    .describe_input(
        "trace",
        "A mask or label picture whose regions become shapes tagged `trace_tag`, besides the node's \
         own: traced again whenever the picture or a `trace_*` param changes, never stored. Meant for a \
         picture that holds still (a tex.image_file, a shader on images); it is stretched to the surface",
    )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(ShapesOp::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

/// Compiles the pipeline ahead of the first node.
fn warm_up(gpu: &penumbra_gpu::Gpu) -> Result<(), Error> {
    Drawer::new().warm_up(gpu)
}

/// The shapes a node draws: from its `table` input when connected, else its `shapes` text.
pub fn source(cx: &CookContext<'_>) -> Result<Vec<shapes::Shape>, CookError> {
    if cx.def().inputs.first().copied().flatten().is_some() {
        let table = cx.input_as::<penumbra_core::Table>(0).ok_or(CookError::Pending)?;
        if !cx.text("shapes")?.trim().is_empty() {
            cx.warn("`shapes` is ignored while the table input is connected");
        }
        return shapes::from_table(table).map_err(CookError::Failed);
    }
    shapes::parse(cx.text("shapes")?).map_err(CookError::Failed)
}

#[derive(Default)]
struct ShapesOp {
    rings: [TexRing; 4],
    drawer: Option<Drawer>,
    trace: Tracer,
    /// Something has been drawn since the node was made.
    drawn: bool,
}

/// How the `trace` input is traced: the `trace_*` params.
#[derive(Clone, Debug, PartialEq)]
struct TraceSettings {
    tag: String,
    channel: Channel,
    options: Options,
}

impl TraceSettings {
    /// From the params, keeping at most `room` regions.
    fn of(cx: &CookContext<'_>, room: usize) -> Result<Self, CookError> {
        let tag = cx.text("trace_tag")?.trim().to_string();
        penumbra_core::Name::new(&tag).map_err(|e| CookError::failed(format!("`trace_tag` `{tag}`: {e}")))?;
        let channel = Channel::parse(cx.text("trace_channel")?).unwrap_or(Channel::Auto);
        Ok(Self {
            tag,
            channel,
            options: Options {
                threshold: cx.float("trace_threshold")? as f32,
                invert: cx.bool("trace_invert")?,
                labels: cx.bool("trace_labels")?,
                tolerance: cx.float("trace_tolerance")?,
                // Saturating `as`; the param is clamped to 0 or more.
                min_area: cx.float("trace_min_area")?.max(0.0).round() as u64,
                max_regions: room,
            },
        })
    }
}

/// What a trace of the `trace` input found.
#[derive(Clone, Debug)]
struct TraceDone {
    shapes: Vec<Shape>,
    /// The picture's size in pixels.
    size: (u32, u32),
    specks: usize,
    dropped: usize,
    /// The channel traced (never `Auto`).
    channel: Channel,
}

/// The tracing of the `trace` input: a readback, then the trace on a job, then its shapes.
#[derive(Default)]
struct Tracer {
    /// The picture and settings of the work in flight, else of `done`.
    source: Option<Data>,
    settings: Option<TraceSettings>,
    pending: Option<(PendingRead, Instant)>,
    job: Option<Job<TraceDone>>,
    /// The shapes traced last, drawn until the next trace is done.
    done: Option<TraceDone>,
    /// Why the last trace failed.
    error: Option<String>,
    /// When the last trace started, and whether it followed the one before closely because the
    /// picture changed again.
    started: Option<Instant>,
    restless: bool,
}

impl Tracer {
    fn busy(&self) -> bool {
        self.pending.is_some() || self.job.is_some()
    }

    /// Moves the trace of `source` on as far as it goes without waiting; true when new shapes
    /// arrived in this call.
    fn step(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &penumbra_gpu::Gpu,
        source: &Data,
        settings: TraceSettings,
    ) -> Result<bool, CookError> {
        let mut arrived = false;
        if let Some(result) = self.job.as_ref().and_then(Job::take) {
            self.job = None;
            match result {
                Ok(done) => {
                    self.done = Some(done);
                    self.error = None;
                    arrived = true;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.pending.as_ref().is_some_and(|(_, started)| started.elapsed() > STALE) {
            self.pending = None;
            self.error = Some("the GPU did not read the picture back in time".into());
        }
        let state = self.pending.as_ref().map(|(read, _)| read.wait(gpu, Duration::ZERO));
        match state {
            Some(ReadState::Ready) => {
                if let (Some((read, _)), Some(settings)) = (self.pending.take(), self.settings.clone()) {
                    self.job = Some(cx.spawn(move |_| trace_read(&read, &settings)));
                }
            }
            Some(ReadState::Failed) => {
                self.pending = None;
                self.error = Some("the GPU could not read the picture back".into());
            }
            Some(ReadState::Waiting) | None => {}
        }
        let same_source = self.source.as_ref().is_some_and(|s| s.same(source));
        let same_settings = self.settings.as_ref() == Some(&settings);
        if !(same_source && same_settings) && !self.busy() {
            let tex =
                source.get::<Tex>().ok_or_else(|| CookError::failed("the `trace` input is not a texture"))?;
            let (w, h) = tex.size();
            // A picture that changes all the time is traced again and again.
            let recent = self.started.is_some_and(|t| t.elapsed() < RESTLESS);
            self.restless = recent && !same_source && same_settings;
            self.started = Some(Instant::now());
            self.source = Some(source.clone());
            self.settings = Some(settings);
            if (w as usize).saturating_mul(h as usize) > MAX_PIXELS {
                self.done = None;
                self.error = Some(format!(
                    "it is {w} x {h}; at most {MAX_PIXELS} pixels are traced (make it smaller first)"
                ));
                return Ok(arrived);
            }
            let waker = cx.waker();
            let read = gpu.read_async_notify(
                &[tex],
                &mut ReadPool::default(),
                Some(Arc::new(move || waker.wake())),
            )?;
            self.pending = Some((read, Instant::now()));
        }
        Ok(arrived)
    }
}

/// Traces the picture of a finished readback as `settings` say.
fn trace_read(read: &PendingRead, settings: &TraceSettings) -> Result<TraceDone, String> {
    let (w, h) = read.size(0).ok_or("nothing was read back")?;
    let mut pixels: Vec<[f32; 4]> = Vec::with_capacity(w as usize * h as usize);
    read.rows(0, |row: &[u8]| -> Result<(), String> {
        for px in row.as_chunks::<8>().0 {
            let mut c = [0f32; 4];
            for (out, half) in c.iter_mut().zip(px.as_chunks::<2>().0) {
                *out = f16::from_le_bytes(*half).to_f32();
            }
            pixels.push(c);
        }
        Ok(())
    })
    .map_err(|e| match e {
        penumbra_gpu::RowsError::Gpu(e) => e.to_string(),
        penumbra_gpu::RowsError::Sink(e) => e,
    })?;
    let (values, channel) = trace::channel(&pixels, settings.channel);
    drop(pixels);
    let traced = trace::trace(w, h, &values, &settings.options)?;
    Ok(TraceDone {
        shapes: trace::to_shapes(&traced.regions, w, h, &settings.tag),
        size: (w, h),
        specks: traced.specks,
        dropped: traced.dropped,
        channel,
    })
}

/// A shape as drawn: its outline in target pixels, its colour and its id.
type Drawn = (Vec<[f32; 2]>, [f32; 4], u32);

fn as_drawn(polys: &[Drawn]) -> Vec<ShapePoly<'_>> {
    polys.iter().map(|(points, color, id)| ShapePoly { points, color: *color, id: *id }).collect()
}

/// An outline in target pixels, clockwise on screen (y down), so that runs along outlines
/// (`tex.shape_content`'s chase) go the same way round every shape.
fn outline(instance: &Instance, size: (u32, u32)) -> Vec<[f32; 2]> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let mut points: Vec<[f32; 2]> =
        instance.geom.outline().into_iter().map(|p| [(p[0] * w) as f32, (p[1] * h) as f32]).collect();
    // Twice the signed area; positive is clockwise with y down.
    let area: f64 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1]))
        .sum();
    if area < 0.0 {
        points.reverse();
    }
    points
}

impl Operator for ShapesOp {
    fn trim(&mut self) {
        for ring in &mut self.rings {
            ring.trim();
        }
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let size = (side(cx, "width")?, side(cx, "height")?);
        let mut all = source(cx)?;
        let arrived = self.traced(cx, gpu, &mut all, size)?;
        let tags = shapes::tags(&all);
        let filter = shapes::filter(cx.text("tag")?);
        for f in &filter {
            if !tags.contains(f) {
                cx.warn(format!("no shape has the tag `{f}`"));
            }
        }
        // Woken only because a trace moved on: nothing to draw again until its shapes arrive.
        let asked =
            cx.first_cook() || cx.params_changed() || cx.inputs_changed() || cx.outputs_wanted_changed();
        if self.drawn && !asked && !arrived {
            return Ok(Cooked::Unchanged);
        }
        let instances = shapes::instances(&all);
        // Ids are row numbers of the table: positions among all instances.
        let all_polys: Vec<Drawn> = instances
            .iter()
            .zip(0u32..)
            .map(|(i, id)| {
                let [r, g, b] = shapes::tag_color(&tags, &i.tag);
                (outline(i, size), [r, g, b, 1.0], id)
            })
            .collect();
        let chosen_polys: Vec<Drawn> = all_polys
            .iter()
            .zip(&instances)
            .filter(|(_, i)| shapes::matches(&filter, &i.tag))
            .map(|(p, _)| p.clone())
            .collect();
        let width = cx.float("outline")? as f32;
        let drawer = self.drawer.get_or_insert_with(Drawer::new);
        let modes = [
            (ShapeMode::Mask, as_drawn(&chosen_polys)),
            // Distances in surface widths: one pixel is 1 / width.
            (ShapeMode::Sdf { unit: 1.0 / size.0 as f32 }, as_drawn(&chosen_polys)),
            (ShapeMode::Outline { width }, as_drawn(&all_polys)),
            (ShapeMode::Id, as_drawn(&chosen_polys)),
        ];
        let mut outputs = Vec::with_capacity(5);
        for (ring, (mode, polys)) in self.rings.iter_mut().zip(modes) {
            let target: Tex = ring.next(gpu, size.0, size.1, SHAPES)?;
            drawer.render(gpu, &target, &polys, mode)?;
            outputs.push(Data::new(DataKind::Tex, target));
        }
        // The table goes between the outline and the ids (outputs keep their numbers).
        outputs.insert(3, Data::new(DataKind::Table, shapes::table(&instances)?));
        self.drawn = true;
        Ok(Cooked::Outputs(outputs))
    }

    fn job_status(&self) -> Option<JobStatus> {
        // Busy until the first shapes are traced, so a command reading the node waits for them.
        (self.trace.done.is_none() && self.trace.busy()).then_some(JobStatus::Running { progress: None })
    }
}

impl ShapesOp {
    /// With the `trace` input connected: moves its trace on and adds the shapes traced last to
    /// `all`; true when they are new. Pending until the first trace is done.
    fn traced(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &penumbra_gpu::Gpu,
        all: &mut Vec<Shape>,
        size: (u32, u32),
    ) -> Result<bool, CookError> {
        if cx.def().inputs.get(TRACE_INPUT).copied().flatten().is_none() {
            self.trace = Tracer::default();
            return Ok(false);
        }
        let Some(source) = cx.input(TRACE_INPUT) else {
            // The picture has not cooked (yet); its node says why.
            return if self.trace.done.is_some() { Ok(false) } else { Err(CookError::Pending) };
        };
        let room = MAX_INSTANCES.saturating_sub(shapes::instances(all).len());
        let settings = TraceSettings::of(cx, room)?;
        let arrived = self.trace.step(cx, gpu, source, settings)?;
        let Some(done) = &self.trace.done else {
            return Err(match &self.trace.error {
                Some(e) => CookError::failed(format!("tracing the `trace` input: {e}")),
                None => CookError::Pending,
            });
        };
        let mut warnings = Vec::new();
        if let Some(e) = &self.trace.error {
            warnings.push(format!("tracing the `trace` input: {e} (the shapes traced before are kept)"));
        }
        if self.trace.restless && self.trace.busy() {
            warnings.push(
                "the `trace` input changes all the time and is traced again and again: trace a picture \
                 that holds still"
                    .to_string(),
            );
        }
        let (w, h) = done.size;
        let ratio = (f64::from(w) / f64::from(h.max(1))) / (f64::from(size.0) / f64::from(size.1.max(1)));
        if (ratio - 1.0).abs() > 0.01 {
            warnings.push(format!(
                "the `trace` input is {w} x {h} but the surface {} x {}: its shapes are stretched",
                size.0, size.1
            ));
        }
        if done.dropped > 0 {
            warnings.push(format!(
                "{} smallest traced regions dropped: at most {MAX_INSTANCES} shapes",
                done.dropped
            ));
        }
        if done.shapes.is_empty() {
            warnings.push(format!(
                "nothing traced in the {} channel of the `trace` input ({} speck(s) dropped)",
                done.channel.name(),
                done.specks
            ));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        all.extend(done.shapes.iter().cloned());
        Ok(arrived)
    }
}
