//! The engine's commands: `project.*`, `edit.*`, `history.*`, `node.*`, `chan.*`, `table.*`,
//! `cook.*`, `gpu.*`, `output.*`, `res.*`, `script.*`, `system.*`, `text.*`. The UI adds `ui.*`
//! (crate `penumbra-ui`).
//!
//! Every edit goes through the session as Claude ([`Actor::Claude`]), so it passes the gate:
//! inside an `edit.begin` / `edit.end` claim all edits form one history entry; without a claim
//! each command needs 3 seconds of quiet from the user and is its own entry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{Chans, Error, Name, NodeId, NodePath, ParamPart, ParamPath, Table, Value};
use penumbra_gpu::{Geo, Tex};
use penumbra_graph::{
    Data, EditOutcome, JobStatus, Network, NodeDesc, NodeStatus, Op, Port, PortRole, Recorded, ThumbnailMode,
};
use penumbra_ops::res::Report;
use penumbra_project::{Actor, Session};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

type Result<T> = std::result::Result<T, CommandError>;

/// A command run with the engine.
pub(crate) type EngineCommand = fn(&Engine, &Json) -> Result<Json>;

/// Registers commands that run with `engine` into `registry`: `add(id, summary, function)`.
pub(crate) fn engine_commands<'a>(
    registry: &'a mut Registry,
    engine: &'a Arc<Engine>,
) -> impl FnMut(&'static str, &'static str, EngineCommand) + 'a {
    move |id, summary, f| {
        let engine = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    }
}

/// Registers the engine's commands into `registry`.
pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "project.info",
        "Project directory, unsaved changes, file errors, nodes that did not load (broken), who is editing",
        project_info,
    );
    add(
        "project.save",
        "Write project.ron now, also over a file that failed to merge (copied to project.ron.bak first)",
        project_save,
    );
    add(
        "project.settings",
        "Project settings with their values and what they do; {set?: {name: value}}, null resets (one undo step)",
        project_settings,
    );
    add("edit.begin", "Claim editing for Claude (needs 3 s without user edits); {label?}", edit_begin);
    add("edit.end", "Give editing back to the user", edit_end);
    add("edit.status", "Who is editing", edit_status);
    add("history.list", "Recent history entries; {limit?}", history_list);
    add("history.undo", "Undo one history entry", history_undo);
    add("history.redo", "Redo one history entry", history_redo);
    add(
        "node.kinds",
        "Node types with their params, inputs and outputs; with a param, its help (what it does, type, default, range, unit, what each choice does); {kind?, param?}",
        node_kinds,
    );
    add("node.list", "Nodes under a path; {path?, recursive?}", node_list);
    add("node.info", "One node: params, connections, cook status; {path}", node_info);
    add("node.create", "Create a node; {path, kind, params?}", node_create);
    add("node.delete", "Delete a node and everything inside it; {path}", node_delete);
    add(
        "node.replace",
        "Replace a node with one of another kind under the same name, keeping the params, wires and bindings that fit (one undo step); lists what was not kept; {path, kind}",
        node_replace,
    );
    add("node.rename", "Rename a node; {path, name}", node_rename);
    add("node.set", "Set params, null resets to the default; {path, params: {name: value}}", node_set);
    add("node.connect", "Connect an output to an input; {from, output?, to, input?}", node_connect);
    add("node.disconnect", "Clear an input; {to, input}", node_disconnect);
    add("node.lock", "Lock (hold outputs) or unlock a node; {path, locked?}", node_lock);
    add(
        "node.place",
        "Move nodes in the editor (top left corner, graph units on their component's plane; one undo step); null leaves a node to the editor to place; {nodes: {path: [x, y]}}",
        node_place,
    );
    add(
        "node.bind",
        "Drive a param (or one component, /t.translate.0) with a channel; {param, from, channel?}",
        node_bind,
    );
    add("node.unbind", "Back to the param's stored value; {param}", node_unbind);
    add("node.batch", "Apply ops as one all-or-nothing batch; {ops: [{op, ...}], label?}", node_batch);
    add("node.watch", "Cook a node every frame (or stop); {path, watch?}", node_watch);
    add(
        "node.reset",
        "Start stateful nodes over (feedback, cache, time machine, tracker, filter...); a component resets the stateful nodes inside; locked nodes keep their outputs; {paths} or {path}",
        node_reset,
    );
    add(
        "node.snapshot",
        "Cook a tex node (or a geo node: its picture) and write its output as PNG (8-bit sRGB); {path, output?, file?}",
        node_snapshot,
    );
    add(
        "chan.read",
        "Cook a chan node and read its channels (values now; {samples: true} for all); {path, output?, samples?}",
        chan_read,
    );
    add(
        "table.read",
        "Cook a table node and read its rows; {path, output?, rows?} (rows: at most, default 100)",
        table_read,
    );
    add(
        "geo.info",
        "Cook a geo node and describe it: parts (id, name, triangles), bounds, counts; {path, output?}",
        geo_info,
    );
    add(
        "render.frame",
        "Move the view.camera wired into a tex.render so its geometry fills the picture, keeping the direction (orthographic: sets its size); {path}",
        render_frame,
    );
    add("cook.status", "Show clock, last cook pass, watched nodes, thumbnail policy", cook_status);
    add(
        "cook.thumbnails",
        "Global thumbnail switch and rate; {mode?: always | when_free | never, every?: cook passes between two thumbnail cooks of a node, 1 = every frame}",
        cook_thumbnails,
    );
    add("cook.play", "Run or stop the show clock; {playing?} (default true)", cook_play);
    add("gpu.info", "GPU adapter, texture size limit, memory, errors raised so far, whether lost", gpu_info);
    add(
        "gpu.simulate_loss",
        "Test aid: end the GPU device as if the driver had lost it; the app then closes as after a real loss and the next start opens paused; {confirm: true}",
        gpu_simulate_loss,
    );
    add("text.fonts", "Font families and their styles, for tex.text's font and style; {family?}", text_fonts);
    add(
        "script.info",
        "The script node's Python: where it was found (or why not), version, state, call times, timeouts",
        script_info,
    );
    add(
        "script.bench",
        "Measure a script call: GIL attach, request round trip, trivial and 200-channel calls; {runs?}",
        script_bench,
    );
    add(
        "output.list",
        "Connected displays (ids for out.projector's display), open output windows, projectors, pacing",
        output_list,
    );
    add(
        "res.analyze",
        "What the projectors need of every node they show: size, needed, verdict, suggestion; waits up to 3 s for render nodes to measure (pending: those that did not)",
        res_analyze,
    );
    add(
        "res.optimize",
        "Give the nodes the suggested sizes, one undoable entry; {paths?, dry_run?} (paths: only these)",
        res_optimize,
    );
    add(
        "res.overlay",
        "Colour the mappers' and render nodes' pictures by resolution (green 1:1, red coarse, blue wasted); {on?}",
        res_overlay,
    );
    add(
        "system.check",
        "Show-machine settings that can ruin a show (gaming overlay, notifications)",
        system_check,
    );
    add("system.fix", "Apply one check's fix; replies with what it replaced; {id}", system_fix);
    add(
        "system.restore",
        "Write back what system.fix replaced; {previous} as system.fix returned it",
        system_restore,
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}

