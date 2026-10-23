//! Looking around in 3D: the viewer's free view drawn on demand, and the 3D mouse.
//!
//! Commands: `view.free` (a node's scene from any camera, as a PNG), `spacenav.status`.

use std::path::PathBuf;
use std::sync::Arc;

use penumbra_core::NodePath;
use penumbra_ops::freeview::{FreeView, Request};
use penumbra_ops::view::Orbit;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{cook_now, engine_commands, require};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "view.free",
        "A render's or a geometry's scene from any camera, as the viewer's free view draws it (without its lines); \
         {path, output?, camera?: {eye, target, roll, fov, ortho?: height}, width?, height?, file?}; no camera starts where the viewer does",
        view_free,
    );
    add(
        "spacenav.status",
        "The 3D mouse: state (searching, connected, failed), raw counts, resting offset, shaped axes, buttons; opens it if not yet",
        spacenav_status,
    );
}

fn view_free(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        #[serde(default)]
        output: u32,
        camera: Option<Orbit>,
        width: Option<u32>,
        height: Option<u32>,
        file: Option<PathBuf>,
    }
    let a: A = args(p)?;
    let gpu = e.gpu().ok_or("no GPU yet (the window opens it)")?.clone();
    let net = e.session().doc().snapshot();
    let id = require(&net, &a.path)?;
    if !penumbra_ops::freeview::available(&net, e.types(), id, a.output) {
        return Err(format!("{} has no 3D scene to look around in (a tex.render or geometry)", a.path).into());
    }
    if let Some(c) = &a.camera
        && !c.is_valid()
    {
        return Err("`camera` needs finite numbers and an eye apart from the target".into());
    }
    // Cook it (a render then also publishes how it drew).
    cook_now(e, id, a.output)?;
    let request = Request {
        node: id,
        output: a.output,
        camera: a.camera,
        size: (a.width.unwrap_or(960).clamp(16, 4096), a.height.unwrap_or(540).clamp(16, 4096)),
    };
    let session = e.session().clone();
    let drawn = e.on_cook(move |st| {
        let net = session.doc().snapshot();
        let gpu = gpu.clone();
        FreeView::new().draw(&gpu, &net, &st.cooker, &st.renders, &request).map(|pic| (pic, gpu))
    })?;
    let (picture, gpu) = drawn.map_err(|e| format!("{}: {}", a.path, e.0))?;
    let pixels = gpu.read(&picture.tex)?;
    let file = match a.file {
        Some(f) => f,
        None => {
            let name = a.path.to_string().trim_start_matches('/').replace('/', "_");
            std::env::temp_dir().join("penumbra").join("snapshots").join(format!("{name}_free.png"))
        }
    };
    if let Some(dir) = file.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&file, pixels.to_png()?).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(json!({
        "file": file.display().to_string(),
        "width": pixels.width,
        "height": pixels.height,
        "camera": picture.camera,
        "home": picture.home,
        "lines": picture.lines.len(),
    }))
}

fn spacenav_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let nav = e.spacenav();
    nav.start();
    Ok(serde_json::to_value(nav.status()).map_err(|e| e.to_string())?)
}
