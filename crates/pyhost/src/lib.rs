//! The embedded Python of the script node.
//!
//! This crate is the PyO3 boundary and the only code that links Python. It is built as a
//! `cdylib` (`pyhost.dll`, `libpyhost.so`) that `penumbra-script` loads at runtime after it
//! has found a suitable Python and loaded that Python's library by full path. The app itself
//! never imports `python3.dll`, so a missing or too old Python is an error message, not a
//! loader dialog or a process that does not start.
//!
//! The interface is a handful of C functions, all of them guarded by `catch_unwind`, exchanging
//! UTF-8 text through a caller-supplied sink (so no memory crosses the two allocators):
//!
//! - [`pn_pyhost_abi`]: the interface version, checked before anything else is called.
//! - [`pn_pyhost_init`]: initialises the interpreter and the bootstrap module
//!   (`bootstrap.py`); call once, from the thread that will run every request.
//! - [`pn_pyhost_request`]: one JSON request (load a script, call it, ...) handed to the
//!   bootstrap's `handle`, the JSON reply out through the sink.
//! - [`pn_pyhost_interrupt`]: raises `Timeout` in the request thread; the watchdog's call,
//!   from any other thread.
//! - [`pn_pyhost_bench_attach`]: times attaching to and detaching from the interpreter.
//!
//! The JSON in and out is interpreted by `bootstrap.py`, so this file stays a thin shim.
//!
//! `unsafe` is allowed in this crate only: exported unmangled C functions are unsafe attributes,
//! the request arrives as a raw pointer and length, and `PyThreadState_SetAsyncExc` has no safe
//! wrapper. Each use is commented.

#![allow(unsafe_code)]

use std::ffi::{CString, c_long, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::time::Instant;

use pyo3::ffi;
use pyo3::prelude::*;

/// Version of the C interface. `penumbra-script` refuses a library with another one.
pub const ABI: u32 = 1;

/// Receives text from the library: `ctx` as passed in, then the UTF-8 bytes.
pub type Sink = extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize);

const BOOTSTRAP: &str = include_str!("bootstrap.py");

/// What `init` set up: the bootstrap's request handler, the exception the watchdog raises, and
/// the id of the thread requests run on.
struct Boot {
    handle: Py<PyAny>,
    timeout: Py<PyAny>,
    thread: u64,
    info: String,
}

static BOOT: OnceLock<Boot> = OnceLock::new();

/// The interface version ([`ABI`]).
#[unsafe(no_mangle)] // SAFETY: a unique, prefixed symbol name; nothing else exports it.
pub extern "C" fn pn_pyhost_abi() -> u32 {
    ABI
}

/// Initialises Python and the bootstrap module. Sends a JSON object with `version`, `prefix`
/// and `thread` (0 returned) or an error message (non-zero returned) to `sink`. Calling it again
/// sends the same information.
///
/// # Safety
///
/// `sink` must be safe to call with `ctx` and a pointer and length of readable bytes.
#[unsafe(no_mangle)] // SAFETY: unique, prefixed symbol name.
pub unsafe extern "C" fn pn_pyhost_init(sink: Sink, ctx: *mut c_void) -> i32 {
    guarded(sink, ctx, init)
}

/// Runs one JSON request through the bootstrap's `handle` on the calling thread, which must be
/// the thread that called [`pn_pyhost_init`]. The reply (JSON) or an error message goes to
/// `sink`; 0 means a reply.
///
/// # Safety
///
/// `request` must point to `len` readable bytes (or be null with `len` 0) for the duration of
/// the call; `sink` as for [`pn_pyhost_init`].
#[unsafe(no_mangle)] // SAFETY: unique, prefixed symbol name.
pub unsafe extern "C" fn pn_pyhost_request(
    request: *const u8,
    len: usize,
    sink: Sink,
    ctx: *mut c_void,
) -> i32 {
    let bytes: &[u8] = if request.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `len` readable bytes at `request` for this call.
        unsafe { std::slice::from_raw_parts(request, len) }
    };
    guarded(sink, ctx, || request_text(bytes))
}