pub(crate) fn no_args(p: &Json) -> Result<()> {
    args::<Empty>(p).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: NodePath,
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<Json> {
    serde_json::to_value(v).map_err(|e| CommandError(e.to_string()))
}

// ---------------------------------------------------------------------------------------------
// project, edit, history

fn project_info(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    let mut status = to_json(&e.session().status(Instant::now()))?;
    if let Some(obj) = status.as_object_mut() {
        obj.insert("file".into(), json!(e.session().file().display().to_string()));
    }
    Ok(status)
}

fn project_save(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    Ok(json!({"changed": e.session().save()?}))
}

fn project_settings(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        set: BTreeMap<Name, Option<Value>>,
    }
    let a: A = args(p)?;
    if !a.set.is_empty() {
        let ops: Vec<Op> = a.set.into_iter().map(|(name, value)| Op::Setting { name, value }).collect();
        edit(e, &ops, "")?;
    }
    let net = e.session().doc().snapshot();
    let settings: Vec<Json> = penumbra_graph::SETTINGS
        .iter()
        .map(|s| json!({"name": s.name, "value": net.switch(s.name), "default": s.default, "doc": s.doc}))
        .collect();
    Ok(json!({"settings": settings}))
}

fn edit_begin(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        label: String,
    }
    let a: A = args(p)?;
    let now = Instant::now();
    e.session().begin(&a.label, now)?;
    to_json(&e.session().status(now).edit)
}

fn edit_end(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    Ok(json!({"was_claimed": e.session().end(Instant::now())}))
}

fn edit_status(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    to_json(&e.session().status(Instant::now()).edit)
}

fn history_list(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default = "twenty")]
        limit: usize,
    }
    fn twenty() -> usize {
        20
    }
    let a: A = args(p)?;
    let now = std::time::SystemTime::now();
    Ok(e.session().doc().with_history(|h| {
        let applied = h.applied();
        let entries: Vec<Json> = h
            .entries()
            .iter()
            .enumerate()
            .rev()
            .take(a.limit)
            .map(|(i, entry)| {
                json!({
                    "index": i,
                    "label": entry.label,
                    "source": entry.source,
                    "applied": i < applied,
                    "nodes": entry.touched.len(),
                    "age_s": now.duration_since(entry.at).map(|d| d.as_secs_f64()).unwrap_or(0.0),
                })
            })
            .collect();
        json!({"entries": entries, "total": h.entries().len(), "applied": applied})
    }))
}

fn history_undo(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    Ok(json!({"undone": e.session().undo(Actor::Claude, Instant::now())?}))
}

fn history_redo(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    Ok(json!({"redone": e.session().redo(Actor::Claude, Instant::now())?}))
}

// ---------------------------------------------------------------------------------------------
// node: reading

fn node_kinds(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        kind: Option<String>,
        param: Option<String>,
    }
    let a: A = args(p)?;
    let types = e.types();
    if let Some(param) = &a.param {
        let kind = a.kind.as_deref().ok_or("`param` needs `kind`")?;
        let desc = types.desc(kind).ok_or_else(|| format!("unknown node kind `{kind}`"))?;
        let spec = desc.param_spec(param).ok_or_else(|| {
            let hint = if desc.extra_params {
                "; params a node declares itself (a shader's) are in node.info"
            } else {
                ""
            };
            format!("{kind} has no param `{param}`{hint}")
        })?;
        return Ok(json!({"kind": kind, "param": to_json(&spec.help())?}));
    }
    let descs: Vec<&NodeDesc> = match &a.kind {
        Some(k) => vec![types.desc(k).ok_or_else(|| format!("unknown node kind `{k}`"))?],
        None => types.kinds().filter_map(|k| types.desc(k)).collect(),
    };
    Ok(json!({"kinds": to_json(&descs)?}))
}

fn node_list(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        path: NodePath,
        #[serde(default = "yes")]
        recursive: bool,
    }
    let a: A = args(p)?;
    let net = e.session().doc().snapshot();
    let under = if a.path.is_root() { None } else { Some(require(&net, &a.path)?) };
    let mut nodes = BTreeMap::new();
    let mut stack = vec![under];
    while let Some(parent) = stack.pop() {
        for n in net.children(parent) {
            if a.recursive {
                stack.push(Some(n.id));
            }
            if let Some(path) = net.path_of(n.id) {
                nodes.insert(path, n.clone());
            }
        }
    }
    let list: Vec<Json> = nodes
        .iter()
        .map(|(path, n)| {
            let mut v = json!({"path": path, "kind": n.kind});
            if n.locked
                && let Some(o) = v.as_object_mut()
            {
                o.insert("locked".into(), json!(true));
            }
            v
        })
        .collect();
    Ok(json!({"nodes": list}))
}

