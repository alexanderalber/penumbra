//! `chan.press`: presses a `chan.button` as a click on its node body does, for Claude, scripts
//! and tests. A momentary button is held in the [`Buttons`] hub (live, no history); a toggle's
//! press flips its `value` as an edit. A slider needs no command of its own: `node.set` sets its
//! `value`.
//!
//! The controls flagged `show` also appear on the show screen ([`show_controls`]). While show
//! mode locks the project, their values may still change ([`set_in_show`]): from the show
//! screen, `chan.press` and `node.set` alike. Such a change makes no history entry and is not
//! written until show mode ends.

use std::collections::BTreeMap;
use std::sync::Arc;

use penumbra_core::{Error, Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::{Network, NodeDef, NodeStatus, NodeTypes, Op};
use penumbra_ops::chan::controls::{BUTTON, Buttons, Holder, MOMENTARY, SHOW, SLIDER, TOGGLE, slider_value};
use penumbra_project::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{edit, engine_commands, outcome, require};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "chan.press",
        "Press a chan.button as a click on its node does: {path, down?}. Momentary: without `down` a tap \
         (1 for one frame), `down: true` holds it until `down: false`; no history entry. Toggle: a press \
         flips `value` (an edit), `down: false` does nothing",
        chan_press,
    );
}

/// The button's `mode`: momentary or toggle.
pub fn mode_of(e: &Engine, node: &NodeDef) -> &'static str {
    let toggle = e
        .types()
        .desc(BUTTON)
        .and_then(|d| node.param(d, "mode"))
        .and_then(Value::as_str)
        .is_some_and(|m| m == TOGGLE);
    if toggle { TOGGLE } else { MOMENTARY }
}

fn chan_press(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        down: Option<bool>,
    }
    let a: A = args(p)?;
    let net = e.session().doc().snapshot();
    let id = require(&net, &a.path)?;
    let Some(node) = net.node(id) else { return Err(format!("no node at {}", a.path).into()) };
    if node.kind != BUTTON {
        return Err(format!("{} is a {}, not a {BUTTON}", a.path, node.kind).into());
    }
    let mode = mode_of(e, node);
    if mode == MOMENTARY {
        let hub: &Buttons = e.buttons();
        match a.down {
            None => hub.tap(id, Holder::Command),
            Some(down) => {
                hub.hold(id, Holder::Command, down);
            }
        }
        let view = hub.view(id);
        return Ok(
            json!({"path": a.path.to_string(), "mode": mode, "held": view.held, "presses": view.presses}),
        );
    }
    let on =
        e.types().desc(BUTTON).and_then(|d| node.param(d, "value")).and_then(Value::as_bool).unwrap_or(false);
    if a.down == Some(false) {
        return Ok(json!({"path": a.path.to_string(), "mode": mode, "value": on, "history": "no_change"}));
    }
    if node.is_bound("value") {
        return Err(format!(
            "{}.value is bound to a channel: unbind it to switch the button by hand",
            a.path
        )
        .into());
    }
    let param = ParamPath::new(a.path.clone(), Name::new("value")?)?;
    let ops = [Op::Set { param, value: Value::Bool(!on) }];
    if set_in_show(&e.session(), &ops)? {
        return Ok(json!({"path": a.path.to_string(), "mode": mode, "value": !on, "history": SHOW_HISTORY}));
    }
    let out = edit(e, &ops, "")?;
    let mut reply = outcome(&out);
    if let Some(o) = reply.as_object_mut() {
        o.insert("path".into(), json!(a.path.to_string()));
        o.insert("mode".into(), json!(mode));
        o.insert("value".into(), json!(!on));
    }
    Ok(reply)
}

/// What `node.info` adds for a `chan.button`: its mode, and for a momentary one whether it is
/// held and how often it was pressed.
pub fn info(e: &Engine, node: &NodeDef) -> Option<Json> {
    if node.kind != BUTTON {
        return None;
    }
    let mode = mode_of(e, node);
    let view = e.buttons().view(node.id);
    Some(json!({"mode": mode, "held": view.held, "presses": view.presses}))
}

/// The `history` of a reply to a change [`set_in_show`] made: none, the change waits for the end
/// of show mode.
pub const SHOW_HISTORY: &str = "none_in_show_mode";

/// A control flagged `show`, as the show screen and `show.status` list it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShowControl {
    #[serde(skip)]
    pub id: NodeId,
    pub path: String,
    pub kind: String,
    /// The name on the show screen: `label`, else the node's name; inside a component the
    /// component's path comes first (`stage/fade`).
    pub label: String,
    /// `momentary` or `toggle` for a button, `None` for a slider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    /// The channel's value now as the control knows it: 1 or 0 for a button (a momentary one
    /// while held), the slider's value kept between `min` and `max`.
    pub value: f64,
    /// `value` follows a channel, so the control cannot set it.
    pub bound: bool,
}

/// The value of param `name` of `node`, its default when it is not set.
fn param<'a>(types: &'a NodeTypes, node: &'a NodeDef, name: &str) -> Option<&'a Value> {
    types.desc(&node.kind).and_then(|d| node.param(d, name))
}

