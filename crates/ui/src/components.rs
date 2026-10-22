//! The editor's side of components: grouping, dissolving, saving and adding through
//! `penumbra_engine::components`, as the user, with the nodes keeping their places on screen.
//! Each is one undo step: the nodes' new places join the entry of the edit that moved them.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use egui::{Pos2, Rect, pos2, vec2};
use penumbra_core::{Error, NodeId, NodePath};
use penumbra_engine::Engine;
use penumbra_engine::components as comps;
use penumbra_graph::EditOutcome;
use penumbra_project::Actor;

use crate::editor::{Editor, level_path};
use crate::layout::{Layout, NODE_SIZE, place_ops};

fn err(e: penumbra_engine::CommandError) -> Error {
    Error::new(e.0)
}

/// Positions by path of the nodes about to move, read before the edit.
fn positions_by_path(
    engine: &Engine,
    layout: &Layout,
    paths: &BTreeMap<NodePath, NodePath>,
) -> BTreeMap<NodePath, Pos2> {
    let net = engine.session().doc().snapshot();
    paths.keys().filter_map(|p| Some((p.clone(), layout.get(net.resolve(p)?)?))).collect()
}

/// Puts each node at `new path` to `pos`, in the network `out` left, joining its history entry.
fn place(
    engine: &Engine,
    out: &EditOutcome,
    at: impl IntoIterator<Item = (NodePath, Pos2)>,
    now: Instant,
) -> Result<(), Error> {
    let net = &out.snapshot;
    let to: HashMap<NodeId, Pos2> =
        at.into_iter().filter_map(|(path, pos)| Some((net.resolve(&path)?, pos))).collect();
    let ops = place_ops(net, &to);
    if !ops.is_empty() {
        engine.session().edit(Actor::User, &ops, Some(out.group), "", now)?;
    }
    Ok(())
}

/// Groups `ids` into a new component, keeping each node's place inside it; the component sits
/// where they were and is selected.
pub fn group(
    engine: &Engine,
    editor: &mut Editor,
    layout: &Layout,
    ids: &[NodeId],
    now: Instant,
) -> Result<(), Error> {
    let net = engine.session().doc().snapshot();
    let paths: Vec<NodePath> = ids.iter().filter_map(|id| net.path_of(*id)).collect();
    // Every node that will move, with its descendants, by old path; known after the edit, so
    // positions are read now for all nodes below the members.
    let before: BTreeMap<NodePath, Pos2> = net
        .nodes()
        .filter_map(|n| {
            let p = net.path_of(n.id)?;
            let inside = paths.iter().any(|m| p.names().starts_with(m.names()));
            inside.then(|| Some((p, layout.get(n.id)?))).flatten()
        })
        .collect();
    let members: Vec<Pos2> = paths.iter().filter_map(|p| before.get(p).copied()).collect();
    let g = comps::collapse(engine, Actor::User, &paths, None).map_err(err)?;
    let bounds = members.iter().map(|p| Rect::from_min_size(*p, NODE_SIZE)).reduce(|a, b| a.union(b));
    let mut at: Vec<(NodePath, Pos2)> =
        g.moved.iter().filter_map(|(old, new)| Some((new.clone(), *before.get(old)?))).collect();
    if let Some(b) = bounds {
        at.push((g.path.clone(), b.center() - NODE_SIZE / 2.0));
        // Input nodes in a column left of the moved nodes, outputs right of them.
        let net = engine.session().doc().snapshot();
        let types = engine.types();
        let (mut row_in, mut row_out) = (0.0, 0.0);
        for port in &g.ports {
            let role = net.resolve(port).and_then(|id| net.port_role(types, id));
            let step = NODE_SIZE.y * 0.6 + 24.0;
            match role {
                Some(penumbra_graph::PortRole::In) => {
                    at.push((port.clone(), pos2(b.min.x - NODE_SIZE.x - 60.0, b.min.y + row_in)));
                    row_in += step;
                }
                _ => {
                    at.push((port.clone(), pos2(b.max.x + 60.0, b.min.y + row_out)));
                    row_out += step;
                }
            }
        }
    }
    place(engine, &g.outcome, at, now)?;
    if let Some(id) = engine.session().doc().snapshot().resolve(&g.path) {
        editor.select([id]);
    }
    Ok(())
}

