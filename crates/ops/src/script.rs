//! `script.python`: a Python script turning chans and tables into chans and a table. The Python
//! side lives in `penumbra-script` and `pyhost`; this is the node.
//!
//! The script comes from `code` or, when set, from `file` (hot reloaded). Loading runs in a job,
//! so a slow import costs the show nothing; a version that fails to load leaves the last good
//! one running, with the error as a warning. Calls run on the cook thread, for at most
//! `timeout`; one stopped by the watchdog stays stopped until its code or `timeout` changes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use penumbra_core::{Chans, Curve, Error, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};
use penumbra_script::{Call, Load, Loaded, ScriptError};

/// The node's own params; a script's `PARAMS` may not reuse these names.
const FIXED: &[&str] = &["file", "code", "cook", "timeout"];

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
#      cx.params (the values of PARAMS), cx.time, cx.frame, cx.rate
# Out: cx.out[name] = a number or a list, cx.table.append([cells]), cx.warn(text)

PARAMS = {"gain": (1.0, 0.0, 2.0)}

def cook(cx):
    a = cx.chans[0]
    cx.out["value"] = (a.value() if a else 0.0) * cx.params["gain"]
"#;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "script.python",
            "Logic of your own in Python: turns channels and tables into new channels and a table",
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
        .input("chan0", DataKind::Chan)
        .input("chan1", DataKind::Chan)
        .input("table0", DataKind::Table)
        .input("table1", DataKind::Table)
        .output("out", DataKind::Chan)
        .output("table", DataKind::Table)
        .describe_output("out", "The channels cook(cx) returns")
        .describe_output("table", "The table cook(cx) returns")
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
        let call = Call {
            key: self.key,
            time,
            rate: cx.rate().as_f64(),
            params: &params,
            chans: vec![cx.input_as::<Chans>(0), cx.input_as::<Chans>(1)],
            tables: vec![cx.input_as::<Table>(2), cx.input_as::<Table>(3)],
        };
        let waker = cx.waker();
        let name = cx.def().name.as_str();
        match host.call(name, &call, Duration::from_secs_f64(timeout), Box::new(move || waker.wake())) {
            Ok(out) => {
                for w in out.warnings {
                    cx.warn(w);
                }
                if !out.printed.is_empty() {
                    log::info!("{name}: {}", out.printed.trim_end());
                }
                Ok(Cooked::Outputs(vec![
                    Data::new(DataKind::Chan, out.chans),
                    Data::new(DataKind::Table, out.table),
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

    fn time_dependent(&self) -> Option<bool> {
        self.every_frame.or_else(|| self.current.as_ref().map(|c| c.uses_time))
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        self.current.as_ref().map(|c| c.params.clone())
    }
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
        let dir = std::env::temp_dir().join(format!("penumbra-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        std::fs::remove_dir_all(&dir).ok();
    }
}
