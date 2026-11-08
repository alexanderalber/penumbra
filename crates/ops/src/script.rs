//! `script.python`: a Python script turning chans, tables and pictures into chans, a table, a
//! picture and a mesh. The Python side lives in `penumbra-script` and `pyhost`; this is the node.
//!
//! The script comes from `code` or, when set, from `file` (hot reloaded). Loading runs in a job,
//! so a slow import costs the show nothing; a version that fails to load leaves the last good
//! one running, with the error as a warning. Calls run on the cook thread, for at most
//! `timeout`; one stopped by the watchdog stays stopped until its code or `timeout` changes.
//!
//! Pictures and meshes are numpy arrays on the Python side. A picture input is read back from
//! the GPU before the call (the cook waits a few milliseconds for it, else the node cooks again
//! once it is there, as `tex.blob_track` does); the `picture` and `mesh` outputs are made only
//! while something reads them, and hold the last one the script set.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use half::slice::HalfFloatSliceExt;
use penumbra_core::{Chans, Curve, Error, ParamSpec, Table, Unit, Value};
use penumbra_gpu::geo::Model;
use penumbra_gpu::{Geo, PendingRead, ReadPool, ReadState, Tex, TexRing};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};
use penumbra_mesh::{Material, Mesh, Range, Vertex};
use penumbra_script::{Call, InPicture, Load, Loaded, OutMesh, OutPicture, Samples, ScriptError};

/// The node's own params; a script's `PARAMS` may not reuse these names.
const FIXED: &[&str] = &["file", "code", "cook", "timeout", "values"];

/// The first picture input (`tex0`), after the chan and table inputs.
const TEX0: usize = 4;
/// Picture inputs.
const PICTURES: usize = 2;
/// The `picture` and `mesh` outputs.
const PICTURE: usize = 2;
const MESH: usize = 3;
/// Most pixels of a picture input: reading back more every frame would cost the show more
/// than the script. 2048 x 2048, so 1080p fits.
const MAX_IN_PIXELS: u64 = 1 << 22;
/// How long a cook waits for the readback of its picture inputs: a late one makes the call a
/// frame later rather than holding up the frame; up to [`STALE`] while the clock waits for
/// every frame.
const WAIT: Duration = Duration::from_millis(4);
/// A readback older than this is given up.
const STALE: Duration = Duration::from_secs(2);
const LABEL: &str = "script.python";

/// When the node recooks: `auto` every frame if the code mentions `time` or `frame`, else when
/// an input or a param changes; `every frame`; `on change`.
const COOK_MODES: [&str; 3] = ["auto", "every frame", "on change"];

/// Largest script file read.
const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// What a new script node holds: a working example that explains the interface.
pub const DEFAULT_CODE: &str = r#"# cook(cx) runs when an input or a param changes, and every frame
# if the script mentions `time` or `frame` (the node's `cook` param decides otherwise).
# In:  cx.chans[0], cx.chans[1]   channels (name -> list of samples) or None
#      cx.tables[0], cx.tables[1] rows of text cells, or None
#      cx.pictures[0], cx.pictures[1] numpy arrays (height, width, 4), or None
#      cx.params (the values of PARAMS), cx.time, cx.frame, cx.rate
# Out: cx.out[name] = a number or a list, cx.table.append([cells]), cx.warn(text)
#      cx.picture = an array, cx.mesh = dict(positions=..., indices=...) (numpy)

PARAMS = {"gain": (1.0, 0.0, 2.0)}

def cook(cx):
    a = cx.chans[0]
    cx.out["value"] = (a.value() if a else 0.0) * cx.params["gain"]
"#;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "script.python",
            "Logic of your own in Python: turns channels, tables and pictures into new channels, a table, a picture and a mesh",
        )
        .param(
            ParamSpec::file("file", &["py"])
                .doc("A .py file used instead of `code`, relative to the project; reloads when saved"),
        )
        .param(
            ParamSpec::choice("cook", &COOK_MODES)
                .doc("When cook(cx) runs")
                .choice_doc("auto", "every frame if the code mentions `time` or `frame`, else as on change")
                .choice_doc("every frame", "every frame, for scripts that keep changing on their own")
                .choice_doc("on change", "only when an input or a param changes"),
        )
        .param(
            ParamSpec::float("timeout", 0.1)
                .clamp(0.001, 10.0)
                .range(0.01, 1.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("A call running longer than this is stopped. The node then stays stopped until the script or `timeout` changes"),
        )
        .param(ParamSpec::text("code", DEFAULT_CODE).lines(14).code().doc("The script while `file` is empty"))
        .param(
            ParamSpec::choice("values", &["color", "data"])
                .doc("How the numbers in `cx.picture` and `cx.pictures` relate to the colours")
                .choice_doc(
                    "color",
                    "a picture: uint8 is sRGB (0 to 255), float is linear (0 to 1), alpha straight; inputs come as linear floats with straight alpha",
                )
                .choice_doc("data", "every channel as stored, for maps, positions and masks: no sRGB, no alpha applied"),
        )
        .input("chan0", DataKind::Chan)
        .input("chan1", DataKind::Chan)
        .input("table0", DataKind::Table)
        .input("table1", DataKind::Table)
        .input("tex0", DataKind::Tex)
        .input("tex1", DataKind::Tex)
        .describe_input("tex0", "A picture for cx.pictures[0], read back to the CPU before every call; at most 2048 x 2048 pixels, so shrink larger ones first")
        .describe_input("tex1", "A picture for cx.pictures[1], as tex0")
        .output("out", DataKind::Chan)
        .output("table", DataKind::Table)
        .output("picture", DataKind::Tex)
        .output("mesh", DataKind::Geo)
        .lazy_output("picture")
        .lazy_output("mesh")
        .describe_output("out", "The channels cook(cx) returns")
        .describe_output("table", "The table cook(cx) returns; bind a text or a choice param to one of its cells")
        .describe_output("picture", "The picture cook(cx) puts in cx.picture; holds the last one until the script sets another")
        .describe_output("mesh", "The mesh cook(cx) puts in cx.mesh; holds the last one until the script sets another")
        .extra_params(),
        || Box::new(Script::new()) as Box<dyn Operator>,
    ))
}

