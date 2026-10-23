//! Keyboard commands: `keyboard.press` presses keys as if typed in a penumbra window (for tests
//! and remote control), `keyboard.status` says what is held and what was pressed last. The
//! windows themselves feed the hub ([`penumbra_ops::keyboard`]).

use std::sync::Arc;

use penumbra_graph::NodeDef;
use penumbra_ops::keyboard::{DEFAULT_KEYS, KEYBOARD_IN, Key, Source, last_index, parse_keys};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "keyboard.press",
        "Press a key as if typed in a penumbra window, for chan.keyboard_in: {key, down?}; without `down` a tap, \
         `down: true` holds it until `down: false`",
        keyboard_press,
    );
    add(
        "keyboard.status",
        "Keys held now (and by which window), the key pressed last, presses so far",
        keyboard_status,
    );
}

fn keyboard_press(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        key: String,
        down: Option<bool>,
    }
    let a: A = args(p)?;
    let key = Key::parse(&a.key).ok_or_else(|| {
        format!(
            "`{}` is no key: digits, letters, f1 to f12, space, left right up down, enter, tab, ...",
            a.key
        )
    })?;
    let kb = e.keyboard();
    match a.down {
        None => kb.tap(key),
        Some(down) => kb.key(Source::Command, key.name(), Some(key), down),
    }
    Ok(json!({"key": key.name(), "status": kb.status()}))
}

fn keyboard_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    Ok(serde_json::to_value(e.keyboard().status()).map_err(|e| e.to_string())?)
}

/// What `node.info` adds for a `chan.keyboard_in`: its keys, which are held, and `last`.
pub fn info(e: &Engine, node: &NodeDef) -> Option<Json> {
    if node.kind != KEYBOARD_IN {
        return None;
    }
    let session = e.session();
    let desc = session.types().desc(KEYBOARD_IN)?;
    let text = node.param(desc, "keys").and_then(|v| v.as_str()).unwrap_or(DEFAULT_KEYS).to_string();
    let (keys, _) = parse_keys(&text);
    let views = e.keyboard().view(&keys);
    let held: Vec<&str> = keys.iter().zip(&views).filter(|(_, v)| v.held).map(|(k, _)| k.name()).collect();
    Some(json!({
        "keys": keys.iter().map(|k| k.name()).collect::<Vec<_>>(),
        "held": held,
        "last": last_index(&views),
        "passed_on": e.keyboard().wanted(),
    }))
}
