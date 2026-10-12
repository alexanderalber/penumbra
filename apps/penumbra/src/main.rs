//! penumbra app. Started without arguments it opens the demo project, the control channel and
//! a window, plus a fullscreen output window per display that an active `out.projector` names.
//!
//! Arguments: `--project DIR` (project directory, created if missing; default: the demo project
//! in the user's data directory; the project menu opens others while it runs), `--port N`
//! (control channel port, default from `PENUMBRA_PORT` or 9880), `--fresh` (discard what
//! feedback loops, caches and other nodes kept from the last run; locked nodes keep their
//! outputs).
//!
//! `--headless SCRIPT` runs the UI without a window, driven by a script of control-channel
//! requests (see `penumbra_ui::headless`), prints each reply and exits; `ui.screenshot` in
//! the script saves what the window would show. Without `--project` it works on a fresh copy
//! of the demo in the temp directory, so the real demo project is left alone. `--software`
//! renders on WARP instead of the GPU, `--size WxH` sets the window size in points.

mod host;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui;
use penumbra_engine::{Engine, control, projects};
use penumbra_project::{Content, Session};
use penumbra_ui::{AppOptions, Bridge, PenumbraApp};
use serde_json::Value;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

struct Args {
    port: u16,
    project: Option<PathBuf>,
    headless: Option<PathBuf>,
    software: bool,
    size: Option<[f32; 2]>,
    fresh: bool,
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;
    let types = Arc::new(penumbra_ops::builtin_types().map_err(|e| e.to_string())?);
    let mut scratch = None;
    let (dir, template) = match (&args.project, &args.headless) {
        (Some(dir), _) => (dir.clone(), Content::default()),
        (None, Some(_)) => {
            let dir = std::env::temp_dir().join("penumbra").join(format!("headless-{}", std::process::id()));
            // A leftover from an earlier run with the same process id would not be the demo.
            let _ = std::fs::remove_dir_all(&dir);
            scratch = Some(dir.clone());
            let template = projects::prepare_demo(&dir, &types).map_err(|e| e.0)?;
            (dir, template)
        }
        (None, None) => {
            let dir = projects::demo_dir().map_err(|e| format!("{e}; pass --project DIR"))?;
            let template = projects::prepare_demo(&dir, &types).map_err(|e| e.0)?;
            (dir, template)
        }
    };
    let session = Arc::new(Session::open(&dir, types, &template).map_err(|e| e.to_string())?);
    log::info!("project: {}", session.file().display());
    let engine = Engine::start(session).map_err(|e| e.to_string())?;
    if args.fresh {
        // Before the GPU arrives, so the start restore leaves the state out.
        engine.state().start_fresh();
    }

    if let Some(script) = &args.headless {
        let result = headless(&engine, script, &args);
        engine.shutdown();
        if let Some(dir) = scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
        return result;
    }

    match projects::default_recent_file() {
        Ok(file) => engine.set_recent_file(Some(file)),
        Err(e) => log::warn!("no list of recent projects: {e}"),
    }
    engine.remember(&dir);

    let (bridge, requests) = Bridge::new();
    let mut registry = penumbra_engine::engine_registry(&engine);
    penumbra_ui::control::register(&mut registry, &bridge);
    let registry = Arc::new(registry);
    let handler = Arc::clone(&registry);
    let control = match control::serve(args.port, Arc::new(move |m: &str, p: &Value| handler.dispatch(m, p)))
    {
        Ok(addr) => {
            log::info!("control channel listening on {addr}");
            Ok(addr.port())
        }
        Err(e) => {
            // The editor still works without it; say so instead of refusing to start.
            log::error!("control channel unavailable on 127.0.0.1:{}: {e}", args.port);
            Err(format!("127.0.0.1:{}: {e}", args.port))
        }
    };

    let mut viewport =
        egui::ViewportBuilder::default().with_title("penumbra").with_inner_size([1600.0, 1000.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let mut options =
        eframe::NativeOptions { viewport, renderer: eframe::Renderer::Wgpu, ..Default::default() };
    // DX12 on Windows: with Vulkan, one shared device throttles the UI to the projectors' vsync
    // (measured). `WGPU_BACKEND` still overrides; Linux keeps Vulkan.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
            setup.instance_descriptor.backends = wgpu::Backends::DX12;
        }
        // The same device serves the cook thread (textures up to 16384 pixels).
        setup.device_descriptor = Arc::new(penumbra_gpu::device_descriptor);
    }
    let result = run_window(&engine, options, control, requests, bridge);
    // Normally done in `on_exit` already; repeated here for a window that failed to open.
    engine.shutdown();
    result
}