#[derive(Clone, Debug, PartialEq)]
enum Source {
    Code(String),
    File(PathBuf),
}

struct Script {
    /// This node's module slot in the interpreter.
    key: u64,
    generation: u64,
    slot: AsyncSlot<Loaded>,
    current: Option<Loaded>,
    source: Option<Source>,
    watch: Option<FileWatch>,
    /// Stopped by the watchdog: the message, and the timeout it ran into.
    stopped: Option<(String, f64)>,
    /// The `cook` param as last seen: `Some(true)` every frame, `Some(false)` on change, `None`
    /// as the code says.
    every_frame: Option<bool>,
    /// The picture inputs as last read back, and whether as data.
    pictures: [Option<InPicture>; PICTURES],
    read_as_data: bool,
    /// The readback in flight, which inputs it holds, and when it started.
    pending: Option<(PendingRead, Vec<usize>, Instant)>,
    pool: ReadPool,
    /// The `picture` output's textures, the last picture made, and the empty picture shown
    /// before the script sets one.
    ring: TexRing,
    picture: Option<Tex>,
    blank: Option<Tex>,
    /// The last mesh made.
    mesh: Option<(Geo, Arc<Model>)>,
    /// A call has produced outputs: while a readback is on its way, they stay.
    made: bool,
}

impl Script {
    fn new() -> Self {
        Self {
            key: penumbra_script::host().new_key(),
            generation: 0,
            slot: AsyncSlot::new(),
            current: None,
            source: None,
            watch: None,
            stopped: None,
            every_frame: None,
            pictures: [None, None],
            read_as_data: false,
            pending: None,
            pool: ReadPool::default(),
            ring: TexRing::new(),
            picture: None,
            blank: None,
            mesh: None,
            made: false,
        }
    }

    fn has_picture(&self, i: usize) -> bool {
        self.pictures.get(i).is_some_and(Option::is_some)
    }

    /// Makes `self.pictures` hold the connected picture inputs, read back for this cook. `false`
    /// while a readback is still on its way (the node is woken when it arrives).
    fn read_pictures(&mut self, cx: &CookContext<'_>, data: bool) -> Result<bool, CookError> {
        let connected: Vec<usize> =
            (0..PICTURES).filter(|i| cx.input_as::<Tex>(TEX0 + i).is_some()).collect();
        for (i, p) in self.pictures.iter_mut().enumerate() {
            if !connected.contains(&i) {
                *p = None;
            }
        }
        if connected.is_empty() {
            self.pending = None;
            return Ok(true);
        }
        let gpu = crate::gpu(cx)?;
        if self.pending.as_ref().is_some_and(|(_, _, started)| started.elapsed() > STALE) {
            self.pending = None;
        }
        let have = connected.iter().all(|i| self.has_picture(*i)) && self.read_as_data == data;
        if self.pending.is_none() && (!have || cx.inputs_changed() || cx.first_cook()) {
            let mut textures = Vec::with_capacity(connected.len());
            for i in &connected {
                let Some(tex) = cx.input_as::<Tex>(TEX0 + i) else { continue };
                let (w, h) = tex.size();
                if u64::from(w) * u64::from(h) > MAX_IN_PIXELS {
                    return Err(CookError::failed(format!(
                        "tex{i}: {w} x {h} pixels is more than a script reads (2048 x 2048); shrink it first, with tex.resolution or tex.fit"
                    )));
                }
                textures.push(tex);
            }
            let waker = cx.waker();
            let read =
                gpu.read_async_notify(&textures, &mut self.pool, Some(Arc::new(move || waker.wake())))?;
            self.pending = Some((read, connected.clone(), Instant::now()));
        }
        let Some((read, _, _)) = &self.pending else { return Ok(have) };
        let wait = if cx.exact_frames() { STALE } else { WAIT };
        match read.wait(gpu, wait) {
            ReadState::Ready => {
                let Some((read, inputs, _)) = self.pending.take() else { return Ok(have) };
                let mut result = Ok(());
                for (k, i) in inputs.iter().enumerate() {
                    match picture_of(&read, k, data) {
                        Ok(p) => {
                            if let Some(slot) = self.pictures.get_mut(*i) {
                                *slot = Some(p);
                            }
                        }
                        Err(e) => result = Err(e),
                    }
                }
                self.pool = read.recycle();
                result.map_err(CookError::failed)?;
                self.read_as_data = data;
                if inputs == connected {
                    return Ok(true);
                }
                // An input was connected while the readback was on its way: read again, now
                // all of them (this readback's waker has fired already).
                self.read_pictures(cx, data)
            }
            ReadState::Failed => {
                self.pending = None;
                Err(CookError::failed("the GPU could not read the input picture back"))
            }
            ReadState::Waiting => Ok(false),
        }
    }