fn yes() -> bool {
    true
}

fn node_info(e: &Engine, p: &Json) -> Result<Json> {
    let a: PathArg = args(p)?;
    let net = e.session().doc().snapshot();
    let id = require(&net, &a.path)?;
    let node = net.node(id).ok_or("node vanished")?.clone();
    let desc = desc_of(e, &node.kind)?;
    let params: Vec<Json> = desc
        .params
        .iter()
        .map(|spec| {
            let explicit = node.params.get(spec.name.as_str());
            let mut v = json!({"name": spec.name, "value": explicit.unwrap_or(&spec.default)});
            if explicit.is_some()
                && let Some(o) = v.as_object_mut()
            {
                o.insert("default".into(), json!(spec.default));
            }
            v
        })
        .collect();
    let binds: Vec<Json> = node
        .binds
        .iter()
        .map(|(part, b)| {
            let from = net.path_of(b.node).map(|p| match &b.channel {
                Some(c) => format!("{p}:{c}"),
                None => p.to_string(),
            });
            json!({"param": part.to_string(), "from": from})
        })
        .collect();
    let inputs: Vec<Json> = desc
        .inputs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let from = node.inputs.get(i).copied().flatten().and_then(|port| source_name(e, &net, port));
            json!({"name": spec.name, "kind": spec.kind, "from": from})
        })
        .collect();
    let outputs: Vec<Json> = desc
        .outputs
        .iter()
        .enumerate()
        .map(|(o, spec)| {
            let to: Vec<Json> = net
                .consumers(id)
                .flat_map(|c| {
                    let reader = desc_of(e, &c.kind).ok();
                    c.inputs
                        .iter()
                        .enumerate()
                        .filter(|(_, slot)| slot.is_some_and(|p| p.node == id && p.output as usize == o))
                        .map(|(i, _)| {
                            let input = reader.and_then(|d| d.inputs.get(i)).map(|s| s.name.clone());
                            json!({"node": net.path_of(c.id), "input": input})
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            json!({"name": spec.name, "kind": spec.kind, "to": to})
        })
        .collect();
    let status = e.on_cook(move |st| {
        let watched = st.watched.contains(&id);
        (st.cooker.status(id), watched, st.clock.rate())
    })?;
    let (status, watched, rate) = status;
    // Params the instance declares (a shader's header), with their values; explicit values
    // nobody declares are listed too, so a typo is visible.
    let mut instance: Vec<Json> = Vec::new();
    if desc.extra_params {
        let declared = status.as_ref().and_then(|s| s.params.clone()).unwrap_or_default();
        for spec in &declared {
            let explicit = node.params.get(spec.name.as_str());
            instance.push(json!({
                "name": spec.name, "kind": spec.kind, "value": explicit.unwrap_or(&spec.default),
                "default": spec.default, "range": spec.range, "unit": spec.unit, "doc": spec.doc,
            }));
        }
        for (name, value) in &node.params {
            let known =
                desc.param_spec(name.as_str()).is_some() || declared.iter().any(|s| s.name == name.as_str());
            if !known {
                instance.push(json!({"name": name, "value": value, "declared": false}));
            }
        }
    } else {
        // Values kept from the project file that the type no longer declares.
        for (name, value) in node.params.iter().filter(|(n, _)| desc.param_spec(n.as_str()).is_none()) {
            instance.push(json!({"name": name, "value": value, "declared": false}));
        }
    }
    let status = status.map(|s| {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        json!({
            "version": s.version,
            "has_output": s.has_output,
            "error": s.error,
            "warning": s.warning,
            "job": s.job,
            "time_dependent": s.time_dependent,
            "bound": s.bound,
            "cooks": s.stats.cooks,
            "last_ms": ms(s.stats.last),
            "average_ms": ms(s.stats.average),
            "max_ms": ms(s.stats.max),
            "load": s.stats.load(rate),
        })
    });
    Ok(json!({
        "path": a.path,
        "id": id.0,
        "kind": node.kind,
        "summary": desc.summary,
        "locked": node.locked,
        "pos": node.pos,
        "params": params,
        "binds": binds,
        "instance_params": instance,
        "inputs": inputs,
        "outputs": outputs,
        "component": crate::components::info(e, &net, id),
        "watched": watched,
        "status": status,
    }))
}

/// `"/a"` for output 0, `"/a:name"` otherwise, as in the project file.
pub(crate) fn source_name(e: &Engine, net: &Network, port: penumbra_graph::Port) -> Option<String> {
    let path = net.path_of(port.node)?;
    if port.output == 0 {
        return Some(path.to_string());
    }
    let kind = &net.node(port.node)?.kind;
    let name = desc_of(e, kind).ok()?.outputs.get(port.output as usize)?.name.clone();
    Some(format!("{path}:{name}"))
}

pub(crate) fn require(net: &Network, path: &NodePath) -> Result<NodeId> {
    net.resolve(path).ok_or_else(|| CommandError(format!("no node at {path}")))
}

pub(crate) fn desc_of<'e>(e: &'e Engine, kind: &str) -> Result<&'e NodeDesc> {
    e.types().desc(kind).ok_or_else(|| CommandError(format!("unknown node kind `{kind}`")))
}

// ---------------------------------------------------------------------------------------------
// node: editing

pub(crate) fn edit(e: &Engine, ops: &[Op], label: &str) -> Result<EditOutcome> {
    edit_in(&e.session(), ops, label)
}

/// [`edit`] in `session`: a command that read the network from it edits the same project, even
/// if another one opened meanwhile.
pub(crate) fn edit_in(session: &Session, ops: &[Op], label: &str) -> Result<EditOutcome> {
    Ok(session.edit(Actor::Claude, ops, None, label, Instant::now())?)
}

pub(crate) fn outcome(out: &EditOutcome) -> Json {
    let recorded = match out.recorded {
        Recorded::New => "new",
        Recorded::Merged => "merged",
        Recorded::Collapsed => "collapsed",
        Recorded::NoChange => "no_change",
    };
    let created: Vec<Json> = out.created.iter().map(|id| json!(out.snapshot.path_of(*id))).collect();
    json!({"history": recorded, "created": created})
}

fn node_create(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        kind: String,
        #[serde(default)]
        params: BTreeMap<Name, Value>,
    }
    let a: A = args(p)?;
    Ok(outcome(&edit(e, &[Op::Create { path: a.path, kind: a.kind, params: a.params }], "")?))
}

