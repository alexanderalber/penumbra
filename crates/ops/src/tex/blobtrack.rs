//! `tex.blobtrack`: finds blobs in the input (a camera, usually) and follows them with ids that
//! stay, by [`penumbra_track::Tracker`].
//!
//! The GPU shrinks the input to `size` pixels wide; that small picture is read back and
//! analysed on the cook thread (a few hundred thousand pixels at most, well under a
//! millisecond). The readback waits up to [`WAIT`] within the cook, so with a quick GPU the
//! blobs belong to the frame that just arrived; when it takes longer the node cooks again once
//! the copy is there, and a frame arriving in between is skipped rather than queued.
//!
//! Outputs: `out`, a picture of what the tracker sees at the analysed size (`show`); `blobs`,
//! a table with a header row and one row per blob; `chans`, `count` plus `id`, `x`, `y`,
//! `area` per slot. A blob keeps its slot while it is tracked and a new one takes the lowest
//! free slot, so a binding to `x1` follows one person rather than whoever is first in the list.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use half::f16;
use half::slice::HalfFloatSliceExt;
use penumbra_core::{Channel, Chans, Curve, Error, Name, NodeId, ParamSpec, Table, Unit, lock};
use penumbra_gpu::{Gpu, PassInput, PendingRead, ReadPool, ReadState, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, Waker,
};
use penumbra_track::{Blob, Detect, Image, Settings, Tracker};

use super::{gpu, waiting};

/// The node kind.
pub const BLOBTRACK: &str = "tex.blobtrack";
const SOURCE: &str = include_str!("blobtrack.wgsl");
/// How long a cook waits for the readback of the frame it just started.
pub const WAIT: Duration = Duration::from_millis(4);
/// A readback older than this is given up.
const STALE: Duration = Duration::from_secs(2);
/// Most slots in `chans`.
const MAX_SLOTS: i64 = 32;
/// Columns of `blobs`.
pub const COLUMNS: [&str; 14] =
    ["id", "x", "y", "left", "top", "width", "height", "area", "age", "vx", "vy", "r", "g", "b"];

fn desc() -> NodeDesc {
    NodeDesc::new(BLOBTRACK, "Finds blobs (people, hands, spots of light) and follows them with ids that stay")
        .param(ParamSpec::choice("detect", &["background", "bright"]).doc(
            "background: what differs from the empty scene (the `background` input where connected, else \
             learned: `blobtrack.learn`); bright: whatever is brighter than `threshold`",
        ))
        .param(
            ParamSpec::float("threshold", 0.1)
                .clamp(0.0, 1.0)
                .range(0.0, 0.5)
                .curve(Curve::Power(2.0))
                .doc("Difference to the background (in any channel), or brightness, from which a pixel counts"),
        )
        .param(
            ParamSpec::float("adapt", 30.0)
                .clamp(0.0, 3600.0)
                .range(0.0, 300.0)
                .curve(Curve::Power(2.0))
                .unit(Unit::Seconds)
                .doc(
                    "How soon a lasting change (light, a moved chair) becomes background; someone standing \
                     still fades ten times slower. 0: never",
                ),
        )
        .param(
            ParamSpec::float("min_area", 0.1)
                .clamp(0.0, 100.0)
                .range(0.0, 5.0)
                .curve(Curve::Power(2.0))
                .unit(Unit::Percent)
                .doc("Smaller blobs are ignored (of the picture's area)"),
        )
        .param(
            ParamSpec::float("max_area", 50.0)
                .clamp(0.0, 100.0)
                .range(1.0, 100.0)
                .unit(Unit::Percent)
                .doc("Larger blobs are ignored (the whole room changing is not a blob)"),
        )
        .param(ParamSpec::int("max_blobs", 16).clamp(1.0, 256.0).range(1.0, 32.0).doc("Most blobs, the largest"))
        .param(ParamSpec::choice("show", &["overlay", "mask", "difference", "background"]).doc(
            "What `out` shows: the picture with the blobs marked, the pixels counted, the difference (the \
             threshold is mid grey) or the learned background",
        ))
        .param(
            ParamSpec::int("size", 320)
                .clamp(16.0, 1920.0)
                .range(64.0, 960.0)
                .curve(Curve::Log)
                .unit(Unit::Pixels)
                .more()
                .doc("Width the input is analysed at; more finds smaller things and costs more"),
        )
        .param(
            ParamSpec::int("open", 1)
                .clamp(0.0, 16.0)
                .range(0.0, 6.0)
                .unit(Unit::Pixels)
                .more()
                .doc("Removes specks and lines thinner than twice this (at the analysed size)"),
        )
        .param(
            ParamSpec::int("close", 2)
                .clamp(0.0, 16.0)
                .range(0.0, 6.0)
                .unit(Unit::Pixels)
                .more()
                .doc("Fills holes and joins parts closer than twice this"),
        )
        .param(
            ParamSpec::float("reach", 0.15)
                .clamp(0.0, 2.0)
                .range(0.0, 0.5)
                .more()
                .doc("How far a blob may jump between frames and keep its id, beyond where its speed puts it (of the width)"),
        )
        .param(
            ParamSpec::float("hold", 0.5)
                .clamp(0.0, 60.0)
                .range(0.0, 5.0)
                .unit(Unit::Seconds)
                .more()
                .doc("A blob gone for less keeps its id when it comes back"),
        )
        .param(ParamSpec::int("slots", 4).clamp(0.0, MAX_SLOTS as f64).range(0.0, 8.0).more().doc(
            "Blobs in `chans`, each as id, x, y, area with the slot's number (id 0: empty)",
        ))
        .input("in", DataKind::Tex)
        .input("background", DataKind::Tex)
        .output("out", DataKind::Tex)
        .output("blobs", DataKind::Table)
        .output("chans", DataKind::Chan)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(BlobTrack::new()) as Box<dyn Operator>))
}

