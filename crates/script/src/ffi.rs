//! The only `unsafe` code of this crate: loading Python's library and pyhost by path, looking up
//! their functions and calling them.
//!
//! Order matters. Python's library is loaded first, by full path (on Windows `python3.dll`,
//! which forwards to `python3XY.dll` in the same directory; elsewhere `libpython3.XY.so` / `.dylib` with
//! `RTLD_GLOBAL`, so pyhost's unresolved symbols bind to it). Its `Py_GetVersion` confirms the
//! version before pyhost is loaded, because pyhost's imports only resolve against 3.12 or
//! newer: loading it against an older Python would fail in the loader, on Windows possibly
//! with a dialog. libloading suppresses Windows' error dialogs while loading.
//!
//! Neither library is ever unloaded: an initialised interpreter cannot be taken out of a
//! process. [`Library`] lives in a `static` once loaded.

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};

use crate::locate::{self, Install};

type Sink = extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize);
type AbiFn = extern "C" fn() -> u32;
type InitFn = unsafe extern "C" fn(Sink, *mut c_void) -> i32;
type RequestFn = unsafe extern "C" fn(*const u8, usize, *const u8, usize, Sink, *mut c_void) -> i32;
type InterruptFn = extern "C" fn(u64) -> i32;
type BenchFn = extern "C" fn(u32) -> u64;
type VersionFn = unsafe extern "C" fn() -> *const c_char;

/// The interface version this crate speaks (pyhost's `ABI`).
const ABI: u32 = 2;

/// pyhost's file name.
#[cfg(windows)]
const HOST_FILE: &str = "pyhost.dll";
#[cfg(target_os = "macos")]
const HOST_FILE: &str = "libpyhost.dylib";
#[cfg(not(any(windows, target_os = "macos")))]
const HOST_FILE: &str = "libpyhost.so";

/// Python's library and pyhost, loaded, with pyhost's functions.
pub struct Library {
    _python: libloading::Library,
    _host: libloading::Library,
    init: InitFn,
    request: RequestFn,
    interrupt: InterruptFn,
    bench: BenchFn,
    /// `Py_GetVersion()`, e.g. `3.12.10 (tags/...) [MSC v.1943 64 bit (AMD64)]`.
    pub version: String,
    /// The pyhost file loaded.
    pub host_path: PathBuf,
}

impl Library {
    /// Loads `install`'s library, checks its version, then loads pyhost from next to the
    /// executable (or from where cargo puts it for tests and debug builds).
    pub fn load(install: &Install) -> Result<Library, String> {
        let python = open(&install.library, true)?;
        // SAFETY: Py_GetVersion has this signature in every CPython 3 and may be called before
        // the interpreter is initialised; the symbol lives as long as `python`.
        let version = unsafe {
            let f = python
                .get::<VersionFn>(b"Py_GetVersion\0")
                .map_err(|e| format!("{}: no Py_GetVersion ({e})", install.library.display()))?;
            let p = f();
            if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        };
        match locate::parse_version(&version) {
            Some((3, minor)) if minor >= locate::MIN_MINOR => {}
            _ => {
                return Err(format!(
                    "{} is Python {}, the script node needs 3.{} or newer",
                    install.library.display(),
                    version.split_whitespace().next().unwrap_or("?"),
                    locate::MIN_MINOR
                ));
            }
        }
        let host_path = host_file()?;
        let host = open(&host_path, false)?;
        // SAFETY: the symbols are pyhost's exported functions with exactly these signatures
        // (crates/pyhost/src/lib.rs, checked through the ABI number first); the pointers stay
        // valid because `host` is stored next to them and never unloaded.
        unsafe {
            let shown = host_path.display().to_string();
            let sym = |name: &str| {
                let (path, name) = (shown.clone(), name.to_string());
                move |e: libloading::Error| format!("{path}: no {name} ({e})")
            };
            let abi = *host.get::<AbiFn>(b"pn_pyhost_abi\0").map_err(sym("pn_pyhost_abi"))?;
            let found = abi();
            if found != ABI {
                return Err(format!(
                    "{shown} speaks interface {found}, expected {ABI}: it is from another version of penumbra; rebuild or reinstall penumbra"
                ));
            }
            let init = *host.get::<InitFn>(b"pn_pyhost_init\0").map_err(sym("pn_pyhost_init"))?;
            let request = *host.get::<RequestFn>(b"pn_pyhost_request\0").map_err(sym("pn_pyhost_request"))?;
            let interrupt =
                *host.get::<InterruptFn>(b"pn_pyhost_interrupt\0").map_err(sym("pn_pyhost_interrupt"))?;
            let bench =
                *host.get::<BenchFn>(b"pn_pyhost_bench_attach\0").map_err(sym("pn_pyhost_bench_attach"))?;
            Ok(Library { _python: python, _host: host, init, request, interrupt, bench, version, host_path })
        }
    }

    /// Initialises the interpreter on the calling thread; the bootstrap's info JSON.
    pub fn init(&self) -> Result<String, String> {
        let mut out = Vec::new();
        // SAFETY: `sink` matches the Sink signature and `out` outlives the call.
        let code = unsafe { (self.init)(sink, (&raw mut out).cast()) };
        text(code, out)
    }

    /// One JSON request and its binary data on the calling thread (the one that called `init`):
    /// the reply's frame as pyhost sends it ([`crate::protocol::Frame`]), or an error message.
    pub fn request(&self, request: &str, blob: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        // SAFETY: pointers and lengths of a live `str` and slice; `out` as in `init`.
        let code = unsafe {
            (self.request)(
                request.as_ptr(),
                request.len(),
                blob.as_ptr(),
                blob.len(),
                sink,
                (&raw mut out).cast(),
            )
        };
        if code == 0 { Ok(out) } else { Err(String::from_utf8_lossy(&out).into_owned()) }
    }

