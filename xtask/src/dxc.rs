//! DXC, Microsoft's current shader compiler for DirectX 12. wgpu's DX12 backend loads
//! `dxcompiler.dll` when it lies beside the executable and falls back to the old FXC when it
//! does not. On an RTX 3090, FXC took up to 10 s for one compute shader and about 23 s for all
//! the built-in ones on one thread; DXC took under 3 s for all of them. The DLL comes from a pinned release
//! of Microsoft's, checked by its SHA-256, kept in `target/dxc/VERSION/`.

use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = "v1.9.2609";
const URL: &str =
    "https://github.com/microsoft/DirectXShaderCompiler/releases/download/v1.9.2609/dxc_2026_09_29.zip";
const SHA256: &str = "ad31b1fc8443175d204f77a611fdb3ef2ec42759bdc2f1167368de24a4a7e7f1";

/// What a package takes from the release: (path in the archive, name beside the executable).
/// `dxil.dll` stays out: wgpu needs only `dxcompiler.dll` from DXC 1.8.2502 on.
pub const FILES: &[(&str, &str)] = &[
    ("bin/x64/dxcompiler.dll", "dxcompiler.dll"),
    ("LICENSE-LLVM.txt", "dxcompiler-LICENSE-LLVM.txt"),
    ("LICENCE-MIT.txt", "dxcompiler-LICENSE-MIT.txt"),
    ("LICENSE-MS.txt", "dxcompiler-LICENSE-MS.txt"),
];

/// The folder holding the release's [`FILES`] under their archive paths, downloaded and
/// checked when they are not there yet.
pub fn fetch(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join("target").join("dxc").join(VERSION);
    if FILES.iter().all(|(inside, _)| dir.join(inside).is_file()) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let zip = dir.join("dxc.zip");
    let system = system32();
    eprintln!("xtask: downloading DXC {VERSION}");
    run(Command::new(system.join("curl.exe")).args(["-fsSL", "-o"]).arg(&zip).arg(URL), "curl")?;
    let digest = sha256(&system, &zip)?;
    if !digest.eq_ignore_ascii_case(SHA256) {
        let _ = std::fs::remove_file(&zip);
        return Err(format!("DXC {VERSION}: SHA-256 {digest}, expected {SHA256}"));
    }
    // Windows' own tar (bsdtar) reads zip archives; the GNU tar of Git for Windows cannot.
    let mut tar = Command::new(system.join("tar.exe"));
    tar.arg("-xf").arg(&zip).arg("-C").arg(&dir).args(FILES.iter().map(|(inside, _)| *inside));
    run(&mut tar, "tar")?;
    let _ = std::fs::remove_file(&zip);
    Ok(dir)
}

/// Puts `dxcompiler.dll` beside the executables of the debug and the release build, so that
/// `cargo run` compiles shaders with DXC too. A copy that a running app holds is moved aside
/// first.
pub fn install(root: &Path) -> Result<(), String> {
    let dir = fetch(root)?;
    let Some((inside, name)) = FILES.first() else { return Ok(()) };
    let from = dir.join(inside);
    let bytes = std::fs::read(&from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for profile in ["debug", "release"] {
        let to_dir = root.join("target").join(profile);
        std::fs::create_dir_all(&to_dir).map_err(|e| format!("cannot create {}: {e}", to_dir.display()))?;
        let to = to_dir.join(name);
        if std::fs::read(&to).is_ok_and(|have| have == bytes) {
            eprintln!("xtask: {} is up to date", to.display());
            continue;
        }
        crate::move_aside(&to)?;
        std::fs::copy(&from, &to).map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
        eprintln!("xtask: {} written", to.display());
    }
    Ok(())
}

fn system32() -> PathBuf {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&system).join("System32")
}

/// The SHA-256 of `file` in hex, by Windows' `certutil`.
fn sha256(system: &Path, file: &Path) -> Result<String, String> {
    let out = Command::new(system.join("certutil.exe"))
        .arg("-hashfile")
        .arg(file)
        .arg("SHA256")
        .output()
        .map_err(|e| format!("cannot run certutil: {e}"))?;
    if !out.status.success() {
        return Err(format!("certutil failed ({})", out.status));
    }
    // The second line is the digest; older Windows put spaces between the bytes.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect())
        .ok_or_else(|| "certutil printed no digest".to_string())
}

fn run(command: &mut Command, name: &str) -> Result<(), String> {
    let status = command.status().map_err(|e| format!("cannot run {name}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{name} failed ({status})")) }
}
