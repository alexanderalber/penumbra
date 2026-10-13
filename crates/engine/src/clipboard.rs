//! `node.copy`, `node.cut`, `node.paste`. The content work is in `penumbra_project::clipboard`;
//! here a paste becomes one undoable edit, and the engine remembers the text copied last together
//! with what its locked nodes held, so a pasted locked node holds the same picture instead of
//! cooking a new one.

use std::collections::BTreeMap;
use std::sync::Arc;

use penumbra_core::{NodeId, NodePath, lock};
use penumbra_graph::{Data, EditOutcome, Kept, Op};
use penumbra_project::clipboard;
use penumbra_project::{Actor, Content, diff};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::outcome;
use crate::components::edit_as;
use crate::engine::Engine;

type Result<T> = std::result::Result<T, CommandError>;

/// What was copied last.
#[derive(Clone)]
pub struct Clipboard {
    pub text: String,
    /// The outputs locked nodes held when they were copied, by the path they had.
    held: Vec<(NodePath, Vec<Data>)>,
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> Result<Json>| {
        let engine = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    };
    add(
        "node.copy",
        "Copy side-by-side nodes with everything inside them; returns the text; {paths}",
        node_copy,
    );
    add("node.cut", "Copy side-by-side nodes, then delete them (one undo step); {paths}", node_cut);
    add(
        "node.paste",
        "Paste the nodes copied last (or `text`) into a component or the root; {into?, text?}",
        node_paste,
    );
}

/// What copying did.
pub struct Copied {
    pub text: String,
    /// The copied nodes (not what is inside them).
    pub top: Vec<NodePath>,
    /// How many locked nodes' outputs came along.
    pub held: usize,
}

/// Copies `paths` (side by side): the text, which the engine also keeps for [`paste`].
pub fn copy(e: &Engine, paths: &[NodePath]) -> Result<Copied> {
    let net = e.session().doc().snapshot();
    let base = Content::from_network(&net)?;
    let clip = clipboard::copy(&base, paths)?;
    let text = clipboard::print(&clip, e.types())?;
    let locked: Vec<(NodePath, NodeId)> = clip
        .content
        .nodes
        .iter()
        .filter(|(_, n)| n.locked)
        .filter_map(|(p, _)| Some((p.clone(), net.resolve(p)?)))
        .collect();
    let held = if locked.is_empty() {
        Vec::new()
    } else {
        // Handles, no copies; without a cook thread the copies cook their own outputs.
        e.on_cook(move |st| {
            locked
                .into_iter()
                .filter_map(|(path, id)| Some((path, st.cooker.outputs(id)?.to_vec())))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|err| {
            log::warn!("copy: the held outputs of locked nodes did not come along: {err}");
            Vec::new()
        })
    };
    let out = Copied { text: text.clone(), top: clip.top(), held: held.len() };
    *lock(e.clipboard()) = Some(Clipboard { text, held });
    Ok(out)
}

/// The text copied last in this run.
pub fn copied_text(e: &Engine) -> Option<String> {
    lock(e.clipboard()).as_ref().map(|c| c.text.clone())
}

/// Copies `paths`, then deletes them as `actor`.
pub fn cut(e: &Engine, actor: Actor, paths: &[NodePath]) -> Result<(Copied, EditOutcome)> {
    let copied = copy(e, paths)?;
    let ops: Vec<Op> = paths.iter().map(|path| Op::Delete { path: path.clone() }).collect();
    let label = match paths {
        [one] => format!("cut {one}"),
        _ => format!("cut {} nodes", paths.len()),
    };
    let out = edit_as(e, actor, &ops, &label)?;
    Ok((copied, out))
}

/// What pasting did.
pub struct Pasted {
    /// The pasted nodes on the level pasted into.
    pub top: Vec<NodePath>,
    /// Copied path to pasted path of every node.
    pub moved: BTreeMap<NodePath, NodePath>,
    /// Wires and bindings from outside that could not come along.
    pub dropped: Vec<String>,
    pub outcome: EditOutcome,
}

/// Pastes `text` (default: what was copied last) into `into`, as `actor`, as one undo step.
pub fn paste(e: &Engine, actor: Actor, text: Option<&str>, into: &NodePath) -> Result<Pasted> {
    let stored = lock(e.clipboard()).clone();
    let text = match (text, &stored) {
        (Some(t), _) => t.to_string(),
        (None, Some(c)) => c.text.clone(),
        (None, None) => return Err("nothing copied yet".into()),
    };
    let types = e.types();
    let clip = clipboard::parse(&text, types)?;
    let net = e.session().doc().snapshot();
    let base = Content::from_network(&net)?;
    let pasted = clipboard::paste(&base, types, &clip, into)?;
    let ops = diff(&base, &pasted.content);
    let label = match pasted.top.as_slice() {
        [one] => format!("paste {one}"),
        many => format!("paste {} nodes into {into}", many.len()),
    };
    let outcome = edit_as(e, actor, &ops, &label)?;
    // The copies of locked nodes hold what the originals held, when this run copied them.
    if let Some(c) = stored.filter(|c| c.text == text && !c.held.is_empty()) {
        let after = outcome.snapshot.clone();
        let held: Vec<(NodePath, NodeId, Vec<Data>)> = c
            .held
            .into_iter()
            .filter_map(|(old, outs)| {
                let new = pasted.moved.get(&old)?;
                Some((new.clone(), after.resolve(new)?, outs))
            })
            .collect();
        let results = e.on_cook(move |st| {
            // Bring the cooker up to the pasted nodes first, so they can take the outputs.
            st.cooker.cook(&after, st.clock.now(), &[], &[]);
            held.into_iter()
                .map(|(path, id, outs)| (path, st.cooker.restore(id, Kept::Outputs(outs))))
                .collect()
        });
        match results {
            Ok(list) => {
                let list: Vec<(NodePath, std::result::Result<(), String>)> = list;
                for (path, r) in list {
                    if let Err(err) = r {
                        log::warn!("paste: {path} cooks its own outputs: {err}");
                    }
                }
            }
            Err(err) => log::warn!("paste: locked copies cook their own outputs: {err}"),
        }
    }
    Ok(Pasted { top: pasted.top, moved: pasted.moved, dropped: pasted.dropped, outcome })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathsArg {
    paths: Vec<NodePath>,
}

fn copied_json(c: &Copied) -> Json {
    json!({"text": c.text, "nodes": c.top, "held": c.held})
}

fn node_copy(e: &Engine, p: &Json) -> Result<Json> {
    let a: PathsArg = args(p)?;
    Ok(copied_json(&copy(e, &a.paths)?))
}

fn node_cut(e: &Engine, p: &Json) -> Result<Json> {
    let a: PathsArg = args(p)?;
    let (c, out) = cut(e, Actor::Claude, &a.paths)?;
    let mut j = copied_json(&c);
    if let Some(map) = j.as_object_mut() {
        map.insert("history".into(), outcome(&out).get("history").cloned().unwrap_or(Json::Null));
    }
    Ok(j)
}

fn node_paste(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        into: Option<NodePath>,
        text: Option<String>,
    }
    let a: A = args(p)?;
    let into = a.into.unwrap_or_else(NodePath::root);
    let r = paste(e, Actor::Claude, a.text.as_deref(), &into)?;
    let moved: serde_json::Map<String, Json> =
        r.moved.iter().map(|(a, b)| (a.to_string(), json!(b))).collect();
    Ok(json!({
        "pasted": r.top,
        "moved": moved,
        "dropped": r.dropped,
        "history": outcome(&r.outcome).get("history").cloned().unwrap_or(Json::Null),
    }))
}