/// Whether `node` is a `chan.button` or `chan.slider` flagged `show`.
pub fn on_show_screen(types: &NodeTypes, node: &NodeDef) -> bool {
    (node.kind == BUTTON || node.kind == SLIDER)
        && param(types, node, SHOW).and_then(Value::as_bool).unwrap_or(false)
}

/// Whether any control in `net` is flagged `show`.
pub fn any_on_show_screen(net: &Network, types: &NodeTypes) -> bool {
    net.nodes().any(|n| on_show_screen(types, n))
}

/// Every control flagged `show`, in components too, in the order the show screen shows them:
/// by position, top to bottom, then left to right. A control inside a component sorts at the
/// component's place, then by its own place in it. `status` (the cook's, when there is one)
/// gives the values of bound params.
pub fn show_controls(
    net: &Network,
    types: &NodeTypes,
    hub: &Buttons,
    status: Option<&BTreeMap<NodeId, NodeStatus>>,
) -> Vec<ShowControl> {
    // Positions from the top level down to the node; an unplaced node sorts last on its level.
    let key = |node: &NodeDef| -> Vec<(f32, f32)> {
        let mut key = Vec::new();
        let mut at = Some(node);
        while let Some(n) = at {
            let [x, y] = n.pos.unwrap_or([f32::MAX, f32::MAX]);
            key.push((y, x));
            at = n.parent.and_then(|p| net.node(p)).map(|p| p.as_ref());
        }
        key.reverse();
        key
    };
    let mut found: Vec<(Vec<(f32, f32)>, ShowControl)> = net
        .nodes()
        .filter(|n| on_show_screen(types, n))
        .filter_map(|n| {
            let path = net.path_of(n.id)?;
            let cooked = status.and_then(|s| s.get(&n.id));
            let param = |name: &str| cooked.and_then(|s| s.bound.get(name)).or_else(|| param(types, n, name));
            let text = |name: &str| param(name).and_then(Value::as_str).unwrap_or_default().trim();
            let float = |name: &str, or: f64| param(name).and_then(Value::as_f64).unwrap_or(or);
            let own = if text("label").is_empty() { n.name.as_str() } else { text("label") };
            let label = match path.parent().filter(|p| !p.is_root()) {
                Some(parent) => format!("{}/{own}", parent.to_string().trim_start_matches('/')),
                None => own.to_string(),
            };
            let (mode, value) = if n.kind == BUTTON {
                let toggle = text("mode") == TOGGLE;
                let on = if toggle {
                    param("value").and_then(Value::as_bool).unwrap_or(false)
                } else {
                    hub.view(n.id).held
                };
                (Some(if toggle { TOGGLE } else { MOMENTARY }), if on { 1.0 } else { 0.0 })
            } else {
                (None, slider_value(float("value", 0.0), float("min", 0.0), float("max", 1.0)))
            };
            let bound = n.is_bound("value") && mode != Some(MOMENTARY);
            let control = ShowControl {
                id: n.id,
                path: path.to_string(),
                kind: n.kind.clone(),
                label,
                mode,
                value,
                bound,
            };
            Some((key(n), control))
        })
        .collect();
    found.sort_by(|(a, ca), (b, cb)| {
        let positions = a.iter().zip(b).find_map(|((ay, ax), (by, bx))| {
            let o = ay.total_cmp(by).then(ax.total_cmp(bx));
            o.is_ne().then_some(o)
        });
        positions.unwrap_or_else(|| a.len().cmp(&b.len())).then_with(|| ca.path.cmp(&cb.path))
    });
    found.into_iter().map(|(_, c)| c).collect()
}

/// The values that may change in show mode, by node path: the `value` of every toggle button
/// and slider flagged `show` whose `value` follows no channel. A momentary button has no value
/// to keep (it is held, not set). Show mode keeps these across a crash ([`crate::show_values`]).
pub fn kept_values(net: &Network, types: &NodeTypes) -> BTreeMap<String, Value> {
    net.nodes()
        .filter(|n| on_show_screen(types, n) && !n.is_bound("value"))
        .filter(|n| n.kind == SLIDER || param(types, n, "mode").and_then(Value::as_str) == Some(TOGGLE))
        .filter_map(|n| Some((net.path_of(n.id)?.to_string(), param(types, n, "value")?.clone())))
        .collect()
}

/// While show mode locks `session`, sets the values `ops` set if each sets the `value` of a
/// toggle button or slider flagged `show`: live, without a history entry, written to the file
/// after show mode ends. Returns `Ok(false)` and does nothing when the project is not locked
/// or `ops` change anything else; the caller then edits as usual (and the locked project
/// refuses it).
pub fn set_in_show(session: &Session, ops: &[Op]) -> Result<bool, Error> {
    if !session.locked() || ops.is_empty() {
        return Ok(false);
    }
    let net = session.doc().snapshot();
    let types = session.types();
    let allowed = ops.iter().all(|op| {
        let Op::Set { param: p, .. } = op else { return false };
        if p.param().as_str() != "value" {
            return false;
        }
        let Some(node) = net.resolve(p.node()).and_then(|id| net.node(id)) else { return false };
        let momentary =
            node.kind == BUTTON && param(types, node, "mode").and_then(Value::as_str) != Some(TOGGLE);
        on_show_screen(types, node) && !momentary
    });
    if !allowed {
        return Ok(false);
    }
    session.edit_locked(ops)?;
    Ok(true)
}
