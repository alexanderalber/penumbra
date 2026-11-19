//! `pts.shader`: a program over points, written in WGSL in a file of the project and hot
//! reloaded, TouchDesigner's GLSL POP. The header declares the attributes it reads and writes
//! and its params ([`penumbra_gpu::points::program::parse_header`]); what the program sees is in
//! [`penumbra_gpu::points::program`].
//!
//! Reading and compiling run off the cook thread. A version that does not compile leaves the
//! last good one running, with the compiler's message as the node's warning.
//!
//! A `// @neighbors <radius>` line makes the program walk the points near each point: the node
//! builds a grid over its input every cook ([`NearGrid`]).

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Tex;
use penumbra_gpu::points::near::NearGrid;
use penumbra_gpu::points::program::{self, Header, Radius, Shape, Spec};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use super::program::{Compiling, input};
use crate::gpu;

pub const KIND: &str = "pts.shader";

/// Largest program file read.
const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// Params of the node itself; the header adds its own.
const FIXED: &[&str] = &["file"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "A program of your own that runs once per point, written in WGSL in a file; reloads when saved. Its header names the attributes it reads and writes, and its params",
        )
        .param(
            ParamSpec::file("file", &["wgsl"])
                .doc("The WGSL file, relative to the project directory; saving it reloads the node. `// @write position` lines name the attributes, `fn main()` runs once per point. `// @neighbors 0.2` lets it walk the points within 0.2 m of each point"),
        )
        .input("in", DataKind::Pts)
        .input("pic0", DataKind::Tex)
        .input("pic1", DataKind::Tex)
        .describe_input("in", "The points the program runs over")
        .describe_input("pic0", "A picture the program samples as pic0, with textureSampleLevel(pic0, pn_clamp, uv, 0.0). Unwired it reads as transparent black")
        .describe_input("pic1", "A second picture, pic1")
        .output("out", DataKind::Pts)
        .describe_output("out", "The points with the attributes the program writes; the others pass on unchanged")
        .extra_params(),
        || Box::new(Shader::default()) as Box<dyn Operator>,
    ))
}

/// A file read and its header parsed.
#[derive(Clone)]
struct Parsed {
    label: String,
    source: String,
    header: Header,
}

#[derive(Default)]
struct Shader {
    file: AsyncSlot<Parsed>,
    watch: Option<FileWatch>,
    compiling: Compiling,
    /// The grid of near points, for a program with `@neighbors`.
    grid: NearGrid,
}

impl Operator for Shader {
    fn gpu_bytes(&self) -> u64 {
        self.compiling.bytes() + self.grid.bytes()
    }

    fn trim(&mut self) {
        self.compiling.trim();
        self.grid.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.file.poll();
        self.compiling.poll();
        let gpu = gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let label =
                path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
            let p = path.clone();
            self.file.start(cx.spawn(move |_job| {
                let len = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?.len();
                if len > MAX_SOURCE_BYTES {
                    return Err(format!("{}: {len} bytes is too large for a program", p.display()));
                }
                let source = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let header = program::parse_header(&source).map_err(|e| format!("{label}: {e}"))?;
                if let Some(p) = header.params.iter().find(|p| FIXED.contains(&p.name.as_str())) {
                    return Err(format!(
                        "{label}: `{}` is a param of the node itself; give the program's param another name",
                        p.name
                    ));
                }
                Ok(Parsed { label, source, header })
            }));
        }
        if cx.exact_frames() {
            super::program::finish(&mut self.file);
        }

        let Some(parsed) = self.file.latest().cloned() else {
            return Err(match self.file.error() {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        let points = input(cx)?;
        let spec = Spec {
            label: parsed.label.clone(),
            source: parsed.source.clone(),
            decls: parsed.header.decls.clone(),
            params: parsed.header.params.clone(),
        };
        let near = parsed.header.near.as_ref();
        let made =
            program::generate_near(&spec, &Shape::of(points), program::storage_budget(gpu), near.is_some());
        let made_error = match made {
            Ok(g) => {
                self.compiling.want(cx, gpu, g);
                None
            }
            Err(e) => Some(e.0),
        };
        // The newest problem: the file, then the program made of it, then the compiler.
        let problem = self
            .file
            .error()
            .map(str::to_string)
            .or(made_error)
            .or(self.compiling.error().map(str::to_string));
        let Some(program) = self.compiling.current(points) else {
            return Err(match problem {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = problem {
            cx.warn(format!("{e}\n(running the last version that compiled)"));
        }
        for name in cx.def().params.keys() {
            let name = name.as_str();
            if FIXED.contains(&name) {
                continue;
            }
            match parsed.header.params.iter().find(|p| p.name == name) {
                None => cx.warn(format!("the program declares no param `{name}`")),
                Some(spec) => {
                    if let Some(Err(e)) = cx.param(name).map(|v| spec.check(v)) {
                        cx.warn(format!("{e}; using the default"));
                    }
                }
            }
        }

        // A wired picture the program reads that is still on its way: wait for it.
        let wired = |i: usize| cx.def().inputs.get(i).is_some_and(Option::is_some);
        let pictures: Vec<Option<&Tex>> = (1..=2).map(|i| cx.input_as::<Tex>(i)).collect();
        let waiting = (0..2).any(|k| {
            pictures.get(k).is_some_and(Option::is_none)
                && wired(k + 1)
                && !cx.input_stuck(k + 1)
                && program.uses_picture(k)
        });
        if waiting {
            return Err(CookError::Pending);
        }
        let param = |name: &str| cx.param(name).cloned();
        // The grid the program walks, built over the points it runs on.
        let grid = match near.filter(|_| program.generated().walks_neighbors()) {
            None => None,
            Some(decl) => {
                let radius = match &decl.radius {
                    Radius::Metres(r) => f64::from(*r),
                    Radius::Param(name) => {
                        let default =
                            parsed.header.params.iter().find(|p| p.name == *name).map(|p| &p.default);
                        cx.param(name).or(default).and_then(|v| v.as_f64()).unwrap_or(0.0)
                    }
                };
                if !(radius.is_finite() && radius > 0.0) {
                    cx.warn(format!(
                        "@neighbors: the radius is {radius}, so no neighbours are found; it must be a positive number of metres"
                    ));
                }
                self.grid.build(gpu, points, radius as f32, decl.cap)?
            }
        };
        let out = self.compiling.run_near(cx, gpu, &program, points, &param, &pictures, grid.as_ref())?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(match self.file.status() {
            JobStatus::Done | JobStatus::Idle => self.compiling.status(),
            other => other,
        })
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.compiling.latest().is_some_and(|p| p.uses_time()))
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        self.file.latest().map(|p| p.header.params.clone())
    }

    fn reads_inputs(&self) -> u64 {
        let Some(p) = self.compiling.latest() else { return 0 };
        (0..2).filter(|k| p.uses_picture(*k)).fold(0, |mask, k| mask | 1 << (k + 1))
    }
}