/// Raises the bootstrap's `Timeout` in thread `thread` the next time it runs Python code.
/// Blocks until this thread can attach to the interpreter (a script running pure Python lets
/// it in every few milliseconds; one stuck in C code does not). Returns 0 when raised, -1
/// before `init`, -2 when something failed.
#[unsafe(no_mangle)] // SAFETY: unique, prefixed symbol name.
pub extern "C" fn pn_pyhost_interrupt(thread: u64) -> i32 {
    let Some(boot) = BOOT.get() else { return -1 };
    catch_unwind(AssertUnwindSafe(|| {
        Python::attach(|_py| {
            // SAFETY: attached to the interpreter; `timeout` is a live exception type, borrowed
            // for the call (SetAsyncExc takes its own reference). The id is a thread ident as
            // Python's `threading.get_ident` reports it; the cast restores the C type.
            unsafe { ffi::PyThreadState_SetAsyncExc(thread as c_long, boot.timeout.as_ptr()) };
            0
        })
    }))
    .unwrap_or(-2)
}

/// Attaches to and detaches from the interpreter `n` times on the calling thread; returns the
/// nanoseconds that took (0 before `init`). For measuring the per-frame cost of the GIL.
#[unsafe(no_mangle)] // SAFETY: unique, prefixed symbol name.
pub extern "C" fn pn_pyhost_bench_attach(n: u32) -> u64 {
    if BOOT.get().is_none() {
        return 0;
    }
    catch_unwind(|| {
        let start = Instant::now();
        for _ in 0..n {
            Python::attach(|py| {
                std::hint::black_box(py);
            });
        }
        u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    })
    .unwrap_or(0)
}

/// Runs `f`, sends its text to `sink` and returns 0 for `Ok`, 1 for `Err`, 2 for a panic.
fn guarded(sink: Sink, ctx: *mut c_void, f: impl FnOnce() -> Result<String, String>) -> i32 {
    let (code, text) = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(text)) => (0, text),
        Ok(Err(e)) => (1, e),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            (2, format!("pyhost panicked: {msg}"))
        }
    };
    sink(ctx, text.as_ptr(), text.len());
    code
}

fn init() -> Result<String, String> {
    if let Some(boot) = BOOT.get() {
        return Ok(boot.info.clone());
    }
    Python::initialize();
    let boot = Python::attach(|py| -> Result<Boot, String> {
        let describe = |e: PyErr| describe(py, &e);
        let code = CString::new(BOOTSTRAP).map_err(|e| e.to_string())?;
        let module = PyModule::from_code(py, &code, c"<penumbra bootstrap>", c"penumbra_bootstrap")
            .map_err(describe)?;
        let handle = module.getattr("handle").map_err(describe)?.unbind();
        let timeout = module.getattr("Timeout").map_err(describe)?.unbind();
        let info: String = module
            .getattr("info")
            .map_err(describe)?
            .call0()
            .map_err(describe)?
            .extract()
            .map_err(describe)?;
        let thread: u64 = module
            .getattr("thread_id")
            .map_err(describe)?
            .call0()
            .map_err(describe)?
            .extract()
            .map_err(describe)?;
        Ok(Boot { handle, timeout, thread, info })
    })?;
    let info = boot.info.clone();
    // Another thread cannot have set it: init runs on the one request thread.
    BOOT.set(boot).ok();
    Ok(info)
}

fn request_text(bytes: &[u8]) -> Result<String, String> {
    let boot = BOOT.get().ok_or("Python is not initialised")?;
    let text = std::str::from_utf8(bytes).map_err(|e| format!("request is not UTF-8: {e}"))?;
    Python::attach(|py| {
        // A watchdog interrupt that arrived after the previous request ended would hit this one:
        // clear it first, and again afterwards.
        clear_interrupt(boot.thread);
        let result = boot.handle.bind(py).call1((text,));
        clear_interrupt(boot.thread);
        match result {
            Ok(reply) => reply.extract::<String>().map_err(|e| describe(py, &e)),
            Err(e) => Err(describe(py, &e)),
        }
    })
}

/// Drops a pending `Timeout` for `thread`. Call while attached.
fn clear_interrupt(thread: u64) {
    // SAFETY: called inside `Python::attach`; a null exception clears the pending one.
    unsafe { ffi::PyThreadState_SetAsyncExc(thread as c_long, std::ptr::null_mut()) };
}

fn describe(py: Python<'_>, e: &PyErr) -> String {
    let mut s = e.to_string();
    if let Some(tb) = e.traceback(py)
        && let Ok(t) = tb.format()
    {
        s.push('\n');
        s.push_str(t.trim_end());
    }
    s
}