fn pass(gpu: &Gpu) -> Result<Arc<penumbra_gpu::Pass>, Error> {
    gpu.cached_pass(BLOBTRACK, SOURCE, &[])
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

/// Requests to the blob trackers from outside the cook (`blobtrack.learn`), by node. Provided by
/// the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct BlobTracks(Arc<Mutex<BTreeMap<NodeId, Entry>>>);

struct Entry {
    waker: Waker,
    /// When the node takes a frame as the background.
    learn: Option<Instant>,
}

impl BlobTracks {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<NodeId, Entry>> {
        lock(&self.0)
    }

    /// The node takes the first frame after `delay` as the background (a delay gives whoever
    /// clicked time to leave the picture). False when it has not cooked yet.
    pub fn learn(&self, node: NodeId, delay: Duration) -> bool {
        let mut entries = self.lock();
        let Some(e) = entries.get_mut(&node) else { return false };
        e.learn = Some(Instant::now() + delay);
        e.waker.wake();
        true
    }

    /// Seconds until the node learns, if it is about to.
    pub fn learning_in(&self, node: NodeId) -> Option<f64> {
        let at = self.lock().get(&node)?.learn?;
        Some(at.saturating_duration_since(Instant::now()).as_secs_f64())
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        self.lock().retain(|id, _| keep(*id));
    }

    /// Registers the node's waker; true when it is time to learn.
    fn check_in(&self, node: NodeId, waker: Waker) -> bool {
        let mut entries = self.lock();
        let e = entries.entry(node).or_insert(Entry { waker: waker.clone(), learn: None });
        e.waker = waker;
        let due = e.learn.is_some_and(|at| at <= Instant::now());
        if due {
            e.learn = None;
        }
        due
    }
}

struct BlobTrack {
    tracker: Tracker,
    small: TexRing,
    small_background: TexRing,
    view: TexRing,
    pool: ReadPool,
    /// The readback in flight and when its frame arrived.
    pending: Option<(PendingRead, Instant)>,
    /// When the last analysed frame arrived.
    last_frame: Option<Instant>,
    /// The blob id in each slot of `chans`.
    slots: Vec<Option<u64>>,
    outputs: Option<Vec<Data>>,
}

impl BlobTrack {
    fn new() -> Self {
        BlobTrack {
            tracker: Tracker::new(),
            small: TexRing::new(),
            small_background: TexRing::new(),
            view: TexRing::new(),
            pool: ReadPool::default(),
            pending: None,
            last_frame: None,
            slots: Vec::new(),
            outputs: None,
        }
    }
}

fn settings(cx: &CookContext<'_>) -> Result<Settings, CookError> {
    let int = |name: &str| -> Result<usize, CookError> { Ok(usize::try_from(cx.int(name)?).unwrap_or(0)) };
    Ok(Settings {
        detect: if cx.text("detect")? == "bright" { Detect::Bright } else { Detect::Background },
        threshold: cx.float("threshold")? as f32,
        adapt: cx.float("adapt")? as f32,
        open: int("open")?,
        close: int("close")?,
        min_area: (cx.float("min_area")? / 100.0) as f32,
        max_area: (cx.float("max_area")? / 100.0) as f32,
        max_blobs: int("max_blobs")?.max(1),
        reach: cx.float("reach")? as f32,
        hold: cx.float("hold")? as f32,
    })
}

impl Operator for BlobTrack {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        if cx.resource::<BlobTracks>().is_some_and(|t| t.check_in(cx.node(), cx.waker())) {
            self.tracker.learn();
        }
        let src = cx.input_as::<Tex>(0).ok_or_else(|| waiting(cx))?;
        // A readback that never arrives (a lost device) must not stop the node for good.
        if self.pending.as_ref().is_some_and(|(_, started)| started.elapsed() > STALE) {
            self.pending = None;
        }
        if (cx.inputs_changed() || cx.first_cook() || self.outputs.is_none()) && self.pending.is_none() {
            self.start(cx, gpu, src)?;
        }
        let ready = match &self.pending {
            Some((read, _)) => read.wait(gpu, WAIT),
            None => ReadState::Waiting,
        };
        match ready {
            ReadState::Ready => {
                if let Some((read, arrived)) = self.pending.take() {
                    let image = image_of(&read, 0);
                    let background = (read.len() > 1).then(|| image_of(&read, 1));
                    self.pool = read.recycle();
                    let image = image.map_err(CookError::failed)?;
                    if let Some(bg) = background {
                        self.tracker.set_background(bg.map_err(CookError::failed)?);
                    }
                    let outputs = self.analyse(cx, gpu, &image, arrived)?;
                    self.outputs = Some(outputs.clone());
                    return Ok(Cooked::Outputs(outputs));
                }
            }
            ReadState::Failed => {
                self.pending = None;
                return Err(CookError::failed("the GPU could not read the picture back"));
            }
            ReadState::Waiting => {}
        }
        match (&self.outputs, cx.params_changed()) {
            // A param changed without a new frame: the next frame shows it.
            (Some(_), _) => Ok(Cooked::Unchanged),
            (None, _) => Err(CookError::Pending),
        }
    }

    fn job_status(&self) -> Option<penumbra_graph::JobStatus> {
        (self.outputs.is_none() && self.pending.is_some())
            .then_some(penumbra_graph::JobStatus::Running { progress: None })
    }
}

