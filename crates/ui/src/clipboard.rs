//! The editor's side of copy and paste: through `penumbra_engine::clipboard`, as the user, with the
//! copies keeping the arrangement of the originals around the pointer.

use std::collections::BTreeMap;
use std::time::Instant;

use egui::{Pos2, Rect, vec2};
use penumbra_core::{Error, NodeId, NodePath};
use penumbra_engine::Engine;
use penumbra_engine::clipboard as clip;
use penumbra_project::Actor;

use crate::editor::{Editor, level_path};
use crate::layout::{Layout, NODE_SIZE};

/// What this run copied last: the text and where every copied node sat, by path.
pub struct Copied {
    pub text: String,
    /// How many nodes were copied (not counting what is inside them).
    pub count: usize,
    positions: BTreeMap<NodePath, Pos2>,
}

fn err(e: penumbra_engine::CommandError) -> Error {
    Error::new(e.0)
}

/// The paths of `ids` and the positions of everything at or inside them, by path.
fn gather(engine: &Engine, layout: &Layout, ids: &[NodeId]) -> (Vec<NodePath>, BTreeMap<NodePath, Pos2>) {
    let net = engine.session().doc().snapshot();
    let paths: Vec<NodePath> = ids.iter().filter_map(|id| net.path_of(*id)).collect();
    let positions = net
        .nodes()
        .filter_map(|n| {
            let p = net.path_of(n.id)?;
            let inside = paths.iter().any(|m| p.names().starts_with(m.names()));
            inside.then(|| Some((p, layout.get(n.id)?))).flatten()
        })
        .collect();
    (paths, positions)
}

/// Copies `ids` (side by side); the text for the system clipboard comes back in [`Copied`].
pub fn copy(engine: &Engine, layout: &Layout, ids: &[NodeId]) -> Result<Copied, Error> {
    let (paths, positions) = gather(engine, layout, ids);
    let copied = clip::copy(engine, &paths).map_err(err)?;
    Ok(Copied { count: copied.top.len(), text: copied.text, positions })
}

/// Copies `ids`, then deletes them (one undo step).
pub fn cut(engine: &Engine, layout: &Layout, ids: &[NodeId]) -> Result<Copied, Error> {
    let (paths, positions) = gather(engine, layout, ids);
    let (copied, _) = clip::cut(engine, Actor::User, &paths).map_err(err)?;
    Ok(Copied { count: copied.top.len(), text: copied.text, positions })
}

/// Pastes `text` (default: what this run copied last) inside `level`, the copies centred on
/// `at` in the arrangement of the originals when this run copied them, else side by side. The
/// copies become the selection; returns a message when wires or bindings were left out.
#[allow(clippy::too_many_arguments)]
pub fn paste(
    engine: &Engine,
    editor: &mut Editor,
    layout: &mut Layout,
    copied: Option<&Copied>,
    text: Option<&str>,
    level: Option<NodeId>,
    at: Pos2,
    now: Instant,
) -> Result<Option<String>, Error> {
    let text = text.or(copied.map(|c| c.text.as_str()));
    let net = engine.session().doc().snapshot();
    let into = level_path(&net, level);
    let pasted = clip::paste(engine, Actor::User, text, &into).map_err(err)?;
    let known = copied.filter(|c| text == Some(c.text.as_str())).map(|c| &c.positions);
    let top_old: Vec<&NodePath> =
        pasted.moved.iter().filter(|(_, new)| pasted.top.contains(new)).map(|(old, _)| old).collect();
    let bounds = known.and_then(|pos| {
        top_old
            .iter()
            .filter_map(|old| pos.get(*old))
            .map(|p| Rect::from_min_size(*p, NODE_SIZE))
            .reduce(|a, b| a.union(b))
    });
    let net = engine.session().doc().snapshot();
    let mut next = at - NODE_SIZE / 2.0;
    for (old, new) in &pasted.moved {
        let Some(id) = net.resolve(new) else { continue };
        let was = known.and_then(|pos| pos.get(old)).copied();
        let pos = match (was, bounds) {
            // The copied nodes themselves move to the pointer; what is inside them keeps its
            // place on its own plane.
            (Some(p), Some(b)) if pasted.top.contains(new) => p + (at - b.center()),
            (Some(p), _) => p,
            (None, _) if pasted.top.contains(new) => {
                let p = next;
                next += vec2(NODE_SIZE.x + 40.0, 0.0);
                p
            }
            (None, _) => continue,
        };
        layout.set(id, pos, now);
    }
    editor.selection = pasted.top.iter().filter_map(|p| net.resolve(p)).collect();
    if pasted.dropped.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("pasted {} node(s), without {}", pasted.top.len(), pasted.dropped.join(", "))))
}
