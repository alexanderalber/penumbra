//! Project automation. `cargo xtask` with no arguments runs `ci`.
//!
//! - `ci`: everything that must pass before a commit: `fmt`, `clippy`, `test`, `linux`, `layers`,
//!   `notices`. `layers` and `notices` read only the dependency set and their own configuration,
//!   so `ci` skips them while all of that is the same as when they last passed (run on their own,
//!   they always run).
//! - `fmt`: `cargo fmt --check`.
//! - `clippy`: clippy on all targets with `-D warnings`.
//! - `test`: the workspace tests.
//! - `linux`: clippy for `x86_64-unknown-linux-gnu` from Windows, so Windows-only code cannot
//!   creep in. Clippy includes `cargo check`.
//! - `layers`: lower crates must not depend on GPU or UI crates.
//! - `bless`: rewrite the golden images (`crates/ops/tests/golden/`) from the current renders on
//!   the software adapter. Look at the result before committing it.
//! - `docs`: rewrite the generated reference (`docs/reference/`) from the node types and
//!   commands, and `docs/penumbra.html`, the single page with all the docs that the `?` on a
//!   node opens. `test` fails while any of it is out of date.
//! - `notices`: the third-party notices a build must ship with, written to
//!   `target/THIRD-PARTY-NOTICES.html` by cargo-about (`about.toml`, `about.hbs`). Fails when a
//!   dependency's licence is not accepted in `about.toml` or cannot be determined.
//! - `dist`: a release build for Windows, zipped with everything it needs beside the exe, the
//!   documentation, the examples and the content library: `target/dist/penumbra-VERSION-windows-x64.zip`. Not part
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
/// `camera` (capture devices), `track` (blob tracking) and `record` (movie encoding) are headless
/// too.
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
    ("penumbra-record", UI_AND_GPU),
    ("penumbra-camera", UI_AND_GPU),
    ("penumbra-track", UI_AND_GPU),
    ("penumbra-script", UI_AND_GPU),
    ("penumbra-pyhost", UI_AND_GPU),
    ("penumbra-spout", UI),
    ("penumbra-ndi", UI),
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
    unless_unchanged("layers", layers_key(), layers)?;
    unless_unchanged("notices", notices_key(), notices)?;
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
    let status = cargo_command()
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
    let status = cargo_command()
        .args(args)
        .env("PENUMBRA_BLESS", "1")
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("writing the reference failed ({status})"));
    }
    eprintln!("xtask: reference rewritten; check `git diff docs/reference docs/penumbra.html`");
    Ok(())
}

fn root() -> Result<&'static Path, String> {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or_else(|| "xtask has no parent directory".into())
}