impl BlobTrack {
    /// Shrinks the input on the GPU and starts reading it back; the node is woken when it
    /// arrives.
    fn start(&mut self, cx: &CookContext<'_>, gpu: &Gpu, src: &Tex) -> Result<(), CookError> {
        let (w, h) = src.size();
        let width = u32::try_from(cx.int("size")?).unwrap_or(320).clamp(16, w.max(16));
        let height = ((u64::from(width) * u64::from(h)) / u64::from(w.max(1))).clamp(1, 4096) as u32;
        let target = self.small.next(gpu, width, height, BLOBTRACK)?;
        let pass = pass(gpu)?;
        pass.render(
            gpu,
            &target,
            &PassInput { inputs: &[Some(src)], seconds: 0.0, frame: 0, param: &|_| None },
        );
        // A connected background is shrunk the same way and read back with the frame.
        let background = match cx.input_as::<Tex>(1) {
            Some(bg) => {
                let t = self.small_background.next(gpu, width, height, BLOBTRACK)?;
                pass.render(
                    gpu,
                    &t,
                    &PassInput { inputs: &[Some(bg)], seconds: 0.0, frame: 0, param: &|_| None },
                );
                Some(t)
            }
            None => None,
        };
        let mut textures = vec![&target];
        textures.extend(background.as_ref());
        let waker = cx.waker();
        let read = gpu.read_async_notify(&textures, &mut self.pool, Some(Arc::new(move || waker.wake())))?;
        self.pending = Some((read, Instant::now()));
        Ok(())
    }