    /// The `picture` output: `made` uploaded if the script set one, else the last picture.
    fn picture_output(
        &mut self,
        cx: &CookContext<'_>,
        made: Option<OutPicture>,
        data: bool,
    ) -> Result<Data, CookError> {
        if !cx.output_wanted(PICTURE) {
            return Ok(Data::skipped(DataKind::Tex));
        }
        let gpu = crate::gpu(cx)?;
        if let Some(p) = made {
            let mut bytes = rgba16f(&p, data);
            let tex = self.ring.next(gpu, p.width, p.height, LABEL)?;
            gpu.write_rgba16f(&tex, &mut bytes)?;
            self.picture = Some(tex);
        }
        if let Some(t) = &self.picture {
            return Ok(Data::new(DataKind::Tex, t.clone()));
        }
        cx.warn("cook(cx) set no cx.picture yet; the `picture` output is empty");
        let blank = match &self.blank {
            Some(t) => t.clone(),
            None => {
                let t = gpu.upload_rgba16f(1, 1, &mut [0; 8], LABEL)?;
                self.blank = Some(t.clone());
                t
            }
        };
        Ok(Data::new(DataKind::Tex, blank))
    }

    /// The `mesh` output: `made` uploaded if the script set one, else the last mesh.
    fn mesh_output(&mut self, cx: &CookContext<'_>, made: Option<OutMesh>) -> Result<Data, CookError> {
        if !cx.output_wanted(MESH) {
            return Ok(Data::skipped(DataKind::Geo));
        }
        if let Some(m) = made {
            let gpu = crate::gpu(cx)?;
            let mesh = mesh_of(m).map_err(|e| CookError::failed(format!("cx.mesh: {e}")))?;
            let (model, _) = Model::upload(gpu, mesh, LABEL)?;
            let model = Arc::new(model);
            self.mesh = Some((Geo::single(model.clone()), model));
        }
        match &self.mesh {
            Some((geo, _)) => Ok(Data::new(DataKind::Geo, geo.clone())),
            None => {
                cx.warn("cook(cx) set no cx.mesh yet; the `mesh` output is empty");
                Ok(Data::new(DataKind::Geo, Geo::default()))
            }
        }
    }

    fn start_load(&mut self, cx: &CookContext<'_>, source: &Source) {
        self.generation += 1;
        let (key, generation) = (self.key, self.generation);
        let source = source.clone();
        let (label, folder) = match &source {
            Source::Code(_) => (cx.def().name.to_string(), cx.resolve_path(".").ok()),
            Source::File(p) => (
                p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
                p.parent().map(PathBuf::from),
            ),
        };
        self.slot.start(cx.spawn(move |job| {
            let text = match &source {
                Source::Code(code) => code.clone(),
                Source::File(p) => {
                    let len = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.len();
                    if len > MAX_SOURCE_BYTES {
                        return Err(format!("{}: {len} bytes is too large for a script", p.display()));
                    }
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?
                }
            };
            let load = Load { key, generation, source: &text, label: &label, folder: folder.as_deref() };
            let loaded =
                penumbra_script::host().load(&load, &|| job.cancelled()).map_err(|e| e.to_string())?;
            if let Some(p) = loaded.params.iter().find(|p| FIXED.contains(&p.name.as_str())) {
                return Err(format!("PARAMS: `{}` is a param of the node itself; pick another name", p.name));
            }
            if !loaded.printed.is_empty() {
                log::info!("{label}: {}", loaded.printed.trim_end());
            }
            Ok(loaded)
        }));
    }

    /// The values of the script's declared params, checked against its declaration.
    fn params(cx: &CookContext<'_>, current: &Loaded) -> BTreeMap<String, Value> {
        for name in cx.def().params.keys() {
            let name = name.as_str();
            if !FIXED.contains(&name) && !current.params.iter().any(|p| p.name == name) {
                cx.warn(format!("the script declares no param `{name}`"));
            }
        }
        current
            .params
            .iter()
            .map(|spec| {
                let value = match cx.param(&spec.name).map(|v| spec.check(v)) {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => {
                        cx.warn(format!("{e}; using the default"));
                        spec.default.clone()
                    }
                    None => spec.default.clone(),
                };
                (spec.name.clone(), value)
            })
            .collect()
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        penumbra_script::host().forget(self.key);
    }
}

impl Operator for Script {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let host = penumbra_script::host();
        if let Err(e) = host.install() {
            return Err(CookError::failed(format!("Python is not available: {e}")));
        }
        if self.slot.poll() {
            self.current = self.slot.latest().cloned();
            self.stopped = None;
        }
        let file = cx.text("file")?.trim();
        let source = if file.is_empty() {
            Source::Code(cx.text("code")?.to_string())
        } else {
            Source::File(cx.resolve_path(file)?)
        };
        let changed_on_disk = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if self.source.as_ref() != Some(&source) || changed_on_disk {
            self.watch = match &source {
                Source::File(p) if self.watch.as_ref().is_some_and(|w| w.path() == p) => self.watch.take(),
                Source::File(p) => Some(cx.watch_file(p)),
                Source::Code(_) => None,
            };
            self.start_load(cx, &source);
            self.source = Some(source);
        }

