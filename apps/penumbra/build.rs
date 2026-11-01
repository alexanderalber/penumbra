//! Embeds the app icon and version info into `penumbra.exe` on Windows, so the file, a shortcut
//! and a pinned taskbar entry show the icon before the app runs; a missing resource compiler is
//! a warning, the build still links. On every target it tells the app which commit it is built
//! from (`PENUMBRA_COMMIT`, `PENUMBRA_COMMIT_DATE`), when git is there to ask; without git the
//! About window says the commit is unknown.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/penumbra.ico");
    commit();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/penumbra.ico")
        .set("ProductName", "penumbra")
        .set("FileDescription", "penumbra")
        .set("LegalCopyright", "MIT OR Apache-2.0")
        .set("OriginalFilename", "penumbra.exe")
        .set("InternalName", "penumbra");
    if let Err(e) = res.compile() {
        println!("cargo:warning=penumbra.exe built without icon and version info: {e}");
    }
}

/// The output of `git ARGS` run here, trimmed; `None` when git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Passes the commit and its date to the app, and asks cargo to run this again when the commit
/// changes. Only this crate is rebuilt then, the libraries stay as they are.
fn commit() {
    let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else { return };
    // HEAD names the branch; the branch's ref file (or the packed refs) moves with each commit.
    let head = dir.join("HEAD");
    watch(&head);
    if let Some(name) = std::fs::read_to_string(&head)
        .ok()
        .and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string()))
    {
        watch(&dir.join(name));
    }
    watch(&dir.join("packed-refs"));
    if let Some(hash) = git(&["rev-parse", "--short=10", "HEAD"]) {
        println!("cargo:rustc-env=PENUMBRA_COMMIT={hash}");
    }
    if let Some(date) = git(&["log", "-1", "--format=%cs", "HEAD"]) {
        println!("cargo:rustc-env=PENUMBRA_COMMIT_DATE={date}");
    }
}

/// A file cargo watches. Only files that exist: a missing one would make cargo run this script
/// on every build.
fn watch(file: &Path) {
    if file.is_file() {
        println!("cargo:rerun-if-changed={}", file.display());
    }
}