    fn analyse(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &Gpu,
        image: &Image,
        arrived: Instant,
    ) -> Result<Vec<Data>, CookError> {
        let mut s = settings(cx)?;
        if cx.input(1).is_some() {
            // The background comes from the input every frame; nothing to adapt.
            s.adapt = 0.0;
        }
        let dt = self.last_frame.map_or(0.0, |t| arrived.saturating_duration_since(t).as_secs_f32());
        self.last_frame = Some(arrived);
        let blobs = self.tracker.update(image, dt, &s);
        let mut bytes = view(cx.text("show")?, image, &self.tracker, &blobs, s.threshold);
        let (w, h) = (image.width as u32, image.height as u32);
        let out = self.view.next(gpu, w, h, BLOBTRACK)?;
        gpu.write_rgba16f(&out, &mut bytes)?;
        let slots = usize::try_from(cx.int("slots")?).unwrap_or(0);
        assign_slots(&mut self.slots, slots, &blobs);
        Ok(vec![
            Data::new(DataKind::Tex, out),
            Data::new(DataKind::Table, table(&blobs)?),
            Data::new(DataKind::Chan, chans(&self.slots, &blobs, cx.rate().as_f64())?),
        ])
    }
}

/// The readback's rows as straight RGB.
fn image_of(read: &PendingRead, index: usize) -> Result<Image, String> {
    let (w, h) = read.size(index).ok_or("nothing was read back")?;
    let mut data = Vec::with_capacity(w as usize * h as usize);
    let mut halves = Vec::with_capacity(w as usize * 4);
    let mut floats = vec![0f32; w as usize * 4];
    read.rows(index, |row: &[u8]| -> Result<(), String> {
        halves.clear();
        halves.extend(row.as_chunks::<2>().0.iter().map(|b| f16::from_le_bytes(*b)));
        floats.resize(halves.len(), 0.0);
        // The slice conversion uses the CPU's half-float instructions where it has them.
        halves.convert_to_f32_slice(&mut floats);
        data.extend(floats.as_chunks::<4>().0.iter().map(|[r, g, b, _]| [*r, *g, *b]));
        Ok(())
    })
    .map_err(|e| match e {
        penumbra_gpu::RowsError::Gpu(e) => e.to_string(),
        penumbra_gpu::RowsError::Sink(e) => e,
    })?;
    Image::new(w as usize, h as usize, data).ok_or_else(|| "the readback has the wrong size".to_string())
}

/// Keeps each tracked blob in its slot, frees the slots of blobs gone and fills free slots
/// with new blobs, oldest (lowest id) first.
fn assign_slots(slots: &mut Vec<Option<u64>>, count: usize, blobs: &[Blob]) {
    slots.resize(count, None);
    for s in slots.iter_mut() {
        if s.is_some_and(|id| !blobs.iter().any(|b| b.id == id)) {
            *s = None;
        }
    }
    for b in blobs {
        if slots.contains(&Some(b.id)) {
            continue;
        }
        if let Some(free) = slots.iter_mut().find(|s| s.is_none()) {
            *free = Some(b.id);
        }
    }
}

fn table(blobs: &[Blob]) -> Result<Table, Error> {
    let mut rows = Vec::with_capacity(blobs.len() + 1);
    rows.push(COLUMNS.iter().map(|c| c.to_string()).collect());
    for b in blobs {
        let n = |v: f32| format!("{v:.4}");
        rows.push(vec![
            b.id.to_string(),
            n(b.x),
            n(b.y),
            n(b.left),
            n(b.top),
            n(b.width),
            n(b.height),
            format!("{:.5}", b.area),
            format!("{:.2}", b.age),
            n(b.vx),
            n(b.vy),
            n(b.color[0]),
            n(b.color[1]),
            n(b.color[2]),
        ]);
    }
    Table::new(rows)
}

fn chans(slots: &[Option<u64>], blobs: &[Blob], rate: f64) -> Result<Chans, Error> {
    let mut channels = Vec::with_capacity(1 + 4 * slots.len());
    channels.push(Channel { name: Name::new("count")?, samples: vec![blobs.len() as f64] });
    for (i, slot) in slots.iter().enumerate() {
        let b = slot.and_then(|id| blobs.iter().find(|b| b.id == id));
        let n = i + 1;
        let values = [
            ("id", b.map_or(0.0, |b| b.id as f64)),
            ("x", b.map_or(0.0, |b| f64::from(b.x))),
            ("y", b.map_or(0.0, |b| f64::from(b.y))),
            ("area", b.map_or(0.0, |b| f64::from(b.area))),
        ];
        for (name, v) in values {
            channels.push(Channel { name: Name::new(&format!("{name}{n}"))?, samples: vec![v] });
        }
    }
    Chans::new(rate, channels)
}

/// A colour per id, bright and far apart for neighbouring ids (golden-ratio hues).
fn id_color(id: u64) -> [f32; 3] {
    let hue = ((id as f64 * 0.618_034) % 1.0) as f32 * 6.0;
    let x = 1.0 - ((hue % 2.0) - 1.0).abs();
    match hue as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    }
}