/// The window icon, chosen once at start for the taskbar's colour: a plain sphere on a light
/// taskbar, the sphere in a ring on a dark one or when the colour is unknown (the icon in the
/// exe). `None` leaves eframe's default.
fn app_icon() -> Option<Arc<egui::IconData>> {
    let png: &[u8] = if penumbra_system::appearance::taskbar_is_light() == Some(true) {
        include_bytes!("../../../assets/app-icon/penumbra-light.png")
    } else {
        include_bytes!("../../../assets/app-icon/penumbra.png")
    };
    match eframe::icon_data::from_png_bytes(png) {
        Ok(icon) => Some(Arc::new(icon)),
        Err(e) => {
            log::warn!("no app icon: {e}");
            None
        }
    }
}

/// Runs the editor window and the output windows until the editor closes. The event loop is
/// ours, not eframe's, so the output windows' events stay with us (see `host`).
fn run_window(
    engine: &Arc<Engine>,
    options: eframe::NativeOptions,
    control: Result<u16, String>,
    requests: std::sync::mpsc::Receiver<penumbra_ui::UiRequest>,
    bridge: Arc<Bridge>,
) -> Result<(), String> {
    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot start the event loop: {e}"))?;
    let instance: host::InstanceSlot = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&instance);
    let app_engine = Arc::clone(engine);
    let eframe_app = eframe::create_native(
        "penumbra",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(rs.instance.clone());
            }
            let app_options =
                AppOptions { control, requests: Some(requests), bridge: Some(bridge), spacenav: true };
            Ok(Box::new(PenumbraApp::new(cc, app_engine, app_options)))
        }),
        &event_loop,
    );
    let mut host = host::Host::new(eframe_app, Arc::clone(engine), instance);
    let result = event_loop.run_app(&mut host).map_err(|e| format!("the window failed: {e}"));
    host.outputs.shutdown();
    result
}

fn headless(engine: &Arc<Engine>, script: &Path, args: &Args) -> Result<(), String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let mut options = penumbra_ui::headless::Options { software: args.software, ..Default::default() };
    if let Some(size) = args.size {
        options.size = size;
    }
    for (method, reply) in penumbra_ui::headless::run(engine.clone(), &text, &options)? {
        println!("{method}: {reply}");
    }
    Ok(())
}

fn parse_size(text: &str) -> Result<[f32; 2], String> {
    let bad = || format!("--size wants WIDTHxHEIGHT in points, e.g. 1600x1000, not `{text}`");
    let (w, h) = text.split_once('x').ok_or_else(bad)?;
    let w: f32 = w.parse().map_err(|_| bad())?;
    let h: f32 = h.parse().map_err(|_| bad())?;
    if !(200.0..=8192.0).contains(&w) || !(200.0..=8192.0).contains(&h) {
        return Err(bad());
    }
    Ok([w, h])
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut port = None;
    let mut parsed =
        Args { port: 0, project: None, headless: None, software: false, size: None, fresh: false };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args.next().ok_or("--port needs a value")?;
                port = Some(control::parse_port(&value)?);
            }
            "--project" => {
                parsed.project = Some(PathBuf::from(args.next().ok_or("--project needs a directory")?))
            }
            "--headless" => {
                parsed.headless = Some(PathBuf::from(args.next().ok_or("--headless needs a script")?))
            }
            "--software" => parsed.software = true,
            "--fresh" => parsed.fresh = true,
            "--size" => parsed.size = Some(parse_size(&args.next().ok_or("--size needs WIDTHxHEIGHT")?)?),
            other => {
                return Err(format!(
                    "unknown argument `{other}`; usage: penumbra [--project DIR] [--port N] [--fresh] \
                     [--headless SCRIPT [--software] [--size WxH]]"
                ));
            }
        }
    }
    parsed.port = match port {
        Some(p) => p,
        None => control::port_from_env()?,
    };
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse_args(args(&["--port", "1234"])).unwrap().port, 1234);
        assert!(parse_args(args(&["--port"])).is_err());
        assert!(parse_args(args(&["--port", "x"])).is_err());
        assert!(parse_args(args(&["--bogus"])).is_err());
        let a = parse_args(args(&["--project", "some/dir", "--port", "1"])).unwrap();
        assert_eq!(a.project, Some(PathBuf::from("some/dir")));
        assert!(parse_args(args(&["--project"])).is_err());
        let h = parse_args(args(&["--headless", "s.jsonl", "--software", "--size", "800x600"])).unwrap();
        assert_eq!(h.headless, Some(PathBuf::from("s.jsonl")));
        assert!(h.software);
        assert_eq!(h.size, Some([800.0, 600.0]));
        assert!(parse_args(args(&["--size", "huge"])).is_err());
        assert!(parse_args(args(&["--size", "10x10"])).is_err());
    }

    #[test]
    fn the_demo_project_opens() {
        let types = Arc::new(penumbra_ops::builtin_types().unwrap());
        let demo = penumbra_project::parse(projects::DEMO, &types).unwrap();
        let dir = std::env::temp_dir().join(format!("penumbra-app-demo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::open(&dir, types, &demo).unwrap();
        assert_eq!(session.doc().snapshot().len(), 7);
        assert_eq!(session.status(Instant::now()).file_error, None);
        drop(session);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