fn node_delete(e: &Engine, p: &Json) -> Result<Json> {
    let a: PathArg = args(p)?;
    Ok(outcome(&edit(e, &[Op::Delete { path: a.path }], "")?))
}

fn node_replace(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        kind: String,
    }
    let a: A = args(p)?;
    let net = e.session().doc().snapshot();
    let r = penumbra_graph::replace(&net, e.types(), &a.path, &a.kind)?;
    let label = format!("replace {} with {}", a.path, a.kind);
    let mut out = outcome(&edit(e, &r.ops, &label)?);
    if let Some(map) = out.as_object_mut() {
        map.insert("lost".into(), json!(r.lost));
    }
    Ok(out)
}

fn node_rename(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        name: Name,
    }
    let a: A = args(p)?;
    Ok(outcome(&edit(e, &[Op::Rename { path: a.path, name: a.name }], "")?))
}

fn node_set(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        params: BTreeMap<Name, Option<Value>>,
    }
    let a: A = args(p)?;
    let ops = a
        .params
        .into_iter()
        .map(|(name, value)| {
            let param = ParamPath::new(a.path.clone(), name)?;
            Ok(match value {
                Some(value) => Op::Set { param, value },
                None => Op::Reset { param },
            })
        })
        .collect::<std::result::Result<Vec<_>, Error>>()?;
    if ops.is_empty() {
        return Err("`params` is empty".into());
    }
    Ok(outcome(&edit(e, &ops, "")?))
}

/// An input or output named by index or by name.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Slot {
    Index(u32),
    Name(String),
}

impl Slot {
    fn json(&self) -> Json {
        match self {
            Slot::Index(i) => json!(i),
            Slot::Name(n) => json!(n),
        }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Slot::Index(0)
    }
}

pub(crate) fn slot_index(slot: &Slot, names: &[&str], what: &str, path: &NodePath) -> Result<u32> {
    match slot {
        Slot::Index(i) => Ok(*i),
        Slot::Name(n) => {
            names.iter().position(|m| m == n).and_then(|i| u32::try_from(i).ok()).ok_or_else(|| {
                CommandError(format!("{path} has no {what} `{n}`; {what}s: {}", names.join(", ")))
            })
        }
    }
}

pub(crate) fn kind_at<'e>(e: &'e Engine, net: &Network, path: &NodePath) -> Result<&'e NodeDesc> {
    let id = require(net, path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    desc_of(e, &kind)
}

fn input_index(e: &Engine, net: &Network, path: &NodePath, slot: &Slot) -> Result<u32> {
    let d = kind_at(e, net, path)?;
    let names: Vec<&str> = d.inputs.iter().map(|i| i.name.as_str()).collect();
    slot_index(slot, &names, "input", path)
}

fn node_connect(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        from: NodePath,
        #[serde(default)]
        output: Slot,
        to: NodePath,
        #[serde(default)]
        input: Slot,
    }
    let mut a: A = args(p)?;
    let net = e.session().doc().snapshot();
    // A component's inputs and outputs are its port nodes.
    if let Some(port) = crate::components::port_node(e, &net, &a.from, &a.output.json(), PortRole::Out)? {
        (a.from, a.output) = (port, Slot::Index(0));
    }
    if let Some(port) = crate::components::port_node(e, &net, &a.to, &a.input.json(), PortRole::In)? {
        (a.to, a.input) = (port, Slot::Index(0));
    }
    let d = kind_at(e, &net, &a.from)?;
    let names: Vec<&str> = d.outputs.iter().map(|o| o.name.as_str()).collect();
    let output = slot_index(&a.output, &names, "output", &a.from)?;
    let input = input_index(e, &net, &a.to, &a.input)?;
    Ok(outcome(&edit(e, &[Op::Connect { from: a.from, output, to: a.to, input }], "")?))
}

fn node_disconnect(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        to: NodePath,
        input: Slot,
    }
    let mut a: A = args(p)?;
    let net = e.session().doc().snapshot();
    if let Some(port) = crate::components::port_node(e, &net, &a.to, &a.input.json(), PortRole::In)? {
        (a.to, a.input) = (port, Slot::Index(0));
    }
    let input = input_index(e, &net, &a.to, &a.input)?;
    Ok(outcome(&edit(e, &[Op::Disconnect { to: a.to, input }], "")?))
}

fn node_lock(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        #[serde(default = "yes")]
        locked: bool,
    }
    let a: A = args(p)?;
    Ok(outcome(&edit(e, &[Op::Lock { path: a.path, locked: a.locked }], "")?))
}

fn node_place(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        nodes: BTreeMap<NodePath, Option<[f32; 2]>>,
    }
    let a: A = args(p)?;
    let ops: Vec<Op> = a.nodes.into_iter().map(|(path, pos)| Op::Place { path, pos }).collect();
    let label = match ops.as_slice() {
        [] => return Err("`nodes` is empty".into()),
        [one] => one.label(),
        many => format!("move {} nodes", many.len()),
    };
    Ok(outcome(&edit(e, &ops, &label)?))
}

/// `/node.param` or `/node.param.N` (component N of a vector or colour).
pub(crate) fn param_part(text: &str) -> Result<(ParamPath, Option<u8>)> {
    if let Ok(p) = ParamPath::parse(text) {
        return Ok((p, None));
    }
    let (path, last) =
        text.rsplit_once('.').ok_or_else(|| format!("`{text}` is not a param path like /node.param"))?;
    let part = ParamPart::parse(&format!("x.{last}"))?;
    Ok((ParamPath::parse(path)?, part.component))
}

