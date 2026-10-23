//! Shapes in the engine. Command: `shapes.trace` (mask to shapes).
//!
//! Tracing reads a texture back once, finds the outlines of its regions on the CPU
//! ([`penumbra_ops::trace`]) and writes them as `poly` lines into a `tex.shapes`'s `shapes`
//! text: one edit, undone like any other, and the lines are edited by hand afterwards like drawn
//! ones. Nothing of it runs per frame.
//!
//! With `live`, nothing is written into the text: the source is connected to the node's `trace`
//! input and the options become its `trace_*` params, so the node traces by itself whenever the
//! source changes or the project opens (what a project file written by a script relies on).

use std::sync::Arc;
use std::time::Instant;

use penumbra_core::{Name, NodePath, ParamPath, Table, Value};
use penumbra_gpu::Tex;
use penumbra_graph::Op;
use penumbra_ops::shapes::{self, MAX_INSTANCES};
/// The tag traced shapes get unless another is named.
pub use penumbra_ops::tex::shapes::TRACE_TAG;
use penumbra_ops::tex::shapes::{SHAPES, TABLE_OUTPUT, TRACE_INPUT};
use penumbra_ops::trace::{self, Channel, MAX_PIXELS, Options};
use penumbra_project::Actor;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{Slot, cook_now, kind_at, require, result_of, slot_index};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "shapes.trace",
        "Trace the outlines of a mask or label texture as poly lines into a tex.shapes (one undo step). \
         Replaces the lines of that tag; {path, from (node: path or name beside path), output?, tag? (trace), \
         channel? (auto | red | green | blue | alpha | luma), threshold? (0.5), invert? (inside is \
         below it), labels? (every value its own shape), tolerance? (px, 1), min_area? (px, 64), append?, \
         live? (instead: connect `from` to the node's trace input and set its trace_* params, so it traces \
         by itself whenever the picture changes or the project opens; options not given are reset)}",
        move |p| {
            let a: TraceArgs = args(p)?;
            trace_into(&e, Actor::Claude, &a, Instant::now())
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceArgs {
    /// The `tex.shapes` written to.
    pub path: NodePath,
    /// The node traced: a path, or the name of a node beside `path`.
    pub from: String,
    pub(crate) output: Option<Slot>,
    pub tag: Option<String>,
    pub channel: Option<String>,
    pub threshold: Option<f64>,
    pub invert: Option<bool>,
    pub labels: Option<bool>,
    pub tolerance: Option<f64>,
    pub min_area: Option<f64>,
    /// Keep the lines with the tag (else they are replaced).
    pub append: Option<bool>,
    /// Trace by the node's `trace` input and params instead of into its text.
    pub live: Option<bool>,
}

impl TraceArgs {
    /// Tracing `from`'s first output into `path` with every default.
    pub fn new(path: NodePath, from: &str) -> Self {
        Self {
            path,
            from: from.to_string(),
            output: None,
            tag: None,
            channel: None,
            threshold: None,
            invert: None,
            labels: None,
            tolerance: None,
            min_area: None,
            append: None,
            live: None,
        }
    }
}

/// Traces as `a` says and writes the result into the shapes text as `actor`; the reply says what
/// was found, dropped and filled, with `message` a one-line summary.
pub fn trace_into(e: &Engine, actor: Actor, a: &TraceArgs, now: Instant) -> CmdResult<Json> {
    let net = e.session().doc().snapshot();
    let target = require(&net, &a.path)?;
    let node = net.node(target).ok_or_else(|| CommandError(format!("no node at {}", a.path)))?;
    if node.kind != SHAPES {
        return Err(CommandError(format!("{} is a {}, not a {SHAPES}", a.path, node.kind)));
    }
    let tag = a.tag.as_deref().unwrap_or(TRACE_TAG).trim().to_string();
    Name::new(&tag).map_err(|e| CommandError(format!("tag `{tag}`: {e}")))?;
    let channel = match a.channel.as_deref() {
        None => Channel::Auto,
        Some(c) => Channel::parse(c)
            .ok_or_else(|| CommandError(format!("channel `{c}`: one of {}", Channel::NAMES.join(", "))))?,
    };
    let threshold = a.threshold.unwrap_or(0.5);
    let tolerance = a.tolerance.unwrap_or(1.0);
    let min_area = a.min_area.unwrap_or(64.0);
    if !threshold.is_finite() {
        return Err("`threshold` must be a number".into());
    }
    if !(0.0..=1000.0).contains(&tolerance) {
        return Err("`tolerance` is 0 to 1000 pixels".into());
    }
    if !(0.0..=1e12).contains(&min_area) {
        return Err("`min_area` is 0 or more pixels".into());
    }

    // The shapes there now: they must parse, so nothing of them is lost.
    let desc = kind_at(e, &net, &a.path)?;
    let text = node.param(desc, "shapes").and_then(Value::as_str).unwrap_or_default().to_string();
    let old = shapes::parse(&text)
        .map_err(|err| CommandError(format!("{}: fix its shapes text first: {err}", a.path)))?;
    let append = a.append.unwrap_or(false);
    let replaced: Vec<usize> = if append {
        Vec::new()
    } else {
        (0..old.len()).filter(|i| old.get(*i).is_some_and(|s| s.tag == tag)).collect()
    };
    let kept: Vec<shapes::Shape> =
        old.iter().enumerate().filter(|(i, _)| !replaced.contains(i)).map(|(_, s)| s.clone()).collect();
    let room = MAX_INSTANCES.saturating_sub(shapes::instances(&kept).len());

    // The source's picture, read back once.
    let source = net.resolve_from(target, &a.from).map_err(|err| CommandError(format!("`from`: {err}")))?;
    let from_path = net.path_of(source).ok_or("`from`: the node is gone")?;
    let source_desc = kind_at(e, &net, &from_path)?;
    let names: Vec<&str> = source_desc.outputs.iter().map(|o| o.name.as_str()).collect();
    let output = match &a.output {
        Some(slot) => slot_index(slot, &names, "output", &from_path)?,
        None => 0,
    };
    if a.live.unwrap_or(false) {
        if append {
            return Err("`append` keeps lines of the text; a live trace writes none".into());
        }
        return trace_live(e, actor, a, &from_path, output, &tag, now);
    }
    let gpu = e.gpu().ok_or("no GPU yet (the window opens it)")?.clone();
    let (data, status) = cook_now(e, source, output)?;
    let error = status.as_ref().and_then(|s| s.error.clone());
    let data = result_of(&from_path, data, error)?;
    let tex = data
        .get::<Tex>()
        .ok_or_else(|| CommandError(format!("{from_path} output {output} is not a texture")))?;
    let (w, h) = tex.size();
    if (w as usize).saturating_mul(h as usize) > MAX_PIXELS {
        return Err(CommandError(format!(
            "{from_path} is {w} x {h}; at most {MAX_PIXELS} pixels are traced (make it smaller first)"
        )));
    }
    let pixels = gpu.read(tex)?;
    let (values, channel) = trace::channel(&pixels.data, channel);
    let options = Options {
        threshold: threshold as f32,
        invert: a.invert.unwrap_or(false),
        labels: a.labels.unwrap_or(false),
        tolerance,
        min_area: min_area.round() as u64,
        max_regions: room,
    };
    let traced = trace::trace(pixels.width, pixels.height, &values, &options).map_err(CommandError)?;
    let new = trace::to_shapes(&traced.regions, pixels.width, pixels.height, &tag);

    // Old lines with the tag out (from the last, so the indices hold), the new ones at the end.
    let mut out = text.clone();
    for i in replaced.iter().rev() {
        out = shapes::replace(&out, *i, &[]);
    }
    out = shapes::replace(&out, usize::MAX, &new);
    shapes::parse(&out).map_err(|err| CommandError(format!("the traced text does not parse: {err}")))?;

    let param = ParamPath::new(a.path.clone(), Name::new("shapes")?)?;
    let label = format!("trace {from_path}");
    let ops = [Op::Set { param, value: Value::Text(out) }];
    e.session().edit(actor, &ops, None, &label, now)?;

    let mut warnings: Vec<String> = Vec::new();
    if node.inputs.first().copied().flatten().is_some() {
        warnings.push(format!("{}: its own shapes are not drawn while its table input is connected", a.path));
    }
    let surface = |name: &str| node.param(desc, name).and_then(Value::as_f64).unwrap_or(0.0);
    let (sw, sh) = (surface("width"), surface("height"));
    if sw > 0.0 && sh > 0.0 && ((f64::from(w) / f64::from(h)) / (sw / sh) - 1.0).abs() > 0.01 {
        warnings.push(format!(
            "{from_path} is {w} x {h} but the surface {sw} x {sh}: the shapes are stretched to it"
        ));
    }
    if traced.dropped > 0 {
        warnings.push(format!("{} smallest regions dropped: at most {MAX_INSTANCES} shapes", traced.dropped));
    }
    let points: usize = traced.regions.iter().map(|r| r.points.len()).sum();
    let holes = traced.holes();
    let mut message = format!("traced {} shape(s) from {from_path} into {}", new.len(), a.path);
    if traced.specks > 0 {
        message.push_str(&format!(", {} speck(s) dropped", traced.specks));
    }
    if holes > 0 {
        message.push_str(&format!(", {holes} hole(s) filled"));
    }
    if new.is_empty() {
        let what = match (options.labels, options.invert) {
            (true, _) => "no value above 0",
            (false, false) => "nothing above the threshold",
            (false, true) => "nothing below the threshold",
        };
        message.push_str(&format!(" ({what} in its {} channel)", channel.name()));
    }
    Ok(json!({
        "message": message,
        "shapes": new.len(),
        "points": points,
        "replaced": replaced.len(),
        "specks": traced.specks,
        "dropped": traced.dropped,
        "holes_filled": holes,
        "coarser": traced.coarser,
        "channel": channel.name(),
        "source": {"path": from_path.to_string(), "output": output, "width": w, "height": h},
        "tag": tag,
        "warnings": warnings,
    }))
}

/// `shapes.trace` with `live`: connects the source to the node's `trace` input and sets its
/// `trace_*` params (the options given; the others back to their defaults) in one undo step,
/// then waits for the node's first trace to say what it found.
fn trace_live(
    e: &Engine,
    actor: Actor,
    a: &TraceArgs,
    from_path: &NodePath,
    output: u32,
    tag: &str,
    now: Instant,
) -> CmdResult<Json> {
    let param =
        |name: &str| -> CmdResult<ParamPath> { Ok(ParamPath::new(a.path.clone(), Name::new(name)?)?) };
    let set = |name: &str, value: Option<Value>| -> CmdResult<Op> {
        Ok(match value {
            Some(value) => Op::Set { param: param(name)?, value },
            None => Op::Reset { param: param(name)? },
        })
    };
    let input = u32::try_from(TRACE_INPUT).map_err(|_| "the trace input has no index")?;
    let ops = [
        Op::Connect { from: from_path.clone(), output, to: a.path.clone(), input },
        set("trace_tag", a.tag.as_ref().map(|_| Value::Text(tag.to_string())))?,
        set("trace_channel", a.channel.as_ref().map(|c| Value::Text(c.trim().to_string())))?,
        set("trace_threshold", a.threshold.map(Value::Float))?,
        set("trace_invert", a.invert.map(Value::Bool))?,
        set("trace_labels", a.labels.map(Value::Bool))?,
        set("trace_tolerance", a.tolerance.map(Value::Float))?,
        set("trace_min_area", a.min_area.map(Value::Float))?,
    ];
    let label = format!("trace {from_path} live");
    e.session().edit(actor, &ops, None, &label, now)?;

    // The node traces on its own now; its table says what came of it.
    let net = e.session().doc().snapshot();
    let target = require(&net, &a.path)?;
    let (data, status) = cook_now(e, target, TABLE_OUTPUT)?;
    let error = status.as_ref().and_then(|s| s.error.clone());
    let warning = status.as_ref().and_then(|s| s.warning.clone());
    let data = result_of(&a.path, data, error)?;
    let table = data.get::<Table>().ok_or_else(|| CommandError(format!("{} has no shapes table", a.path)))?;
    // The first row names the columns; the second column is the tag.
    let shapes = table.rows().iter().skip(1).filter(|r| r.get(1).is_some_and(|t| t == tag)).count();
    let mut warnings: Vec<String> = warning.into_iter().collect();
    if net.node(target).is_some_and(|n| n.inputs.first().copied().flatten().is_some()) {
        warnings.push(format!("{}: its own shapes are not drawn while its table input is connected", a.path));
    }
    Ok(json!({
        "message": format!("{} traces {from_path} by itself: {shapes} shape(s) tagged `{tag}`", a.path),
        "shapes": shapes,
        "live": true,
        "source": {"path": from_path.to_string(), "output": output},
        "tag": tag,
        "warnings": warnings,
    }))
}
