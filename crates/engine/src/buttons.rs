//! `chan.press`: presses a `chan.button` as a click on its node body does, for Claude, scripts
//! and tests. A momentary button is held in the [`Buttons`] hub (live, no history); a toggle's
//! press flips its `value` as an edit. A slider needs no command of its own: `node.set` sets its
//! `value`.

use std::sync::Arc;

use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::{NodeDef, Op};
use penumbra_ops::chan::controls::{BUTTON, Buttons, Holder, MOMENTARY, TOGGLE};
use serde::Deserialize;
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
    let out = edit(e, &[Op::Set { param, value: Value::Bool(!on) }], "")?;
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