fn node_bind(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        param: String,
        from: NodePath,
        channel: Option<Name>,
    }
    let a: A = args(p)?;
    let (param, component) = param_part(&a.param)?;
    Ok(outcome(&edit(e, &[Op::Bind { param, component, from: a.from, channel: a.channel }], "")?))
}

fn node_unbind(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        param: String,
    }
    let a: A = args(p)?;
    let (param, component) = param_part(&a.param)?;
    Ok(outcome(&edit(e, &[Op::Unbind { param, component }], "")?))
}

fn node_batch(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        ops: Vec<Op>,
        #[serde(default)]
        label: String,
    }
    let a: A = args(p)?;
    if a.ops.is_empty() {
        return Err("`ops` is empty".into());
    }
    Ok(outcome(&edit(e, &a.ops, &a.label)?))
}

// ---------------------------------------------------------------------------------------------
// cooking

fn node_watch(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        #[serde(default = "yes")]
        watch: bool,
    }
    let a: A = args(p)?;
    let id = require(&e.session().doc().snapshot(), &a.path)?;
    let watch = a.watch;
    e.on_cook(move |st| {
        if watch {
            st.watched.insert(id);
        } else {
            st.watched.remove(&id);
        }
    })?;
    Ok(json!({"path": a.path, "watched": watch}))
}

fn node_reset(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        path: Option<NodePath>,
        #[serde(default)]
        paths: Vec<NodePath>,
    }
    let a: A = args(p)?;
    let paths: Vec<NodePath> = a.path.into_iter().chain(a.paths).collect();
    if paths.is_empty() {
        return Err(CommandError("give the nodes to reset: {paths: [...]} or {path}".into()));
    }
    let net = e.session().doc().snapshot();
    let ids = paths.iter().map(|p| require(&net, p)).collect::<Result<Vec<NodeId>>>()?;
    let report = e.reset_nodes(&ids)?;
    Ok(json!({
        "reset": report.reset,
        "locked": report.locked,
        "stateless": report.stateless,
        "message": report.message(),
    }))
}

fn cook_status(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    let (now, playing, last, watched, thumbs, pace) = e.on_cook(|st| {
        (
            st.clock.now(),
            st.clock.is_playing(),
            st.last.clone(),
            st.watched.iter().copied().collect::<Vec<_>>(),
            st.cooker.thumbnail_policy(),
            st.pace(),
        )
    })?;
    let net = e.session().doc().snapshot();
    let watched: Vec<Json> = watched.iter().map(|id| json!(net.path_of(*id))).collect();
    Ok(json!({
        "frame": now.frame,
        "seconds": now.seconds(),
        "rate": now.rate.to_string(),
        "playing": playing,
        "notice": e.notice(),
        "pace": pace,
        "pass": last.pass,
        "last_pass_ms": last.elapsed.as_secs_f64() * 1000.0,
        "cooked_last_pass": last.cooked.len(),
        "watched": watched,
        "thumbnails": {
            "mode": thumbs.mode,
            "min_interval": thumbs.min_interval,
            "budget_ms": thumbs.budget.as_secs_f64() * 1000.0,
        },
    }))
}

/// The slowest thumbnail rate `cook.thumbnails` takes: one cook a second at 60 fps.
const MAX_THUMBNAIL_EVERY: u32 = 60;

fn cook_thumbnails(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        mode: Option<ThumbnailMode>,
        every: Option<u32>,
    }
    let a: A = args(p)?;
    if a.mode.is_none() && a.every.is_none() {
        return Err(CommandError("give `mode`, `every` or both".into()));
    }
    if let Some(every) = a.every
        && !(1..=MAX_THUMBNAIL_EVERY).contains(&every)
    {
        return Err(CommandError(format!("`every` must be 1 to {MAX_THUMBNAIL_EVERY} cook passes")));
    }
    let policy = e.on_cook(move |st| {
        let mut policy = st.cooker.thumbnail_policy();
        policy.mode = a.mode.unwrap_or(policy.mode);
        policy.min_interval = a.every.unwrap_or(policy.min_interval);
        st.cooker.set_thumbnail_policy(policy);
        policy
    })?;
    Ok(json!({"mode": policy.mode, "every": policy.min_interval}))
}

fn cook_play(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default = "yes")]
        playing: bool,
    }
    let a: A = args(p)?;
    let playing = a.playing;
    if playing {
        e.set_notice(None);
    }
    let frame = e.on_cook(move |st| {
        if playing {
            st.clock.play();
        } else {
            st.clock.pause();
        }
        st.clock.now().frame
    })?;
    Ok(json!({"playing": playing, "frame": frame}))
}

// ---------------------------------------------------------------------------------------------
// snapshots and the GPU

/// How long `node.snapshot` waits for a node with an asynchronous job (an image loading, a
/// shader compiling).
const SNAPSHOT_WAIT: Duration = Duration::from_secs(10);

/// Cooks node `id` on demand (it need not be watched or feed an output) and returns output
/// `output` with the node's status. Waits while a job is in flight, so the result is the
/// current one rather than the previous file's.
pub(crate) fn cook_now(e: &Engine, id: NodeId, output: u32) -> Result<(Option<Data>, Option<NodeStatus>)> {
    cook_now_in(e, &e.session(), id, output)
}

