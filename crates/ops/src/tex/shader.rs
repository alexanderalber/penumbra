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
use penumbra_gpu::{Pass, PassInput, TexRing};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, Density, FileWatch, FnNodeType, JobStatus,
    NodeDef, NodeDesc, NodeTypes, Operator,
};

/// Largest shader file read.
const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// Parameters every shader node has; the header adds its own.
const FIXED: &[&str] = &["file", "resolution", "width", "height", super::KEEP_SIZE];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.shader",
            "A WGSL fragment shader from a file, hot reloaded; parameters come from its `// @param` header",
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
            "Width of the output in pixels, for resolution custom or without in0; res.optimize may change it unless keep_size is on",
        ))
        .param(super::height_param().doc(
            "Height of the output in pixels, for resolution custom or without in0; res.optimize may change it unless keep_size is on",
        ))
        .param(super::keep_size())
        .input("in0", DataKind::Tex)
        .input("in1", DataKind::Tex)
        .input("in2", DataKind::Tex)
        .input("in3", DataKind::Tex)
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
}

impl Operator for Shader {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        if self.slot.poll() {
            self.current = self.slot.latest().cloned();
        }
        let gpu = super::gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        // Only a new file name or a change on disk recompiles; params, inputs and time do not.
        if moved || self.watch.as_ref().is_some_and(FileWatch::take_changed) {
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
                        "{label}: `{}` is a param of the node itself; give the shader's another name",
                        p.name
                    ));
                }
                let pass = Pass::compile(&gpu, &label, &source, &header.params).map_err(|e| e.0)?;
                Ok(Compiled { pass: Arc::new(pass), header })
            }));
        }

        let Some(current) = &self.current else {
            return Err(match self.slot.error() {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
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
        let target = self.ring.next(gpu, w, h, "tex.shader")?;
        let param = |name: &str| cx.param(name).cloned();
        current.pass.render(gpu, &target, &PassInput { inputs: &inputs, seconds, frame, param: &param })?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
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

    fn density(&self, _def: &NodeDef, _desc: &NodeDesc, _input: usize) -> Density {
        Density::needs([self.current.as_ref().map_or(1.0, |c| c.header.density); 2])
    }

    fn sets_size(&self, def: &NodeDef, desc: &NodeDesc) -> bool {
        let custom = def.param(desc, "resolution").and_then(|v| v.as_str()) == Some("custom");
        custom || def.inputs.first().is_none_or(Option::is_none)
    }
}
