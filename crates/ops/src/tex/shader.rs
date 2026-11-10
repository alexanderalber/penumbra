//! `tex.shader`: a WGSL file in the project, hot reloaded. Its header declares parameters and
//! pixel density ([`penumbra_gpu::header`]); the conventions it can use (inputs, time,
//! samplers, noise) are in [`penumbra_gpu::pass`].
//!
//! Reading and compiling run off the cook thread, so saving a shader during a show costs the
//! show nothing. A version that does not compile leaves the last good one running, with the
//! compiler's message as the node's warning.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::header::{self, Header};
use penumbra_gpu::{FLOAT_FORMAT, FORMAT, Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, Density, FileWatch, FnNodeType, JobStatus,
    NodeDef, NodeDesc, NodeTypes, Operator,
};

/// Largest shader file read.
const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// Parameters every shader node has; the header adds its own.
const FIXED: &[&str] =
    &["file", "resolution", "width", "height", super::KEEP_SIZE, "iterations", "format", "bypass"];

/// Most passes per cook. A cheap simulation step costs roughly 0.2 ms per pass at 1920 x 1080
/// on a desktop GPU, so 64 passes still fit a 60 fps frame there; a bound value gone wild cannot
/// stall the show for longer than that.
const MAX_ITERATIONS: i64 = 64;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.shader",
            "An effect or generator of your own, written as a WGSL shader in a file; reloads when saved, its `// @param` lines become params",
        )
        .param(
            ParamSpec::file("file", &["wgsl"])
                .doc("The WGSL file, relative to the project directory; saving it reloads the node"),
        )
        .param(
            ParamSpec::choice("resolution", &["input", "custom"])
                .doc("Where the output's size comes from")
                .choice_doc("input", "in0's size; width and height while in0 is unconnected")
                .choice_doc("custom", "width and height, whatever is wired"),
        )
        .param(super::width_param().doc(
            "Width of the output in pixels, used with resolution custom or without in0. res.optimize may change it unless keep_size is on",
        ))
        .param(super::height_param().doc(
            "Height of the output in pixels, used with resolution custom or without in0. res.optimize may change it unless keep_size is on",
        ))
        .param(super::keep_size())
        .param(
            ParamSpec::int("iterations", 1)
                .clamp(1.0, MAX_ITERATIONS as f64)
                .range(1.0, 16.0)
                .more()
                .doc(
                    "How often the shader runs per cook, TouchDesigner's Passes. From the second pass on, in0 is the pass before. The shader reads its pass as pn.iteration, from 0. For simulations that take several steps a frame. In a loop that a tex.feedback's `repeat` runs several times, pn.iteration counts on over the runs",
                ),
        )
        .param(
            ParamSpec::bool("bypass", false)
                .more()
                .doc(
                    "While on, the output is in0 as it is and no pass runs, TouchDesigner's Bypass flag. Bind a simulation's pause to it: a step whose in0 is its own last frame then holds the picture and costs nothing. While in0 is unwired the shader runs as usual",
                ),
        )
        .param(
            ParamSpec::choice("format", &["16f", "32f"])
                .more()
                .doc("Number format of the output, TouchDesigner's Pixel Format. Use 32f for simulation state that 16f rounds away. 32f takes twice the memory")
                .choice_doc("16f", "half floats, about 3 decimal digits, like every other picture")
                .choice_doc(
                    "32f",
                    "32-bit floats, about 7 decimal digits, in every pass. A GPU that cannot filter 32-bit floats gets 16f and a warning",
                ),
        )
        .input_run_of(
            "in",
            DataKind::Tex,
            penumbra_gpu::pass::INPUTS,
            1,
            "The pictures the shader reads as in0 to in7. An unwired one reads as transparent black. An input the shader reads stays shown while nothing is wired there",
        )
        .output("out", DataKind::Tex)
        .extra_params(),
        || Box::new(Shader::default()) as Box<dyn Operator>,
    ))
}

#[derive(Clone)]
struct Compiled {
    pass: Arc<Pass>,
    header: Header,
}

#[derive(Default)]
struct Shader {
    slot: AsyncSlot<Compiled>,
    current: Option<Compiled>,
    watch: Option<FileWatch>,
    ring: TexRing,
    /// The passes before the last when `iterations` is above 1.
    middle: TexRing,
    /// Whether the compile started last renders 32-bit floats.
    asked: Option<bool>,
    palettes: crate::palette::Palettes,
}

impl Operator for Shader {
    fn gpu_bytes(&self) -> u64 {
        self.ring.bytes() + self.middle.bytes()
    }