/// [`cook_now`] with the network of `session`.
pub(crate) fn cook_now_in(
    e: &Engine,
    session: &Arc<Session>,
    id: NodeId,
    output: u32,
) -> Result<(Option<Data>, Option<NodeStatus>)> {
    let deadline = Instant::now() + SNAPSHOT_WAIT;
    loop {
        let session = session.clone();
        let (data, status) = e.on_cook(move |st| {
            let net = session.doc().snapshot();
            let port = Port { node: id, output };
            st.cooker.cook_showing(&net, st.clock.now(), &[id], &[], &[port]);
            (st.cooker.output(port).cloned(), st.cooker.status(id))
        })?;
        let busy = status
            .as_ref()
            .is_some_and(|s| matches!(s.job, Some(JobStatus::Queued | JobStatus::Running { .. })));
        let failed_empty = data.is_none() && status.as_ref().is_some_and(|s| s.error.is_some());
        if (data.is_some() && !busy) || failed_empty || Instant::now() >= deadline {
            return Ok((data, status));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn result_of(path: &NodePath, data: Option<Data>, error: Option<String>) -> Result<Data> {
    data.ok_or_else(|| match error {
        Some(e) => format!("{path} has no result: {e}").into(),
        None => format!("{path} has no result yet").into(),
    })
}

/// Cooks the node at `path` and returns output `output` (default the first of kind `kind`).
fn read_output(
    e: &Engine,
    path: &NodePath,
    output: Option<&Slot>,
    kind: penumbra_graph::DataKind,
) -> Result<(Data, Option<NodeStatus>)> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let desc = kind_at(e, &net, path)?;
    let names: Vec<&str> = desc.outputs.iter().map(|o| o.name.as_str()).collect();
    let output = match output {
        Some(slot) => slot_index(slot, &names, "output", path)?,
        None => desc
            .outputs
            .iter()
            .position(|o| o.kind == kind)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| format!("{path} has no {kind:?} output"))?,
    };
    let (data, status) = cook_now(e, id, output)?;
    let data = result_of(path, data, status.as_ref().and_then(|s| s.error.clone()))?;
    Ok((data, status))
}

fn chan_read(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        output: Option<Slot>,
        #[serde(default)]
        samples: bool,
    }
    let a: A = args(p)?;
    let (data, status) = read_output(e, &a.path, a.output.as_ref(), penumbra_graph::DataKind::Chan)?;
    let chans = data.get::<Chans>().ok_or_else(|| format!("{} output is not channels", a.path))?;
    let channels: Vec<Json> = chans
        .channels()
        .iter()
        .map(|c| {
            let mut v = json!({"name": c.name, "value": c.value()});
            if a.samples
                && let Some(o) = v.as_object_mut()
            {
                o.insert("samples".into(), json!(c.samples));
            }
            v
        })
        .collect();
    Ok(json!({
        "rate": chans.rate(),
        "length": chans.len(),
        "channels": channels,
        "warning": status.and_then(|s| s.warning),
    }))
}

fn table_read(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        output: Option<Slot>,
        #[serde(default = "hundred")]
        rows: usize,
    }
    let a: A = args(p)?;
    let (data, status) = read_output(e, &a.path, a.output.as_ref(), penumbra_graph::DataKind::Table)?;
    let table = data.get::<Table>().ok_or_else(|| format!("{} output is not a table", a.path))?;
    let rows: Vec<&Vec<String>> = table.rows().iter().take(a.rows).collect();
    Ok(json!({
        "rows": rows,
        "num_rows": table.num_rows(),
        "num_cols": table.num_cols(),
        "warning": status.and_then(|s| s.warning),
    }))
}

fn geo_info(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        output: Option<Slot>,
    }
    let a: A = args(p)?;
    let (data, status) = read_output(e, &a.path, a.output.as_ref(), penumbra_graph::DataKind::Geo)?;
    let geo = data.get::<Geo>().ok_or_else(|| format!("{} output is not geometry", a.path))?;
    let mut parts: BTreeMap<u32, (String, usize)> = BTreeMap::new();
    for item in &geo.items {
        for p in item.model.mesh.part_info() {
            let entry = parts.entry(item.part_offset + p.id).or_insert_with(|| (p.name.clone(), 0));
            entry.1 += p.triangles;
        }
    }
    let parts: Vec<Json> = parts
        .into_iter()
        .map(|(id, (name, triangles))| json!({"id": id, "name": name, "triangles": triangles}))
        .collect();
    let bounds =
        geo.bounds().map(|b| json!({"min": b.min, "max": b.max, "size": b.size(), "center": b.center()}));
    Ok(json!({
        "parts": parts,
        "triangles": geo.triangle_count(),
        "vertices": geo.vertex_count(),
        "models": geo.items.len(),
        "gpu_bytes": geo.items.iter().map(|i| i.model.bytes()).sum::<u64>(),
        "bounds": bounds,
        "warning": status.and_then(|s| s.warning),
        "note": "world units (metres where the file says so), z up; part ids as the render node's data pass writes them (minus 1)",
    }))
}