        let Some(current) = &self.current else {
            return Err(match self.slot.error() {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = self.slot.error() {
            cx.warn(format!("{e}\n(running the last version that loaded)"));
        }
        let timeout = cx.float("timeout")?;
        if let Some((message, at)) = &self.stopped {
            if *at == timeout {
                // A new version on its way gets its chance once loaded.
                return Err(if self.slot.busy() {
                    CookError::Pending
                } else {
                    CookError::failed(message.clone())
                });
            }
            self.stopped = None;
        }

        let params = Self::params(cx, current);
        self.every_frame = match cx.text("cook")? {
            "every frame" => Some(true),
            "on change" => Some(false),
            _ => None,
        };
        // The time goes to a script that cooks every frame or reads it; one that reads it but
        // cooks on change sees the time of its last cook.
        let time = (current.uses_time || self.every_frame == Some(true)).then(|| {
            let t = cx.time();
            (t.seconds(), t.frame)
        });
        let data = cx.text("values")? == "data";
        if !self.read_pictures(cx, data)? {
            // The readback's waker brings the node back.
            return if self.made { Ok(Cooked::Unchanged) } else { Err(CookError::Pending) };
        }
        let call = Call {
            key: self.key,
            time,
            rate: cx.rate().as_f64(),
            params: &params,
            chans: vec![cx.input_as::<Chans>(0), cx.input_as::<Chans>(1)],
            tables: vec![cx.input_as::<Table>(2), cx.input_as::<Table>(3)],
            pictures: self.pictures.iter().map(Option::as_ref).collect(),
            want_picture: cx.output_wanted(PICTURE),
            want_mesh: cx.output_wanted(MESH),
        };
        let waker = cx.waker();
        let name = cx.def().name.to_string();
        match host.call(&name, &call, Duration::from_secs_f64(timeout), Box::new(move || waker.wake())) {
            Ok(out) => {
                for w in out.warnings {
                    cx.warn(w);
                }
                if !out.printed.is_empty() {
                    log::info!("{name}: {}", out.printed.trim_end());
                }
                let picture = self.picture_output(cx, out.picture, data)?;
                let mesh = self.mesh_output(cx, out.mesh)?;
                self.made = true;
                Ok(Cooked::Outputs(vec![
                    Data::new(DataKind::Chan, out.chans),
                    Data::new(DataKind::Table, out.table),
                    picture,
                    mesh,
                ]))
            }
            // Something else runs in Python right now; the waker brings this node back.
            Err(ScriptError::Busy) => Err(CookError::Pending),
            Err(e @ (ScriptError::Timeout(_) | ScriptError::Hung(_))) => {
                let message = format!("{e}; the node stays stopped until the script or `timeout` changes");
                self.stopped = Some((message.clone(), timeout));
                Err(CookError::failed(message))
            }
            Err(e) => Err(CookError::failed(e.to_string())),
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn gpu_bytes(&self) -> u64 {
        let tex = self.ring.bytes() + self.blank.as_ref().map_or(0, Tex::bytes);
        tex + self.pool.bytes() + self.mesh.as_ref().map_or(0, |(_, m)| m.bytes())
    }

    fn trim(&mut self) {
        self.ring.trim();
    }

    fn time_dependent(&self) -> Option<bool> {
        self.every_frame.or_else(|| self.current.as_ref().map(|c| c.uses_time))
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        self.current.as_ref().map(|c| c.params.clone())
    }
}

/// Readback `index` as a script sees it: float32 RGBA, linear with straight alpha (colour where
/// alpha is 0 is lost), or as stored for `data`.
fn picture_of(read: &PendingRead, index: usize, data: bool) -> Result<InPicture, String> {
    let (w, h) = read.size(index).ok_or("nothing was read back")?;
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    let mut floats = vec![0f32; w as usize * 4];
    let mut halves = Vec::with_capacity(w as usize * 4);
    read.rows(index, |row: &[u8]| -> Result<(), String> {
        halves.clear();
        halves.extend(row.as_chunks::<2>().0.iter().map(|b| f16::from_le_bytes(*b)));
        floats.resize(halves.len(), 0.0);
        halves.convert_to_f32_slice(&mut floats);
        if !data {
            for px in floats.as_chunks_mut::<4>().0 {
                let a = px[3];
                let inv = if a > 0.0 { 1.0 / a } else { 0.0 };
                for c in px.iter_mut().take(3) {
                    *c *= inv;
                }
            }
        }
        rgba.extend_from_slice(&floats);
        Ok(())
    })
    .map_err(|e| match e {
        penumbra_gpu::RowsError::Gpu(e) => e.to_string(),
        penumbra_gpu::RowsError::Sink(e) => e,
    })?;
    if rgba.len() != w as usize * h as usize * 4 {
        return Err("the readback has the wrong size".into());
    }
    Ok(InPicture { width: w, height: h, rgba })
}

/// A script's picture as the texture's bytes: little-endian half floats, RGBA, linear and
/// premultiplied (as stored for `data`). Grey fills red, green and blue; a missing alpha is 1.
fn rgba16f(p: &OutPicture, data: bool) -> Vec<u8> {
    if let Samples::U8(s) = &p.samples
        && let Some(bytes) = rgba16f_u8(s, p.channels, data)
    {
        return bytes;
    }
    let pixels = p.width as usize * p.height as usize;
    let mut floats = vec![0f32; pixels * 4];
    let out = floats.as_chunks_mut::<4>().0;
    let colour: [f32; 256] = std::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        if data { v } else { penumbra_gpu::readback::srgb_to_linear(v) }
    });
    // Alpha is never sRGB-encoded.
    let alpha: [f32; 256] = std::array::from_fn(|i| i as f32 / 255.0);
    let c = |v: u8| colour.get(usize::from(v)).copied().unwrap_or(0.0);
    let a = |v: u8| alpha.get(usize::from(v)).copied().unwrap_or(0.0);
    match (&p.samples, p.channels) {
        (Samples::U8(s), 4) => {
            for (px, s) in out.iter_mut().zip(s.as_chunks::<4>().0) {
                *px = [c(s[0]), c(s[1]), c(s[2]), a(s[3])];
            }
        }
        (Samples::U8(s), 3) => {
            for (px, s) in out.iter_mut().zip(s.as_chunks::<3>().0) {
                *px = [c(s[0]), c(s[1]), c(s[2]), 1.0];
            }
        }
        (Samples::U8(s), _) => {
            for (px, s) in out.iter_mut().zip(s) {
                let g = c(*s);
                *px = [g, g, g, 1.0];
            }
        }
        (Samples::F32(s), 4) => {
            for (px, s) in out.iter_mut().zip(s.as_chunks::<4>().0) {
                *px = *s;
            }
        }
        (Samples::F32(s), 3) => {
            for (px, s) in out.iter_mut().zip(s.as_chunks::<3>().0) {
                *px = [s[0], s[1], s[2], 1.0];
            }
        }
        (Samples::F32(s), _) => {
            for (px, s) in out.iter_mut().zip(s) {
                *px = [*s, *s, *s, 1.0];
            }
        }
    }
    if !data {
        for px in out.iter_mut() {
            let a = px[3].clamp(0.0, 1.0);
            *px = [px[0] * a, px[1] * a, px[2] * a, a];
        }
    }
    let mut halves = vec![f16::ZERO; floats.len()];
    halves.convert_from_f32_slice(&floats);
    let mut bytes = vec![0u8; halves.len() * 2];
    for (b, h) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(&halves) {
        *b = h.to_le_bytes();
    }
    bytes
}

/// [`rgba16f`] for 8-bit samples without transparency (or as data): half floats looked up
/// per sample. `None` when some pixel is transparent, which needs the arithmetic.
fn rgba16f_u8(s: &[u8], channels: u8, data: bool) -> Option<Vec<u8>> {
    if channels == 4 && !data && s.as_chunks::<4>().0.iter().any(|p| p[3] != 255) {
        return None;
    }
    let half: [[u8; 2]; 256] = std::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        f16::from_f32(if data { v } else { penumbra_gpu::readback::srgb_to_linear(v) }).to_le_bytes()
    });
    // Alpha is never sRGB-encoded.
    let alpha: [[u8; 2]; 256] = std::array::from_fn(|i| f16::from_f32(i as f32 / 255.0).to_le_bytes());
    let h = |v: u8| half.get(usize::from(v)).copied().unwrap_or([0; 2]);
    let a = |v: u8| alpha.get(usize::from(v)).copied().unwrap_or([0; 2]);
    let one = f16::ONE.to_le_bytes();
    let pixel = |[r, g, b, a]: [[u8; 2]; 4]| [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]];
    let pixels = s.len() / usize::from(channels.max(1));
    let mut out = vec![0u8; pixels * 8];
    let dst = out.as_chunks_mut::<8>().0.iter_mut();
    match channels {
        4 => {
            dst.zip(s.as_chunks::<4>().0).for_each(|(o, p)| *o = pixel([h(p[0]), h(p[1]), h(p[2]), a(p[3])]))
        }
        3 => dst.zip(s.as_chunks::<3>().0).for_each(|(o, p)| *o = pixel([h(p[0]), h(p[1]), h(p[2]), one])),
        _ => dst.zip(s).for_each(|(o, g)| *o = pixel([h(*g), h(*g), h(*g), one])),
    }
    Some(out)
}