    fn trim(&mut self) {
        self.ring.trim();
        self.middle.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        if self.slot.poll() {
            self.current = self.slot.latest().cloned();
        }
        let gpu = super::gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let float = wants_float(cx, gpu)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        // Only a new file name, a change on disk or another format recompiles; params, inputs
        // and time do not. The format is part of the pipeline.
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed || self.asked != Some(float) {
            self.asked = Some(float);
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let gpu = gpu.clone();
            let label =
                path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| {
                let len = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?.len();
                if len > MAX_SOURCE_BYTES {
                    return Err(format!("{}: {len} bytes is too large for a shader", p.display()));
                }
                let source = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let header = header::parse(&source).map_err(|e| format!("{label}: {e}"))?;
                // Such a param would silently take the node's own value.
                if let Some(p) = header.params.iter().find(|p| FIXED.contains(&p.name.as_str())) {
                    return Err(format!(
                        "{label}: `{}` is a param of the node itself; give the shader's param another name. The node's own params: {}",
                        p.name,
                        FIXED.join(", ")
                    ));
                }
                let format = if float { FLOAT_FORMAT } else { FORMAT };
                let pass = Pass::compile_in(&gpu, &label, &source, &header.params, format).map_err(|e| e.0)?;
                Ok(Compiled { pass: Arc::new(pass), header })
            }));
        }

        // A shader that never compiled fails, bypassed or not: a paused simulation with a broken
        // step shows the error, and so does the component around it. This comes before `bypass`
        // is read, so a shader that declares `bypass` itself gets the error naming the clash.
        if self.current.is_none()
            && let Some(e) = self.slot.error()
        {
            return Err(CookError::failed(e));
        }

        // Bypassed: in0 goes out unchanged. The compile above still starts, so the shader is
        // ready when bypass goes off.
        if cx.bool("bypass")?
            && let Some(t) = cx.input_as::<Tex>(0)
        {
            if let Some(e) = self.slot.error() {
                cx.warn(format!("{e}\n(bypassed; the last version that compiled runs when bypass goes off)"));
            }
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, t.clone())]));
        }

        let Some(current) = &self.current else { return Err(CookError::Pending) };
        if let Some(e) = self.slot.error() {
            cx.warn(format!("{e}\n(showing the last version that compiled)"));
        }
        for name in cx.def().params.keys() {
            let name = name.as_str();
            if FIXED.contains(&name) {
                continue;
            }
            match current.header.params.iter().find(|p| p.name == name) {
                None => cx.warn(format!("the shader declares no param `{name}`")),
                Some(spec) => {
                    if let Some(Err(e)) = cx.param(name).map(|v| spec.check(v)) {
                        cx.warn(format!("{e}; using the default"));
                    }
                }
            }
        }

        let inputs = super::inputs(cx);
        let from_input = cx.text("resolution")? == "input";
        let in0_connected = cx.def().inputs.first().is_some_and(Option::is_some);
        let (w, h) = match inputs.first().copied().flatten() {
            Some(t) if from_input => t.size(),
            // Connected, but upstream has nothing yet: wait rather than flash the custom size.
            None if from_input && in0_connected => return Err(CookError::Pending),
            _ => (super::side(cx, "width")?, super::side(cx, "height")?),
        };
        let (seconds, frame) = if current.pass.uses_time() {
            let t = cx.time();
            (t.seconds(), t.frame)
        } else {
            (0.0, 0)
        };
        // 0 and below run once, like 1.
        let iterations = u32::try_from(cx.int("iterations")?.clamp(1, MAX_ITERATIONS)).unwrap_or(1);
        // In a loop that runs several times per frame, the passes count on over the runs.
        let (run, runs) = cx.run();
        let palettes = self.palettes.pass_values(cx, &current.header.params);
        let param = |name: &str| palettes.get(name).cloned().or_else(|| cx.param(name).cloned());
        // Every pass renders at the output's size, so from the second pass on in0 has that size
        // too, whatever size the wired in0 has.
        let mut previous: Option<Tex> = None;
        for i in 0..iterations {
            let last = i + 1 == iterations;
            let ring = if last { &mut self.ring } else { &mut self.middle };
            // The compiled pass's format: until a new format has compiled, the old one.
            let target = ring.next_in(gpu, w, h, current.pass.format(), "tex.shader")?;
            let mut ins = inputs.clone();
            if let Some(p) = &previous {
                match ins.first_mut() {
                    Some(slot) => *slot = Some(p),
                    None => ins.push(Some(p)),
                }
            }
            let input = PassInput { inputs: &ins, seconds, frame, param: &param };
            current.pass.render_iteration(
                gpu,
                &target,
                &input,
                run.saturating_mul(iterations).saturating_add(i),
                runs.saturating_mul(iterations),
            )?;
            previous = Some(target);
        }
        if iterations == 1 {
            // Back to one pass: the in-between textures go.
            self.middle.trim();
        }
        let Some(out) = previous else { return Err(CookError::failed("the shader ran no pass")) };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, out)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.current.as_ref().is_some_and(|c| c.pass.uses_time()))
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        self.current.as_ref().map(|c| c.header.params.clone())
    }

    fn reads_inputs(&self) -> u64 {
        let Some(c) = &self.current else { return 0 };
        (0..penumbra_gpu::pass::INPUTS).filter(|i| c.pass.uses_input(*i)).fold(0, |mask, i| mask | 1 << i)
    }

    fn density(&self, _def: &NodeDef, _desc: &NodeDesc, _input: usize) -> Density {
        Density::needs([self.current.as_ref().map_or(1.0, |c| c.header.density); 2])
    }

    fn sets_size(&self, def: &NodeDef, desc: &NodeDesc) -> bool {
        let custom = def.param(desc, "resolution").and_then(|v| v.as_str()) == Some("custom");
        custom || def.inputs.first().is_none_or(Option::is_none)
    }
}

/// Whether the output is to be 32-bit floats: `format` asks for them and the GPU can do it. A
/// GPU that cannot filter or render 32-bit floats gets half floats, with a warning on the node.
fn wants_float(cx: &CookContext<'_>, gpu: &Gpu) -> Result<bool, CookError> {
    if cx.text("format")? != "32f" {
        return Ok(false);
    }
    if gpu.float_targets() {
        return Ok(true);
    }
    cx.warn("format 32f: this GPU cannot filter or render 32-bit floats, so the node writes 16f");
    Ok(false)
}