fn render_frame(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
    }
    let a: A = args(p)?;
    let session = e.session();
    let net = session.doc().snapshot();
    let id = require(&net, &a.path)?;
    let desc = kind_at(e, &net, &a.path)?;
    let node = net.node(id).ok_or_else(|| format!("{} does not exist", a.path))?;
    if node.kind != "tex.render" {
        return Err(format!("{} is a {}, not a tex.render", a.path, node.kind).into());
    }
    let source =
        node.inputs.first().copied().flatten().ok_or_else(|| format!("{} has no geo input", a.path))?;
    let (data, _) = cook_now_in(e, &session, source.node, source.output)?;
    let geo = data.as_ref().and_then(|d| d.get::<Geo>()).ok_or("its geo input has no geometry yet")?;
    let b = geo.bounds().ok_or("its geometry is empty")?;
    let size = |name: &str| node.param(desc, name).and_then(Value::as_f64).unwrap_or(1.0).max(1.0);
    let aspect = size("width") / size("height");
    // The wired camera node is the one to move.
    let src = node
        .inputs
        .get(penumbra_ops::tex::render::CAMERA_INPUT)
        .copied()
        .flatten()
        .ok_or_else(|| format!("{} has no camera: wire a view.camera into its `camera` input", a.path))?;
    let cam = net.node(src.node).ok_or("its camera input comes from nowhere")?;
    let path = net.path_of(src.node).ok_or("its camera input comes from nowhere")?;
    if cam.kind != penumbra_ops::view::CAMERA {
        return Err(format!(
            "its camera comes from {path}, a {}; frame a {} instead",
            cam.kind,
            penumbra_ops::view::CAMERA
        )
        .into());
    }
    let desc = kind_at(e, &net, &path)?;
    let node = cam;
    let text = |name: &str| node.param(desc, name).and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !text("calibration").is_empty() {
        return Err(
            format!("{path} is calibrated (its `calibration` param); framing would not move it").into()
        );
    }
    let ortho = text("projection") == "orthographic";
    let vec3 = |name: &str| match node.param(desc, name).and_then(Value::components) {
        Some(&[x, y, z]) => [x, y, z],
        _ => [0.0; 3],
    };
    let fov = node.param(desc, "fov").and_then(Value::as_f64).unwrap_or(40.0);
    let framing = penumbra_ops::view::frame_bounds(&b, vec3("position"), vec3("target"), fov, aspect, ortho);
    let (eye, center) = (framing.eye, framing.target);
    let mut ops = vec![
        Op::Set { param: ParamPath::new(path.clone(), Name::new("position")?)?, value: Value::Vec3(eye) },
        Op::Set { param: ParamPath::new(path.clone(), Name::new("target")?)?, value: Value::Vec3(center) },
    ];
    if let Some(height) = framing.height {
        let height = (height * 1e6).round() / 1e6;
        ops.push(Op::Set {
            param: ParamPath::new(path.clone(), Name::new("size")?)?,
            value: Value::Float(height),
        });
    }
    let mut out = outcome(&edit_in(&session, &ops, "frame the geometry")?);
    if let Some(o) = out.as_object_mut() {
        o.insert("camera".into(), json!(path.to_string()));
        o.insert("position".into(), json!(eye));
        o.insert("target".into(), json!(center));
        if let Some(height) = framing.height {
            o.insert("size".into(), json!(height));
        }
    }
    Ok(out)
}

fn hundred() -> usize {
    100
}

fn node_snapshot(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        output: Option<Slot>,
        file: Option<PathBuf>,
    }
    let a: A = args(p)?;
    let gpu = e.gpu().ok_or("no GPU yet (the window opens it)")?.clone();
    let net = e.session().doc().snapshot();
    let id = require(&net, &a.path)?;
    let desc = kind_at(e, &net, &a.path)?;
    let names: Vec<&str> = desc.outputs.iter().map(|o| o.name.as_str()).collect();
    let output = match &a.output {
        Some(slot) => slot_index(slot, &names, "output", &a.path)?,
        None => 0,
    };
    if names.is_empty() {
        return Err(format!("{} has no outputs", a.path).into());
    }
    let (data, status) = cook_now(e, id, output)?;
    let error = status.as_ref().and_then(|s| s.error.clone());
    let warning = status.as_ref().and_then(|s| s.warning.clone());
    let data = result_of(&a.path, data, error.clone())?;
    let tex = match data.get::<Geo>() {
        Some(geo) => match &geo.preview {
            Some(t) => t.clone(),
            None => penumbra_ops::geo::preview(
                &gpu,
                &mut penumbra_gpu::render::Renderer::new(),
                geo,
                penumbra_ops::geo::Preview::Geometry,
            )
            .map_err(|e| format!("{}: no picture of its geometry: {e}", a.path))?,
        },
        None => {
            data.get::<Tex>().ok_or_else(|| format!("{} output {output} is not a texture", a.path))?.clone()
        }
    };
    let pixels = gpu.read(&tex)?;
    let png = pixels.to_png()?;
    let file = match a.file {
        Some(f) => f,
        None => {
            let name = a.path.to_string().trim_start_matches('/').replace('/', "_");
            std::env::temp_dir().join("penumbra").join("snapshots").join(format!("{name}.png"))
        }
    };
    if let Some(dir) = file.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&file, png).map_err(|e| format!("{}: {e}", file.display()))?;
    let stats = pixels.stats();
    Ok(json!({
        "file": file.display().to_string(),
        "width": pixels.width,
        "height": pixels.height,
        "version": status.as_ref().map(|s| s.version),
        "error": error,
        "warning": warning,
        "stats": {
            "min": stats.min,
            "max": stats.max,
            "mean": stats.mean,
            "non_finite_pixels": stats.non_finite,
        },
        "note": "linear premultiplied values in stats; the PNG is sRGB with straight alpha",
    }))
}

// ---------------------------------------------------------------------------------------------
// output.*, system.*

fn output_list(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    let board = e.board().state();
    let net = e.session().doc().snapshot();
    let frame = e.output();
    let projectors: Vec<Json> = frame
        .projectors
        .iter()
        .map(|pr| {
            let state = board.projectors.get(&pr.node).cloned().unwrap_or_default();
            json!({
                "path": net.path_of(pr.node).map(|p| p.to_string()),
                "display": pr.display,
                "active": pr.active,
                "shown_on": state.display,
                "error": state.error,
                "warning": state.warning,
                "has_input": pr.source.is_some(),
            })
        })
        .collect();
    let pace = e.on_cook(|st| st.pace()).ok();
    Ok(json!({
        "windows_supported": board.managed,
        "displays": board.displays,
        "windows": board.windows,
        "keeps_display_awake": board.keeps_display_awake,
        "projectors": projectors,
        "pace": pace,
    }))
}

// ---------------------------------------------------------------------------------------------
// res.*

/// How long `res.analyze` and `res.optimize` wait for render nodes to measure what their
/// cameras see (on the connection's thread; the cook thread never waits for the GPU).
const RES_MEASURE_WAIT: Duration = Duration::from_secs(3);

