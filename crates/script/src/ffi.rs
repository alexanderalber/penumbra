//! The only `unsafe` code of this crate: loading Python's library and pyhost by path, looking up
//! their functions and calling them.
//!
//! Order matters. Python's library is loaded first, by full path (on Windows `python3.dll`,
//! which forwards to `python3XY.dll` in the same directory; elsewhere `libpython3.XY.so` with
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
type RequestFn = unsafe extern "C" fn(*const u8, usize, Sink, *mut c_void) -> i32;
type InterruptFn = extern "C" fn(u64) -> i32;
type BenchFn = extern "C" fn(u32) -> u64;
type VersionFn = unsafe extern "C" fn() -> *const c_char;

/// The interface version this crate speaks (pyhost's `ABI`).
const ABI: u32 = 1;

/// pyhost's file name.
#[cfg(windows)]
const HOST_FILE: &str = "pyhost.dll";
#[cfg(not(windows))]
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
    /// executable (or its `deps` directory, where cargo puts it for tests and debug builds).
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
                return Err(format!("{shown} speaks interface {found}, expected {ABI}"));
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

    /// One JSON request on the calling thread (the one that called `init`).
    pub fn request(&self, request: &str) -> Result<String, String> {
        let mut out = Vec::new();
        // SAFETY: pointer and length of a live `str`; `out` as in `init`.
        let code = unsafe { (self.request)(request.as_ptr(), request.len(), sink, (&raw mut out).cast()) };
        text(code, out)
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

/// pyhost next to the executable, else in its `deps` directory.
fn host_file() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the executable: {e}"))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let candidates = [dir.join(HOST_FILE), dir.join("deps").join(HOST_FILE)];
    candidates.iter().find(|p| p.is_file()).cloned().ok_or_else(|| {
        format!("{HOST_FILE} is missing (looked in {}); it is built with the app", dir.display())
    })
}