/// Dissolves component `id`: its nodes come up where it sat, in their arrangement from inside.
pub fn dissolve(
    engine: &Engine,
    editor: &mut Editor,
    layout: &Layout,
    id: NodeId,
    now: Instant,
) -> Result<(), Error> {
    let net = engine.session().doc().snapshot();
    let path = net.path_of(id).ok_or_else(|| Error::new("the component is gone"))?;
    let comp_at = layout.get(id).unwrap_or(pos2(0.0, 0.0));
    // Read the positions of everything inside first (by path), the move map comes with the edit.
    let inside: BTreeMap<NodePath, NodePath> = net
        .nodes()
        .filter_map(|n| {
            let p = net.path_of(n.id)?;
            (p.names().len() > path.names().len() && p.names().starts_with(path.names()))
                .then(|| (p.clone(), p))
        })
        .collect();
    let before = positions_by_path(engine, layout, &inside);
    let (moved, out) = comps::expand(engine, Actor::User, &path).map_err(err)?;
    // The direct children land centred where the component was: grouping put the component
    // at the centre of its nodes, so grouping and dissolving again puts everything back.
    let direct = moved
        .keys()
        .filter(|old| old.parent().as_ref() == Some(&path))
        .filter_map(|old| before.get(old).map(|p| Rect::from_min_size(*p, NODE_SIZE)))
        .reduce(|a, b| a.union(b));
    let offset = direct.map_or(vec2(0.0, 0.0), |b| (comp_at + NODE_SIZE / 2.0) - b.center());
    let at: Vec<(NodePath, Pos2)> = moved
        .iter()
        .filter_map(|(old, new)| {
            let p = *before.get(old)?;
            let shifted = if old.parent().as_ref() == Some(&path) { p + offset } else { p };
            Some((new.clone(), shifted))
        })
        .collect();
    place(engine, &out, at, now)?;
    let net = engine.session().doc().snapshot();
    editor.select(
        moved
            .iter()
            .filter(|(old, _)| old.parent().as_ref() == Some(&path))
            .filter_map(|(_, new)| net.resolve(new)),
    );
    Ok(())
}

/// Writes component `id` to the library; the message for the status bar.
pub fn save(engine: &Engine, id: NodeId) -> Result<String, Error> {
    let net = engine.session().doc().snapshot();
    let path = net.path_of(id).ok_or_else(|| Error::new("the component is gone"))?;
    let saved = comps::save(engine, &path, None).map_err(err)?;
    let mut text = format!("saved {path} to {}", saved.file.display());
    if !saved.dropped.is_empty() {
        text.push_str(&format!(
            "; left out {} reference(s) to nodes outside it: {}",
            saved.dropped.len(),
            saved.dropped.join(", ")
        ));
    }
    Ok(text)
}

/// Takes the component file `name` out of the library, the one the add dialog offers (the
/// project's before the user's); the message for the status bar.
pub fn remove(engine: &Engine, name: &str) -> Result<String, Error> {
    let to = comps::remove(engine, name, None).map_err(err)?;
    Ok(format!("took `{name}` out of the library; the file is now {}", to.display()))
}

/// Adds the library's component `name` inside `level` at `at`, selected.
pub fn add(
    engine: &Engine,
    editor: &mut Editor,
    name: &str,
    level: Option<NodeId>,
    at: Pos2,
    now: Instant,
) -> Result<(), Error> {
    let net = engine.session().doc().snapshot();
    let level_path = level_path(&net, level);
    let (path, out) = comps::load(engine, Actor::User, name, None, &level_path).map_err(err)?;
    place(engine, &out, [(path.clone(), at)], now)?;
    if let Some(id) = engine.session().doc().snapshot().resolve(&path) {
        editor.select([id]);
    }
    Ok(())
}

/// The component files on offer and where each lies (project first, then the library; a name
/// in both is the project's, as `comp.load` picks it).
pub fn library_names(engine: &Engine) -> Vec<crate::add_dialog::Offered> {
    let mut seen = std::collections::BTreeSet::new();
    comps::library(engine)
        .into_iter()
        .filter(|e| seen.insert(e.name.clone()))
        .map(|e| crate::add_dialog::Offered { name: e.name, place: e.place })
        .collect()
}
