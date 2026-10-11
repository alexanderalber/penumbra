//! Project automation. `cargo xtask` with no arguments runs `ci`.
//!
//! - `ci`: everything that must pass before a commit: `fmt`, `clippy`, `test`, `linux`, `layers`.
//! - `fmt`: `cargo fmt --check`.
//! - `clippy`: clippy on all targets with `-D warnings`.
//! - `test`: the workspace tests.
//! - `linux`: clippy for `x86_64-unknown-linux-gnu` from Windows, so Windows-only code cannot
//!   creep in. Clippy includes `cargo check`.
//! - `layers`: lower crates must not depend on GPU or UI crates.
//! - `bless`: rewrite the golden images (`crates/ops/tests/golden/`) from the current renders on
//!   the software adapter. Look at the result before committing it.
//! - `docs`: rewrite the generated reference (`docs/reference/`) from the node types and
//!   commands. `test` fails while it is out of date.

use std::process::{Command, ExitCode};
use std::time::Instant;

const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";

/// Native link libraries that cannot be found when checking for Linux from Windows. Declaring
/// them through `--config` overrides the sys crates' build scripts. Passed on the command line
/// rather than kept in `.cargo/config.toml`, so normal builds on a real Linux machine still run the
/// build scripts.
const LINUX_LINK_OVERRIDES: &[&str] = &[
    "target.x86_64-unknown-linux-gnu.alsa.rustc-link-lib=[\"asound\"]",
    "target.x86_64-unknown-linux-gnu.libudev.rustc-link-lib=[\"udev\"]",
];

/// Which crates may not reach which: (crate, forbidden dependencies). `core`, `graph` and
/// `project` stay headless; `gpu`, `ops` and `engine` use wgpu but no UI. `system` is OS code and
/// `video` decodes files, both without GPU or windows; `output` has windows (winit) but no egui.
/// `script` and `pyhost` (Python) `audio` (input devices, analysis) and
/// `camera` (capture devices) and `track` (blob tracking) are headless too.
const UI: &[&str] = &["egui", "eframe", "egui-wgpu", "egui-winit", "winit"];
const EGUI: &[&str] = &["egui", "eframe", "egui-wgpu", "egui-winit"];
const UI_AND_GPU: &[&str] = &["egui", "eframe", "egui-wgpu", "egui-winit", "winit", "wgpu"];
const LAYERS: &[(&str, &[&str])] = &[
    ("penumbra-core", UI_AND_GPU),
    ("penumbra-graph", UI_AND_GPU),
    ("penumbra-project", UI_AND_GPU),
    ("penumbra-gpu", UI),
    ("penumbra-ops", UI),
    ("penumbra-engine", UI),
    ("penumbra-system", UI_AND_GPU),
    ("penumbra-video", UI_AND_GPU),
    ("penumbra-calib", UI_AND_GPU),
    ("penumbra-mesh", UI_AND_GPU),
    ("penumbra-osc", UI_AND_GPU),
    ("penumbra-audio", UI_AND_GPU),
    ("penumbra-camera", UI_AND_GPU),
    ("penumbra-track", UI_AND_GPU),
    ("penumbra-script", UI_AND_GPU),
    ("penumbra-pyhost", UI_AND_GPU),
    ("penumbra-output", EGUI),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().map_or("ci", String::as_str);
    let result = match task {
        "ci" => ci(),
        "fmt" => fmt(),
        "clippy" => clippy(),
        "test" => test(),
        "linux" => linux(),
        "layers" => layers(),
        "bless" => bless(),
        "docs" => docs(),
        other => {
            Err(format!("unknown task `{other}`; tasks: ci, fmt, clippy, test, linux, layers, bless, docs"))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn ci() -> Result<(), String> {
    let start = Instant::now();
    fmt()?;
    clippy()?;
    test()?;
    linux()?;
    layers()?;
    eprintln!("xtask: ci passed in {:.0} s", start.elapsed().as_secs_f64());
    Ok(())
}

fn fmt() -> Result<(), String> {
    cargo(&["fmt", "--all", "--check"])
}

fn clippy() -> Result<(), String> {
    cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
}

fn test() -> Result<(), String> {
    cargo(&["test", "--workspace"])
}

fn bless() -> Result<(), String> {
    let args = ["test", "-p", "penumbra-ops", "golden"];
    eprintln!("xtask: PENUMBRA_BLESS=1 cargo {}", args.join(" "));
    let status = Command::new(cargo_bin())
        .args(args)
        .env("PENUMBRA_BLESS", "1")
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("blessing failed ({status})"));
    }
    eprintln!(
        "xtask: golden images rewritten; check `git diff --stat crates/ops/tests/golden` and look at them"
    );
    Ok(())
}

fn docs() -> Result<(), String> {
    let args = ["test", "-p", "penumbra-ui", "reference_docs_are_current"];
    eprintln!("xtask: PENUMBRA_BLESS=1 cargo {}", args.join(" "));
    let status = Command::new(cargo_bin())
        .args(args)
        .env("PENUMBRA_BLESS", "1")
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("writing the reference failed ({status})"));
    }
    eprintln!("xtask: reference rewritten; check `git diff docs/reference`");
    Ok(())
}

fn linux() -> Result<(), String> {
    let mut args = vec!["clippy", "--workspace", "--all-targets", "--target", LINUX_TARGET];
    for o in LINUX_LINK_OVERRIDES {
        args.extend(["--config", o]);
    }
    args.extend(["--", "-D", "warnings"]);
    cargo(&args).map_err(|e| format!("{e}\n(if the target is missing: `rustup target add {LINUX_TARGET}`)"))
}

fn layers() -> Result<(), String> {
    eprintln!("xtask: layers");
    let mut broken = Vec::new();
    for (krate, forbidden) in LAYERS {
        let out = Command::new(cargo_bin())
            .args(["tree", "--quiet", "-p", krate, "-e", "normal", "--target", "all"])
            .args(["--prefix", "none", "--format", "{p}"])
            .output()
            .map_err(|e| format!("cannot run cargo tree: {e}"))?;
        if !out.status.success() {
            return Err(format!("cargo tree -p {krate} failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        for name in text.lines().filter_map(|l| l.split_whitespace().next()) {
            if forbidden.contains(&name) && !broken.iter().any(|(k, n)| k == krate && n == name) {
                broken.push((*krate, name.to_string()));
            }
        }
    }
    if broken.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = broken.iter().map(|(k, n)| format!("{k} depends on {n}")).collect();
    Err(format!("layering violated:\n  {}", list.join("\n  ")))
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn cargo(args: &[&str]) -> Result<(), String> {
    eprintln!("xtask: cargo {}", args.join(" "));
    let status =
        Command::new(cargo_bin()).args(args).status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("`cargo {}` failed ({status})", args.join(" "))) }
}