fn notices() -> Result<(), String> {
    let root = root()?;
    let template = root.join("about.hbs");
    let out = notices_file(root);
    let (template, out) = (template.to_string_lossy(), out.to_string_lossy());
    let args = ["about", "generate", "--workspace", "--locked", "--fail", "-o", &out, &template];
    eprintln!("xtask: cargo {}", args.join(" "));
    let status = cargo_command()
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

fn notices_file(root: &Path) -> std::path::PathBuf {
    root.join("target").join("THIRD-PARTY-NOTICES.html")
}

/// Runs `step` unless it passed before with exactly the same inputs. `key` hashes everything the
/// step reads (`None`: something could not be read, so the step runs); the key of the last pass
/// is kept in `target/xtask-NAME.inputs`.
fn unless_unchanged(name: &str, key: Option<String>, step: fn() -> Result<(), String>) -> Result<(), String> {
    let stamp = root()?.join("target").join(format!("xtask-{name}.inputs"));
    if let Some(key) = &key
        && std::fs::read_to_string(&stamp).is_ok_and(|s| s == *key)
    {
        eprintln!("xtask: {name}: inputs unchanged since it last passed");
        return Ok(());
    }
    step()?;
    if let Some(key) = key {
        if let Some(dir) = stamp.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        std::fs::write(&stamp, key).map_err(|e| format!("cannot write {}: {e}", stamp.display()))?;
    }
    Ok(())
}

/// What `layers` reads: cargo's version, `Cargo.lock`, every manifest in the workspace and the
/// layering rules themselves.
fn layers_key() -> Option<String> {
    inputs_key(&["--version"], &[], &format!("{LAYERS:?}"))
}

/// What `notices` reads: cargo-about's version, `Cargo.lock`, every manifest, `about.toml` and
/// `about.hbs`, plus the notices file itself, so a missing or overwritten file is made again. The
/// crate sources behind a locked version or git revision do not change. cargo-about takes about
/// ten seconds on all cores, so this is worth skipping.
fn notices_key() -> Option<String> {
    let root = root().ok()?;
    let files = [root.join("about.toml"), root.join("about.hbs"), notices_file(root)];
    inputs_key(&["about", "--version"], &files, "")
}

/// A hash of the output of `cargo VERSION_ARGS`, `Cargo.lock`, every manifest in the workspace,
/// `files` and `extra`. `None` when any of it cannot be read.
fn inputs_key(version_args: &[&str], files: &[std::path::PathBuf], extra: &str) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let root = root().ok()?;
    let version = cargo_command().args(version_args).output().ok().filter(|o| o.status.success())?;
    let mut all =
        vec![root.join("Cargo.lock"), root.join("Cargo.toml"), root.join("xtask").join("Cargo.toml")];
    for group in ["crates", "apps"] {
        let mut members: Vec<_> = std::fs::read_dir(root.join(group))
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path().join("Cargo.toml")))
            .filter(|p| p.is_file())
            .collect();
        members.sort();
        all.extend(members);
    }
    all.extend(files.iter().cloned());
    let mut hasher = std::hash::DefaultHasher::new();
    version.stdout.hash(&mut hasher);
    extra.hash(&mut hasher);
    for file in &all {
        file.hash(&mut hasher);
        std::fs::read(file).ok()?.hash(&mut hasher);
    }
    Some(format!("{:016x}", hasher.finish()))
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
    let status = cargo_command()
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
    // The content library: components with their shaders, offered in the add dialog.
    copy_dir(&root.join("library"), &stage.join("library"))?;
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
        let out = cargo_command()
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

/// A cargo command with the `PATH` of the shell that started `cargo xtask`.
///
/// `cargo xtask` runs through `cargo run`, which puts the target directory and the toolchain's
/// library directory in front of `PATH`. pyo3's build script reruns whenever `PATH` changes, so
/// with that `PATH` passed on, every `cargo` command typed in between and the next `cargo xtask`
/// would each rebuild pyo3 and all crates above it.
fn cargo_command() -> Command {
    let mut command = Command::new(cargo_bin());
    if let Some(path) = shell_path() {
        command.env("PATH", path);
    }
    command
}

/// `PATH` without the leading entries `cargo run` added: directories in the target directory
/// this program runs from and the toolchain's `rustlib`. `None` when there is nothing to remove.
fn shell_path() -> Option<std::ffi::OsString> {
    let path = std::env::var_os("PATH")?;
    let exe = std::env::current_exe().ok()?;
    let target = exe.parent()?.parent()?;
    let entries: Vec<std::path::PathBuf> = std::env::split_paths(&path).collect();
    let added = entries
        .iter()
        .take_while(|p| p.starts_with(target) || p.components().any(|c| c.as_os_str() == "rustlib"))
        .count();
    if added == 0 {
        return None;
    }
    std::env::join_paths(entries.get(added..)?).ok()
}

fn cargo(args: &[&str]) -> Result<(), String> {
    eprintln!("xtask: cargo {}", args.join(" "));
    let status = cargo_command().args(args).status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("`cargo {}` failed ({status})", args.join(" "))) }
}