fn analyze(e: &Engine) -> Result<(Arc<Network>, Arc<Report>)> {
    let started = Instant::now();
    loop {
        let session = e.session().clone();
        let (net, report) = e.on_cook(move |st| {
            st.renders.activate_for(RES_MEASURE_WAIT);
            st.renders.collect();
            let net = session.doc().snapshot();
            let report = st.analyze_resolution(&net, session.types());
            (net, Arc::new(report))
        })?;
        if report.pending.is_empty() || started.elapsed() >= RES_MEASURE_WAIT {
            return Ok((net, report));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn res_analyze(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    let (_, report) = analyze(e)?;
    let mut out = to_json(&*report)?;
    if let Some(o) = out.as_object_mut() {
        o.insert(
            "limits".into(),
            json!({"coarse_below": penumbra_ops::res::COARSE, "wasted_above": penumbra_ops::res::WASTED}),
        );
        o.insert("percentile".into(), json!(penumbra_ops::mapping::DEMAND_PERCENTILE));
    }
    Ok(out)
}

fn res_optimize(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        paths: Option<Vec<NodePath>>,
        #[serde(default)]
        dry_run: bool,
    }
    let a: A = args(p)?;
    let (net, report) = analyze(e)?;
    let only = match &a.paths {
        Some(paths) => Some(paths.iter().map(|p| require(&net, p)).collect::<Result<BTreeSet<NodeId>>>()?),
        None => None,
    };
    let changes = report.changes(only.as_ref());
    let ids: Vec<NodeId> = changes.iter().map(|(n, _, _)| n.id).collect();
    let listed: Vec<Json> =
        changes.iter().map(|(n, from, to)| json!({"path": n.path, "from": from, "to": to})).collect();
    let session = e.session().clone();
    let pixel = e.on_cook(move |st| {
        penumbra_ops::res::pixel_params(&session.doc().snapshot(), session.types(), &st.cooker, &ids)
    })?;
    let ops = report.optimize_ops(&net, only.as_ref());
    let history = if a.dry_run || ops.is_empty() {
        json!(null)
    } else {
        outcome(&edit(e, &ops, "res.optimize")?).get("history").cloned().unwrap_or(Json::Null)
    };
    Ok(json!({
        "changes": listed,
        "dry_run": a.dry_run,
        "history": history,
        "pixel_params": pixel,
        "note": "params in pixels change their look with the resolution; check them",
    }))
}

fn res_overlay(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default = "yes")]
        on: bool,
    }
    let a: A = args(p)?;
    let on = a.on;
    let session = e.session().clone();
    let (mappers, renders) = e.on_cook(move |st| {
        let net = session.doc().snapshot();
        st.set_overlay(on, &net, session.types());
        let count = |kind: &str| net.nodes().filter(|n| n.kind == kind).count();
        (count(penumbra_ops::tex::mapper::MAPPER), count(penumbra_ops::tex::render::RENDER))
    })?;
    Ok(json!({"on": on, "mappers": mappers, "renders": renders}))
}

fn system_check(_e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    to_json(&penumbra_system::checks::run())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixArgs {
    id: String,
}

fn system_fix(_e: &Engine, p: &Json) -> Result<Json> {
    let a: FixArgs = args(p)?;
    let previous = penumbra_system::checks::fix(&a.id)?;
    let after = penumbra_system::checks::run().into_iter().find(|c| c.id == a.id);
    Ok(json!({"previous": previous, "check": after}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreArgs {
    previous: penumbra_system::Previous,
}

fn system_restore(_e: &Engine, p: &Json) -> Result<Json> {
    let a: RestoreArgs = args(p)?;
    penumbra_system::checks::restore(&a.previous)?;
    let after = penumbra_system::checks::run().into_iter().find(|c| c.id == a.previous.id);
    Ok(json!({"check": after}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextFontsArgs {
    family: Option<String>,
}

/// Waits for the font scan (started by the first call) for a while rather than failing at once.
fn text_fonts(_: &Engine, p: &Json) -> Result<Json> {
    let a: TextFontsArgs = args(p)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let list = loop {
        if let Some(list) = penumbra_ops::tex::font_families() {
            break list;
        }
        if Instant::now() > deadline {
            return Err("the system fonts are still being scanned; try again".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let fonts: Vec<Json> = list
        .iter()
        .filter(|(f, _)| a.family.as_ref().is_none_or(|want| f.eq_ignore_ascii_case(want)))
        .map(|(family, styles)| json!({"family": family, "styles": styles}))
        .collect();
    if fonts.is_empty()
        && let Some(want) = &a.family
    {
        return Err(format!("no font family `{want}`").into());
    }
    Ok(json!({"fonts": fonts}))
}

fn script_info(_: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    to_json(&penumbra_script::host().status())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptBenchArgs {
    runs: Option<u32>,
}

fn script_bench(_: &Engine, p: &Json) -> Result<Json> {
    let a: ScriptBenchArgs = args(p)?;
    // Off the cook thread (this is a connection's thread): scripts of the show wait meanwhile,
    // as `Busy`, so they keep their last result.
    let bench =
        penumbra_script::host().bench(a.runs.unwrap_or(1000)).map_err(|e| CommandError(e.to_string()))?;
    to_json(&bench)
}

fn gpu_info(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
    let Some(gpu) = e.gpu() else {
        return Ok(json!({"attached": false}));
    };
    let errors = gpu.errors();
    let vram = gpu.vram();
    Ok(json!({
        "attached": true,
        "adapter": gpu.name(),
        "max_texture_size": gpu.max_texture_size(),
        "format": format!("{:?}", penumbra_gpu::FORMAT),
        "vram_budget": vram.map(|v| v.budget),
        "vram_usage": vram.map(|v| v.usage),
        "errors": errors.count,
        "last_error": errors.last,
        "lost": gpu.lost(),
    }))
}

fn gpu_simulate_loss(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        confirm: bool,
    }
    let a: A = args(p)?;
    if !a.confirm {
        return Err(CommandError(
            "this ends the GPU device and closes the app; pass {\"confirm\": true}".into(),
        ));
    }
    let gpu = e.gpu().ok_or_else(|| CommandError("no GPU attached".into()))?;
    gpu.simulate_loss("simulated by gpu.simulate_loss");
    Ok(json!({"lost": true}))
}