/// A script's mesh as a [`Mesh`] of one part. Normals as given; else, with indices, each
/// vertex gets the average of the triangles using it, weighted by their area (smooth where
/// vertices are shared); without indices, each triangle its own (flat).
fn mesh_of(m: OutMesh) -> Result<Mesh, Error> {
    let clean = |c: f32| if c.is_finite() { c } else { 0.0 };
    let n = m.positions.len();
    let vertices: Vec<Vertex> = (0..n)
        .map(|i| Vertex {
            position: m.positions.get(i).copied().unwrap_or_default(),
            normal: m.normals.as_ref().and_then(|v| v.get(i)).map_or([0.0; 3], |v| v.map(clean)),
            uv: m.uvs.as_ref().and_then(|v| v.get(i)).map_or([0.0; 2], |v| v.map(clean)),
        })
        .collect();
    let indices: Vec<u32> = match m.indices {
        Some(t) => t.into_iter().flatten().collect(),
        None => (0..u32::try_from(n).map_err(|_| Error::new("too many vertices"))?).collect(),
    };
    let count = u32::try_from(indices.len()).map_err(|_| Error::new("too many triangles"))?;
    let mut mesh = Mesh {
        vertices,
        indices,
        ranges: vec![Range { first: 0, count, part: 0, material: 0 }],
        parts: vec!["script".into()],
        materials: vec![Material::plain("default", [1.0; 4])],
        ..Mesh::default()
    };
    if m.normals.is_none() {
        // The same sum works for both cases: without indices, no vertex is shared.
        let mut sums = vec![[0f32; 3]; n];
        for t in mesh.indices.as_chunks::<3>().0 {
            let [p0, p1, p2] = t.map(|i| mesh.vertices.get(i as usize).map_or([0.0; 3], |v| v.position));
            let (u, v) = (sub(p1, p0), sub(p2, p0));
            // Twice the triangle's area long: larger triangles weigh more.
            let face = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            for i in t {
                if let Some(s) = sums.get_mut(*i as usize) {
                    *s = [s[0] + face[0], s[1] + face[1], s[2] + face[2]];
                }
            }
        }
        for (v, s) in mesh.vertices.iter_mut().zip(&sums) {
            v.normal = penumbra_mesh::normalize(*s);
        }
    }
    mesh.check()?;
    Ok(mesh)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[cfg(test)]
mod tests {
    use crate::builtin_types;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use penumbra_core::{Chans, NodePath, ParamPath, Rate, ShowClock, Table, Value};
    use penumbra_graph::{Cooker, JobPool, JobStatus, Network, Op, Port};

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    fn connect(from: &str, to: &str, input: u32) -> Op {
        Op::Connect { from: from.parse().unwrap(), output: 0, to: to.parse().unwrap(), input }
    }

    struct F {
        types: Arc<penumbra_graph::NodeTypes>,
        net: Arc<Network>,
        cooker: Cooker,
        clock: ShowClock,
    }

    impl F {
        fn new(ops: &[Op]) -> Self {
            let types = Arc::new(builtin_types().unwrap());
            let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
            let cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(2).unwrap()));
            Self { types, net, cooker, clock: ShowClock::new(Rate::FPS_60, Default::default()) }
        }

        fn apply(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        }

        /// Cooks `/s` until it has a new result or an error.
        fn cook(&mut self) -> Result<(Chans, Table), String> {
            let id = self.net.resolve(&"/s".parse().unwrap()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                self.cooker.cook(&self.net, self.clock.now(), &[id], &[]);
                let status = self.cooker.status(id).unwrap();
                if let Some(e) = status.error {
                    return Err(e);
                }
                let loading = matches!(status.job, Some(JobStatus::Queued | JobStatus::Running { .. }));
                if !loading
                    && let (Some(c), Some(t)) = (
                        self.cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Chans>()),
                        self.cooker.output(Port { node: id, output: 1 }).and_then(|d| d.get::<Table>()),
                    )
                {
                    return Ok((c.clone(), t.clone()));
                }
                assert!(Instant::now() < deadline, "no result");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn warning(&self) -> Option<String> {
            let id = self.net.resolve(&"/s".parse().unwrap()).unwrap();
            self.cooker.status(id).unwrap().warning
        }
    }

    #[test]
    fn script_node_end_to_end() {
        if penumbra_script::host().install().is_err() {
            eprintln!("no Python, skipped");
            return;
        }
        // The default code: input times gain.
        let mut f = F::new(&[
            create("/c", "chan.constant"),
            set("/c.value0", 0.5),
            create("/s", "script.python"),
            connect("/c", "/s", 0),
        ]);
        let (c, _) = f.cook().unwrap();
        assert_eq!(c.value(Some("value")), Some(0.5));
        // A declared param, set like any other.
        f.apply(&[set("/s.gain", 2.0)]);
        assert_eq!(f.cook().unwrap().0.value(None), Some(1.0));
        // Unknown params warn.
        f.apply(&[set("/s.nope", 1.0)]);
        f.cook().unwrap();
        assert!(f.warning().unwrap().contains("declares no param `nope`"));

        // Time, tables, a file with hot reload.
        let dir = penumbra_core::scratch::ScratchDir::new("script").unwrap();
        let file = dir.join("logic.py");
        std::fs::write(&file, "def cook(cx):\n    cx.out['f'] = cx.frame\n    cx.table.append(['a', 1])\n")
            .unwrap();
        let file_s = file.to_string_lossy().to_string();
        let mut f = F::new(&[create("/s", "script.python"), set("/s.file", file_s.as_str())]);
        f.clock.advance(10);
        let (c, t) = f.cook().unwrap();
        assert_eq!(c.value(Some("f")), Some(10.0));
        assert_eq!(t.rows(), [vec!["a".to_string(), "1".into()]]);
        f.clock.advance(5);
        assert_eq!(f.cook().unwrap().0.value(None), Some(15.0), "time-dependent: recooks as time moves");

        // `cook`: a script that counts its calls runs every frame only when told to.
        let counter = "n = 0\ndef cook(cx):\n    global n\n    n += 1\n    cx.out['n'] = n\n";
        let mut f = F::new(&[create("/s", "script.python"), set("/s.code", counter)]);
        assert_eq!(f.cook().unwrap().0.value(None), Some(1.0));
        f.clock.advance(1);
        assert_eq!(f.cook().unwrap().0.value(None), Some(1.0), "auto: no time in the code, no recook");
        f.apply(&[set("/s.cook", "every frame")]);
        let first = f.cook().unwrap().0.value(None).unwrap();
        f.clock.advance(1);
        assert_eq!(f.cook().unwrap().0.value(None), Some(first + 1.0), "every frame");
        let ticking = "def cook(cx):\n    cx.out['f'] = cx.frame\n";
        f.apply(&[set("/s.code", ticking), set("/s.cook", "on change")]);
        let at = f.cook().unwrap().0.value(None);
        f.clock.advance(3);
        assert_eq!(f.cook().unwrap().0.value(None), at, "on change: the time alone does not recook");

        // A runaway: stopped, and stays stopped (no 100 ms per frame) until the code changes.
        let mut f =
            F::new(&[create("/s", "script.python"), set("/s.code", "def cook(cx):\n    while True: pass\n")]);
        let e = f.cook().unwrap_err();
        assert!(e.contains("stopped after 100 ms"), "{e}");
        let start = Instant::now();
        f.clock.advance(1);
        assert!(f.cook().unwrap_err().contains("stays stopped"));
        assert!(start.elapsed() < Duration::from_millis(50));
        f.apply(&[set("/s.code", "def cook(cx):\n    cx.out['ok'] = 1\n")]);
        assert_eq!(f.cook().unwrap().0.value(None), Some(1.0));

        // A syntax error keeps the last good version running, with a warning.
        f.apply(&[set("/s.code", "def cook(cx)\n")]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.warning().is_none_or(|w| !w.contains("SyntaxError")) {
            assert!(Instant::now() < deadline);
            f.cook().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(f.warning().unwrap().contains("running the last version"));
    }

    fn halves(bytes: &[u8]) -> Vec<f32> {
        bytes.as_chunks::<2>().0.iter().map(|b| half::f16::from_le_bytes(*b).to_f32()).collect()
    }

    #[test]
    fn pictures_become_linear_premultiplied_half_floats() {
        use super::rgba16f;
        use penumbra_script::{OutPicture, Samples};
        let pic = |channels, samples| OutPicture { width: 2, height: 1, channels, samples };
        let near =
            |a: &[f32], b: &[f32]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3);
        // Opaque 8-bit (the lookup path): sRGB 128 is linear 0.216.
        let v = halves(&rgba16f(&pic(4, Samples::U8(vec![128, 0, 255, 255, 0, 0, 0, 255])), false));
        assert!(near(&v, &[0.216, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0]), "{v:?}");
        // Transparent 8-bit: premultiplied; alpha is not sRGB.
        let v = halves(&rgba16f(&pic(4, Samples::U8(vec![255, 255, 255, 51, 255, 0, 0, 0])), false));
        assert!(near(&v, &[0.2, 0.2, 0.2, 0.2, 0.0, 0.0, 0.0, 0.0]), "{v:?}");
        // Data: as stored, also where alpha is 0.
        let v = halves(&rgba16f(&pic(4, Samples::U8(vec![128, 0, 0, 0, 255, 0, 0, 0])), true));
        assert!(near(&v, &[0.502, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]), "{v:?}");
        // Grey and RGB fill in; floats are linear, alpha clamped to 0..1 for colour.
        let v = halves(&rgba16f(&pic(1, Samples::U8(vec![255, 0])), false));
        assert!(near(&v, &[1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0]), "{v:?}");
        let v = halves(&rgba16f(&pic(3, Samples::F32(vec![0.5, 2.0, 0.0, 0.25, 0.0, 0.0])), false));
        assert!(near(&v, &[0.5, 2.0, 0.0, 1.0, 0.25, 0.0, 0.0, 1.0]), "{v:?}");
        let v = halves(&rgba16f(&pic(4, Samples::F32(vec![1.0, 1.0, 1.0, 2.0, 1.0, 0.0, 0.0, 0.5])), false));
        assert!(near(&v, &[1.0, 1.0, 1.0, 1.0, 0.5, 0.0, 0.0, 0.5]), "{v:?}");
    }

    #[test]
    fn meshes_get_normals() {
        use super::mesh_of;
        use penumbra_script::OutMesh;
        // Two triangles folded along a shared edge: the shared vertices get the average.
        let positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let m = mesh_of(OutMesh {
            positions: positions.clone(),
            indices: Some(vec![[0, 1, 2], [0, 3, 1]]),
            ..OutMesh::default()
        })
        .unwrap();
        let n0 = m.vertices[0].normal;
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!((n0[1] - s).abs() < 1e-5 && (n0[2] - s).abs() < 1e-5, "{n0:?}");
        assert_eq!(m.vertices[2].normal, [0.0, 0.0, 1.0]);
        assert_eq!((m.triangle_count(), m.ranges.len(), m.parts.len()), (2, 1, 1));
        // Without indices every three vertices are a flat triangle.
        let m = mesh_of(OutMesh { positions: positions[..3].to_vec(), ..OutMesh::default() }).unwrap();
        assert!(m.vertices.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
        // Given normals stay (non-finite ones become 0).
        let m = mesh_of(OutMesh {
            positions: positions[..3].to_vec(),
            normals: Some(vec![[1.0, 0.0, 0.0], [f32::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]]),
            uvs: Some(vec![[0.5, 0.5]; 3]),
            ..OutMesh::default()
        })
        .unwrap();
        assert_eq!(m.vertices[1].normal, [0.0; 3]);
        assert_eq!(m.vertices[2].uv, [0.5, 0.5]);
    }

    #[test]
    fn script_node_pictures_and_meshes() {
        use penumbra_gpu::{AdapterChoice, Geo, Gpu, Tex};
        if penumbra_script::host().install().is_err() {
            eprintln!("no Python, skipped");
            return;
        }
        let gpu = match Gpu::headless(AdapterChoice::Software) {
            Ok(g) => g,
            Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
            Err(_) => return,
        };
        let code = r#"
import numpy as np
def cook(cx):
    p = cx.pictures[0]
    img = np.zeros((4, 8, 3), np.uint8)
    img[..., 0] = 255
    if p is not None:
        img[..., 1] = int(p[0, 0, 1] * 255 + 0.5)
        cx.out["h"] = p.shape[0]
    cx.picture = img
    cx.mesh = dict(positions=[[0, 0, 0], [1, 0, 0], [0, 1, 0]])
"#;
        let mut f = F::new(&[
            create("/c", "tex.constant"),
            set("/c.width", 5i64),
            set("/c.height", 3i64),
            set("/c.color", Value::Vec4([0.0, 1.0, 0.0, 1.0])),
            create("/s", "script.python"),
            set("/s.code", code),
            set("/s.values", "data"),
            Op::Connect { from: "/c".parse().unwrap(), output: 0, to: "/s".parse().unwrap(), input: 4 },
        ]);
        f.cooker.provide(gpu.clone());
        if let Err(e) = f.cook() {
            assert!(e.contains("pip install numpy"), "{e}");
            eprintln!("no numpy, skipped: {e}");
            return;
        }
        let id = f.net.resolve(&"/s".parse().unwrap()).unwrap();
        let shown = [Port { node: id, output: 2 }, Port { node: id, output: 3 }];
        let deadline = Instant::now() + Duration::from_secs(20);
        let (tex, geo) = loop {
            f.clock.advance(1);
            f.cooker.cook_showing(&f.net, f.clock.now(), &[id], &[], &shown);
            let out = |i| f.cooker.output(Port { node: id, output: i }).cloned();
            if let (Some(t), Some(g)) = (out(2), out(3))
                && let (Some(t), Some(g)) = (t.get::<Tex>(), g.get::<Geo>())
                && !g.items.is_empty()
            {
                break (t.clone(), g.clone());
            }
            assert!(Instant::now() < deadline, "no picture: {:?}", f.cooker.status(id).unwrap().error);
            std::thread::sleep(Duration::from_millis(5));
        };
        let chans = f.cooker.output(Port { node: id, output: 0 }).unwrap().get::<Chans>().unwrap().clone();
        assert_eq!(chans.value(Some("h")), Some(3.0), "the input picture arrived");
        assert_eq!(tex.size(), (8, 4));
        let px = gpu.read(&tex).unwrap().get(0, 0).unwrap();
        assert!(
            (px[0] - 1.0).abs() < 1e-3 && (px[1] - 1.0).abs() < 1e-3 && px[2] == 0.0 && px[3] == 1.0,
            "{px:?}"
        );
        let mesh = &geo.items[0].model.mesh;
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.vertices[0].normal, [0.0, 0.0, 1.0], "a triangle's own normal");

        // Colour: uint8 is sRGB, so 255 stays 1 and 0 stays 0, and a grey of 128 is about 0.216.
        let grey = "import numpy as np\ndef cook(cx):\n    cx.picture = np.full((2, 2), 128, np.uint8)\n";
        f.apply(&[set("/s.code", grey), set("/s.values", "color")]);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            f.clock.advance(1);
            f.cooker.cook_showing(&f.net, f.clock.now(), &[id], &[], &shown);
            let t = f.cooker.output(Port { node: id, output: 2 }).and_then(|d| d.get::<Tex>().cloned());
            if let Some(t) = t
                && t.size() == (2, 2)
            {
                let px = gpu.read(&t).unwrap().get(1, 1).unwrap();
                assert!((px[0] - 0.216).abs() < 2e-3 && px[3] == 1.0, "{px:?}");
                break;
            }
            assert!(Instant::now() < deadline, "no grey: {:?}", f.cooker.status(id).unwrap().error);
            std::thread::sleep(Duration::from_millis(5));
        }
        // The mesh output holds the last mesh: this script sets none.
        let held =
            f.cooker.output(Port { node: id, output: 3 }).and_then(|d| d.get::<Geo>().cloned()).unwrap();
        assert_eq!(held.items.len(), 1);

        // Errors from malformed arrays are node errors.
        let bad = "import numpy as np\ndef cook(cx):\n    cx.mesh = dict(positions=np.zeros((3, 3)), indices=[[0, 1, 7]])\n";
        f.apply(&[set("/s.code", bad)]);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            f.clock.advance(1);
            f.cooker.cook_showing(&f.net, f.clock.now(), &[id], &[], &shown);
            if let Some(e) = f.cooker.status(id).unwrap().error {
                assert!(e.contains("index 7 is past the last vertex"), "{e}");
                break;
            }
            assert!(Instant::now() < deadline, "no error");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