    /// Raises `Timeout` in the interpreter thread `thread`. May block (see pyhost).
    pub fn interrupt(&self, thread: u64) -> bool {
        (self.interrupt)(thread) == 0
    }

    /// Nanoseconds for `n` attach/detach pairs on the calling thread.
    pub fn bench_attach(&self, n: u32) -> u64 {
        (self.bench)(n)
    }
}

fn text(code: i32, out: Vec<u8>) -> Result<String, String> {
    let s = String::from_utf8_lossy(&out).into_owned();
    if code == 0 { Ok(s) } else { Err(s) }
}

/// Collects pyhost's output into the `Vec<u8>` behind `ctx`.
extern "C" fn sink(ctx: *mut c_void, data: *const u8, len: usize) {
    if ctx.is_null() || data.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `&mut Vec<u8>` passed by `init` / `request` for this call only, and
    // pyhost hands over `len` readable bytes at `data` for the duration of this callback.
    let (out, bytes) = unsafe { (&mut *ctx.cast::<Vec<u8>>(), std::slice::from_raw_parts(data, len)) };
    out.extend_from_slice(bytes);
}

/// Opens a library by full path; `global` makes its symbols available to libraries loaded later
/// (needed for libpython outside Windows).
fn open(path: &Path, global: bool) -> Result<libloading::Library, String> {
    let fail = |e: libloading::Error| format!("cannot load {}: {e}", path.display());
    #[cfg(windows)]
    {
        let _ = global;
        use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
        // SAFETY: loading runs the library's initialisers: Python's and our own pyhost, both
        // trusted; the altered search path finds python3XY.dll next to python3.dll.
        unsafe { Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH) }.map(Into::into).map_err(fail)
    }
    #[cfg(not(windows))]
    {
        use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_LOCAL, RTLD_NOW};
        let flags = RTLD_NOW | if global { RTLD_GLOBAL } else { RTLD_LOCAL };
        // SAFETY: as above.
        unsafe { Library::open(Some(path), flags) }.map(Into::into).map_err(fail)
    }
}

/// The directory cargo builds pyhost in, below a profile directory's `build`: its package name.
const HOST_PACKAGE: &str = "penumbra-pyhost";
/// Most builds of pyhost looked at in a checkout.
const MAX_HOST_BUILDS: usize = 256;

/// pyhost next to the executable, as a package has it, else where cargo left it in a checkout.
fn host_file() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the executable: {e}"))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let beside = dir.join(HOST_FILE);
    if beside.is_file() {
        return Ok(beside);
    }
    built_host(dir).ok_or_else(|| {
        format!("{HOST_FILE} is missing (looked in {}); it is built with the app", dir.display())
    })
}

/// pyhost as cargo built it for a program in `dir`. Several files can be left from earlier
/// builds; the newest one is taken.
///
/// Cargo up to 1.99 writes it to `deps` in the profile directory (`target/debug`), which holds
/// the app. From 1.100 on cargo writes it to `build/penumbra-pyhost/<hash>/out` there, and a
/// test program lies in `build/<package>/<hash>/out`, four directories below the profile
/// directory.
fn built_host(dir: &Path) -> Option<PathBuf> {
    let above_test = match dir.ancestors().nth(3) {
        Some(build) if build.file_name().is_some_and(|n| n == "build") => build.parent(),
        _ => None,
    };
    let mut found = vec![dir.join("deps").join(HOST_FILE)];
    for profile in [Some(dir), above_test].into_iter().flatten() {
        let Ok(builds) = std::fs::read_dir(profile.join("build").join(HOST_PACKAGE)) else { continue };
        found.extend(builds.flatten().take(MAX_HOST_BUILDS).map(|b| b.path().join("out").join(HOST_FILE)));
    }
    found
        .into_iter()
        .filter_map(|file| {
            let meta = std::fs::metadata(&file).ok().filter(|m| m.is_file())?;
            Some((meta.modified().ok()?, file))
        })
        .max_by_key(|(written, _)| *written)
        .map(|(_, file)| file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// Writes an empty pyhost into `dir`, written `age` seconds ago.
    fn host_in(dir: &Path, age: u64) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(HOST_FILE);
        let written = SystemTime::now() - Duration::from_secs(age);
        std::fs::File::create(&file).unwrap().set_modified(written).unwrap();
        file
    }

    #[test]
    fn a_checkout_gives_the_newest_pyhost_cargo_built() {
        let root = std::env::temp_dir().join(format!("penumbra-pyhost-places-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("debug");
        let test_dir = profile.join("build").join("penumbra-ops").join("0123").join("out");
        std::fs::create_dir_all(&test_dir).unwrap();
        assert_eq!(built_host(&profile), None);
        assert_eq!(built_host(&test_dir), None);

        // Cargo up to 1.99: in `deps`, found from the app's directory.
        let old = host_in(&profile.join("deps"), 300);
        assert_eq!(built_host(&profile), Some(old));
        // Cargo from 1.100 on: a newer build wins over the file an older cargo left, both
        // from the app's directory and from a test program's.
        let builds = profile.join("build").join(HOST_PACKAGE);
        host_in(&builds.join("aaaa").join("out"), 200);
        let new = host_in(&builds.join("bbbb").join("out"), 100);
        assert_eq!(built_host(&profile), Some(new.clone()));
        assert_eq!(built_host(&test_dir), Some(new));
        // A directory named like the file is no pyhost.
        std::fs::create_dir_all(builds.join("cccc").join("out").join(HOST_FILE)).unwrap();
        assert!(built_host(&test_dir).is_some_and(|f| f.is_file()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
