//! The embedded Python of the script node.
//!
//! This crate is the PyO3 boundary and the only code that links Python. It is built as a
//! `cdylib` (`pyhost.dll`, `libpyhost.so`) that `penumbra-script` loads at runtime after it
//! has found a suitable Python and loaded that Python's library by full path. The app itself
//! never imports `python3.dll`, so a missing or too old Python is an error message, not a
//! loader dialog or a process that does not start.
//!
//! The interface is a handful of C functions, all of them guarded by `catch_unwind`, exchanging
//! bytes through a caller-supplied sink (so no memory crosses the two allocators):
//!
//! - [`pn_pyhost_abi`]: the interface version, checked before anything else is called.
//! - [`pn_pyhost_init`]: initialises the interpreter and the bootstrap module
//!   (`bootstrap.py`); call once, from the thread that will run every request.
//! - [`pn_pyhost_request`]: one JSON request (load a script, call it, ...) and a block of
//!   binary data (pictures) handed to the bootstrap's `handle`; the JSON reply and the binary
//!   parts returned with it (pictures, mesh arrays) go out through the sink as one frame.
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
use pyo3::types::{PyByteArray, PyBytes, PyString};

/// Version of the C interface. `penumbra-script` refuses a library with another one.
pub const ABI: u32 = 2;

/// Receives output from the library: `ctx` as passed in, then bytes. One reply may come in
/// several pieces; the caller appends them.
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
/// the thread that called [`pn_pyhost_init`]. `blob` is binary data the request refers to by
/// offset (input pictures), handed to Python as a `bytearray` (`None` when empty).
///
/// Returns 0 for a reply, which goes to `sink` as one frame: little-endian `u64`s for the
/// JSON's length, the number `n` of binary parts and the `n` lengths of the parts, then the
/// JSON text and the parts one after the other. Anything else returned means an error message
/// (plain UTF-8) went to `sink`.
///
/// # Safety
///
/// `request` must point to `len` readable bytes and `blob` to `blob_len` (either may be null
/// with length 0) for the duration of the call; `sink` as for [`pn_pyhost_init`].
#[unsafe(no_mangle)] // SAFETY: unique, prefixed symbol name.
pub unsafe extern "C" fn pn_pyhost_request(
    request: *const u8,
    len: usize,
    blob: *const u8,
    blob_len: usize,
    sink: Sink,
    ctx: *mut c_void,
) -> i32 {
    let bytes: &[u8] = if request.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `len` readable bytes at `request` for this call.
        unsafe { std::slice::from_raw_parts(request, len) }
    };
    let blob: &[u8] = if blob.is_null() || blob_len == 0 {
        &[]
    } else {
        // SAFETY: as for `request`: `blob_len` readable bytes at `blob` for this call.
        unsafe { std::slice::from_raw_parts(blob, blob_len) }
    };
    guarded_with(sink, ctx, |emit| request_frame(bytes, blob, emit))
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
    guarded_with(sink, ctx, |emit| {
        let text = f()?;
        emit(text.as_bytes());
        Ok(())
    })
}

/// Runs `f`, which sends its output to `sink` through the `emit` it is given; an error or a
/// panic sends its message instead. Returns 0 for `Ok`, 1 for `Err`, 2 for a panic. `f` emits
/// only once it cannot fail any more, so an error message never follows part of a reply.
fn guarded_with(sink: Sink, ctx: *mut c_void, f: impl FnOnce(&dyn Fn(&[u8])) -> Result<(), String>) -> i32 {
    let emit = |bytes: &[u8]| sink(ctx, bytes.as_ptr(), bytes.len());
    let (code, text) = match catch_unwind(AssertUnwindSafe(|| f(&emit))) {
        Ok(Ok(())) => return 0,
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
    emit(text.as_bytes());
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

/// One request. The bootstrap's `handle(text, blob)` returns the reply's JSON text, or a tuple
/// of it and a list of `bytes`; either goes out as the frame [`pn_pyhost_request`] describes.
fn request_frame(bytes: &[u8], blob: &[u8], emit: &dyn Fn(&[u8])) -> Result<(), String> {
    let boot = BOOT.get().ok_or("Python is not initialised")?;
    let text = std::str::from_utf8(bytes).map_err(|e| format!("request is not UTF-8: {e}"))?;
    Python::attach(|py| {
        let blob = if blob.is_empty() { py.None() } else { PyByteArray::new(py, blob).into_any().unbind() };
        // A watchdog interrupt that arrived after the previous request ended would hit this one:
        // clear it first, and again afterwards.
        clear_interrupt(boot.thread);
        let result = boot.handle.bind(py).call1((text, blob));
        clear_interrupt(boot.thread);
        let reply = result.map_err(|e| describe(py, &e))?;
        let (json, parts): (Bound<'_, PyString>, Vec<Bound<'_, PyBytes>>) = match reply.cast::<PyString>() {
            Ok(s) => (s.clone(), Vec::new()),
            Err(_) => reply.extract().map_err(|e| describe(py, &e))?,
        };
        let json = json.to_str().map_err(|e| describe(py, &e))?;
        let lengths = [json.len(), parts.len()].into_iter().chain(parts.iter().map(|p| p.as_bytes().len()));
        let header: Vec<u8> = lengths.flat_map(|n| (n as u64).to_le_bytes()).collect();
        emit(&header);
        emit(json.as_bytes());
        for p in &parts {
            emit(p.as_bytes());
        }
        Ok(())
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