/// The `out` picture as half-float RGBA rows.
fn view(show: &str, image: &Image, tracker: &Tracker, blobs: &[Blob], threshold: f32) -> Vec<u8> {
    let (w, h) = (image.width, image.height);
    let mask = tracker.mask();
    let value = tracker.value();
    let mut pixels: Vec<[f32; 3]> = match show {
        "mask" => mask.data.iter().map(|m| [f32::from(*m); 3]).collect(),
        "difference" => {
            let scale = 0.5 / threshold.max(1e-4);
            value.iter().map(|v| [(v * scale).clamp(0.0, 1.0); 3]).collect()
        }
        "background" => tracker.background().map_or_else(|| image.data.clone(), |b| b.data.clone()),
        _ => {
            let mut px: Vec<[f32; 3]> = image
                .data
                .iter()
                .zip(&mask.data)
                .map(|(c, m)| if *m != 0 { *c } else { c.map(|v| v * 0.3) })
                .collect();
            for b in blobs {
                outline(&mut px, w, h, b, id_color(b.id));
            }
            px
        }
    };
    pixels.resize(w * h, [0.0; 3]);
    let floats: Vec<f32> = pixels.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
    let mut halves = vec![f16::ZERO; floats.len()];
    halves.convert_from_f32_slice(&floats);
    halves.iter().flat_map(|h| h.to_le_bytes()).collect()
}

/// The blob's box and a cross at its centre.
fn outline(px: &mut [[f32; 3]], w: usize, h: usize, b: &Blob, color: [f32; 3]) {
    let to_px = |v: f32, n: usize| ((v * n as f32).max(0.0) as usize).min(n.saturating_sub(1));
    let (x0, y0) = (to_px(b.left, w), to_px(b.top, h));
    let (x1, y1) = (to_px(b.left + b.width, w), to_px(b.top + b.height, h));
    let mut set = |x: usize, y: usize| {
        if let Some(p) = px.get_mut(y * w + x) {
            *p = color;
        }
    };
    for x in x0..=x1 {
        set(x, y0);
        set(x, y1);
    }
    for y in y0..=y1 {
        set(x0, y);
        set(x1, y);
    }
    let (cx, cy) = (to_px(b.x, w), to_px(b.y, h));
    for d in 0..4usize {
        set(cx.saturating_sub(d), cy);
        set((cx + d).min(w.saturating_sub(1)), cy);
        set(cx, cy.saturating_sub(d));
        set(cx, (cy + d).min(h.saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(id: u64) -> Blob {
        Blob {
            id,
            x: 0.5,
            y: 0.5,
            left: 0.4,
            top: 0.4,
            width: 0.2,
            height: 0.2,
            area: 0.04,
            color: [1.0; 3],
            age: 0.0,
            vx: 0.0,
            vy: 0.0,
        }
    }

    #[test]
    fn slots_stay_with_their_blob() {
        let mut slots = Vec::new();
        assign_slots(&mut slots, 3, &[blob(1), blob(2)]);
        assert_eq!(slots, [Some(1), Some(2), None]);
        assign_slots(&mut slots, 3, &[blob(2), blob(5)]);
        assert_eq!(slots, [Some(5), Some(2), None], "2 keeps slot 2, 5 takes the free slot 1");
        assign_slots(&mut slots, 1, &[blob(2), blob(5)]);
        assert_eq!(slots, [Some(5)]);
    }

    #[test]
    fn table_and_chans_have_their_columns() {
        let t = table(&[blob(3)]).unwrap();
        assert_eq!(t.rows()[0], COLUMNS.map(String::from));
        assert_eq!(t.rows()[1][0], "3");
        let c = chans(&[Some(3), None], &[blob(3)], 60.0).unwrap();
        assert_eq!(c.value(Some("count")), Some(1.0));
        assert_eq!(c.value(Some("x1")), Some(0.5));
        assert_eq!(c.value(Some("id2")), Some(0.0));
    }

    #[test]
    fn the_view_has_the_picture_size() {
        let img = Image::filled(20, 10, [0.5; 3]);
        let tracker = Tracker::new();
        for show in ["overlay", "mask", "difference", "background"] {
            assert_eq!(view(show, &img, &tracker, &[blob(1)], 0.1).len(), 20 * 10 * 8, "{show}");
        }
    }
}
