//! Mouse commands: `mouse.status` says where the pointer is and which displays it can be on,
//! `mouse.fake` puts a pointer in place of the real one (for tests and remote control) and turns
//! the wheel. The nodes read the pointer themselves ([`penumbra_ops::mouse`]).

use std::sync::Arc;

use penumbra_ops::mouse::{MOUSE_IN, Mouse};
use penumbra_system::pointer::{LEFT, MIDDLE, Pointer, RIGHT};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "mouse.status",
        "The mouse pointer as chan.mouse_in sees it (desktop pixels, buttons, faked or real), the wheel so far, the displays' rectangles",
        mouse_status,
    );
    add(
        "mouse.fake",
        "Stand in for the mouse, for chan.mouse_in: {x?, y?, buttons?: [left, right, middle], wheel?: notches}; \
         x and y in desktop pixels put a fake pointer in place of the real one, {real: true} brings the real one back; \
         wheel turns it by that many notches (up positive)",
        mouse_fake,
    );
}

fn mouse_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    Ok(serde_json::to_value(e.mouse().status()).map_err(|e| e.to_string())?)
}

fn mouse_fake(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        x: Option<i32>,
        y: Option<i32>,
        #[serde(default)]
        buttons: Vec<String>,
        wheel: Option<f64>,
        #[serde(default)]
        real: bool,
    }
    let a: A = args(p)?;
    let mouse: &Mouse = e.mouse();
    if a.real {
        if a.x.is_some() || a.y.is_some() || !a.buttons.is_empty() {
            return Err("`real` brings the real pointer back; it takes no x, y or buttons".into());
        }
        mouse.fake(None);
    } else if a.x.is_some() || a.y.is_some() || !a.buttons.is_empty() {
        let (Some(x), Some(y)) = (a.x, a.y) else {
            return Err("a fake pointer needs both `x` and `y`".into());
        };
        let mut bits = 0;
        for b in &a.buttons {
            bits |= match b.as_str() {
                "left" => LEFT,
                "right" => RIGHT,
                "middle" => MIDDLE,
                other => return Err(format!("`{other}` is no button: left, right, middle").into()),
            };
        }
        mouse.fake(Some(Pointer { x, y, buttons: bits }));
    }
    if let Some(w) = a.wheel {
        if !w.is_finite() || w.abs() > 1e6 {
            return Err("`wheel` must be a finite number of notches".into());
        }
        mouse.wheel(w);
    }
    let nodes = e.session().doc().snapshot().nodes().filter(|n| n.kind == MOUSE_IN).count();
    Ok(json!({"status": mouse.status(), "nodes": nodes}))
}
