//! Project automation. `cargo xtask` with no arguments runs `ci`.
//!
//! - `ci`: everything that must pass before a commit: `fmt`, `clippy`, `test`, `linux`, `layers`,
//!   `notices`.
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
//! - `notices`: the third-party notices a build must ship with, written to
//!   `target/THIRD-PARTY-NOTICES.html` by cargo-about (`about.toml`, `about.hbs`). Fails when a
//!   dependency's licence is not accepted in `about.toml` or cannot be determined.
//! - `dist`: a release build for Windows, zipped with everything it needs beside the exe, the
//!   documentation and the examples: `target/dist/penumbra-VERSION-windows-x64.zip`. Not part
//!   of `ci`.

use std::path::Path;
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
/// `script` and `pyhost` (Python) `audio` (input devices, analysis), `dmx` (serial port, UDP) and
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
    ("penumbra-dmx", UI_AND_GPU),
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
        "notices" => notices(),
        "dist" => dist(),
        other => Err(format!(
            "unknown task `{other}`; tasks: ci, fmt, clippy, test, linux, layers, bless, docs, notices, dist"
        )),
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
    notices()?;
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

fn root() -> Result<&'static Path, String> {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or_else(|| "xtask has no parent directory".into())
}

fn notices() -> Result<(), String> {
    let root = root()?;
    let template = root.join("about.hbs");
    let out = root.join("target").join("THIRD-PARTY-NOTICES.html");
    let (template, out) = (template.to_string_lossy(), out.to_string_lossy());
    let args = ["about", "generate", "--workspace", "--locked", "--fail", "-o", &out, &template];
    eprintln!("xtask: cargo {}", args.join(" "));
    let status = Command::new(cargo_bin())
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "generating the notices failed ({status})\n(if cargo-about is missing: `cargo install cargo-about --locked --features cli`)"
        ));
    }
    eprintln!("xtask: notices written to {out}");
    Ok(())
}

/// The Windows target, named explicitly so that the static CRT applies to the shipped binaries
/// only, not to build scripts and proc macros.
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// What ships beside the exe, from the target directory.
const DIST_BINARIES: &[&str] = &["penumbra.exe", "penumbra-cli.exe", "pyhost.dll"];

fn dist() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("dist builds for Windows only so far".into());
    }
    let root = root()?;
    notices()?;

    // A static C runtime, so the exe does not need the Visual C++ redistributable, which a fresh
    // Windows may lack. pyhost is built explicitly: the app's build makes only its rlib.
    let args = ["build", "--release", "--locked", "--target", WINDOWS_TARGET];
    let packages = ["-p", "penumbra", "-p", "penumbra-cli", "-p", "penumbra-pyhost"];
    eprintln!("xtask: cargo {} {}", args.join(" "), packages.join(" "));
    let status = Command::new(cargo_bin())
        .args(args)
        .args(packages)
        .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS", "-C target-feature=+crt-static")
        .current_dir(root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("the release build failed ({status})"));
    }

    let version = env!("CARGO_PKG_VERSION");
    let name = format!("penumbra-{version}-windows-x64");
    let dist = root.join("target").join("dist");
    let stage = dist.join(&name);
    if stage.exists() {
        std::fs::remove_dir_all(&stage).map_err(|e| format!("cannot clear {}: {e}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage).map_err(|e| format!("cannot create {}: {e}", stage.display()))?;

    let release = root.join("target").join(WINDOWS_TARGET).join("release");
    let mut files: Vec<(std::path::PathBuf, &str)> =
        DIST_BINARIES.iter().map(|f| (release.join(f), *f)).collect();
    files.push((root.join("target").join("THIRD-PARTY-NOTICES.html"), "THIRD-PARTY-NOTICES.html"));
    files.push((root.join("LICENSE-MIT"), "LICENSE-MIT"));
    files.push((root.join("LICENSE-APACHE"), "LICENSE-APACHE"));
    for (from, to) in &files {
        std::fs::copy(from, stage.join(to)).map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
    }
    // Windows line endings, whatever the checkout has: the README is for people opening it in
    // whatever editor a double-click picks.
    let readme = include_str!("../dist/README.txt")
        .replace("{version}", version)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    std::fs::write(stage.join("README.txt"), readme).map_err(|e| format!("cannot write README.txt: {e}"))?;
    // For a Claude Code session started in the unpacked folder, which reads CLAUDE.md on its own.
    std::fs::write(stage.join("CLAUDE.md"), include_str!("../dist/CLAUDE.md"))
        .map_err(|e| format!("cannot write CLAUDE.md: {e}"))?;

    // The documentation of exactly this version, readable offline. Getting started shows the
    // screenshot by a path relative to docs/, so it keeps that place.
    copy_dir(&root.join("docs"), &stage.join("docs"))?;
    copy_dir(&root.join("examples"), &stage.join("examples"))?;
    std::fs::create_dir_all(stage.join("assets")).map_err(|e| format!("cannot create assets: {e}"))?;
    let shot = root.join("assets").join("screenshot.png");
    std::fs::copy(&shot, stage.join("assets").join("screenshot.png"))
        .map_err(|e| format!("cannot copy {}: {e}", shot.display()))?;

    // Windows' own tar (bsdtar) writes zip archives, `-a` picks the format from the extension.
    // Named by full path: the GNU tar of Git for Windows may come first on PATH and cannot.
    let tar = Path::new(&std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
        .join("System32")
        .join("tar.exe");
    let zip = dist.join(format!("{name}.zip"));
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| format!("cannot remove {}: {e}", zip.display()))?;
    }
    let status = Command::new(&tar)
        .arg("-a")
        .arg("-cf")
        .arg(&zip)
        .arg(&name)
        .current_dir(&dist)
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !status.success() {
        return Err(format!("zipping failed ({status})"));
    }
    eprintln!("xtask: {} written", zip.display());
    Ok(())
}

/// Copies a directory tree of plain files.
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| format!("cannot copy {}: {e}", path.display()))?;
        }
    }
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
