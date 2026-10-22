//! Keeping the displays on while projectors show something: Windows turns every display off
//! after its idle timeout, projectors included, unless a process says the display is required.
//!
//! [`KeepAwake`] holds that request while at least one output window is open and drops it when
//! the last one closes (and when it is dropped). On Windows the request is a power request
//! object (`PowerCreateRequest` with display and system required), not
//! `SetThreadExecutionState`: the latter belongs to the calling thread and silently ends with
//! it, while a power request belongs to its handle, works from any thread, and shows up with its
//! reason in `powercfg /requests`, which makes "why did the projectors stay on" answerable.

use penumbra_core::Error;

/// The operating-system side of [`KeepAwake`]; a fake one in tests.
pub trait PowerRequest {
    /// Holds (`true`) or releases (`false`) the request. Called only on a change.
    fn set(&mut self, on: bool) -> Result<(), Error>;
}

/// The request of this platform: a Windows power request, an error elsewhere (not implemented;
/// screen savers and DPMS stay in charge).
pub struct OsRequest {
    #[cfg(windows)]
    inner: Option<crate::windows::DisplayRequest>,
}

impl PowerRequest for OsRequest {
    #[cfg(windows)]
    fn set(&mut self, on: bool) -> Result<(), Error> {
        if self.inner.is_none() {
            if !on {
                return Ok(());
            }
            self.inner = Some(crate::windows::DisplayRequest::new("penumbra is showing on projectors")?);
        }
        match &self.inner {
            Some(r) => r.set(on),
            None => Ok(()),
        }
    }

    #[cfg(not(windows))]
    fn set(&mut self, on: bool) -> Result<(), Error> {
        if on { Err(Error::new("keeping the displays awake is only implemented on Windows")) } else { Ok(()) }
    }
}

/// Holds the display request while output windows are open.
pub struct KeepAwake<R: PowerRequest = OsRequest> {
    request: R,
    held: bool,
    /// The last failure, logged once rather than every tick.
    error: Option<String>,
}

impl KeepAwake<OsRequest> {
    pub fn new() -> Self {
        Self::with(OsRequest {
            #[cfg(windows)]
            inner: None,
        })
    }
}

impl Default for KeepAwake<OsRequest> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: PowerRequest> KeepAwake<R> {
    pub fn with(request: R) -> Self {
        Self { request, held: false, error: None }
    }

    /// Holds the request when `windows > 0`, releases it otherwise; returns whether it is held.
    /// A failed hold is retried on the next call.
    pub fn update(&mut self, windows: usize) -> bool {
        let want = windows > 0;
        if want == self.held {
            return self.held;
        }
        match self.request.set(want) {
            Ok(()) => {
                self.held = want;
                self.error = None;
                log::info!("displays {}", if want { "kept awake" } else { "may sleep again" });
            }
            Err(e) => {
                let e = e.to_string();
                if self.error.as_ref() != Some(&e) {
                    log::warn!("keeping the displays awake: {e}");
                    self.error = Some(e);
                }
                // A release that failed still means nobody wants it; the handle's end clears it.
                if !want {
                    self.held = false;
                }
            }
        }
        self.held
    }

    /// Whether the request is held.
    pub fn held(&self) -> bool {
        self.held
    }
}

impl<R: PowerRequest> Drop for KeepAwake<R> {
    fn drop(&mut self) {
        self.update(0);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// Records every call; fails holds while `fail` is set.
    #[derive(Clone, Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<bool>>>,
        fail: Rc<RefCell<bool>>,
    }

    impl PowerRequest for Fake {
        fn set(&mut self, on: bool) -> Result<(), Error> {
            self.calls.borrow_mut().push(on);
            if on && *self.fail.borrow() { Err(Error::new("refused")) } else { Ok(()) }
        }
    }

    #[test]
    fn held_while_windows_are_open() {
        let fake = Fake::default();
        let mut k = KeepAwake::with(fake.clone());
        assert!(!k.update(0));
        assert!(k.update(1));
        assert!(k.update(3));
        assert!(k.update(1));
        assert!(!k.update(0));
        assert!(!k.update(0));
        assert!(k.update(2));
        drop(k);
        // Only changes reach the OS, and dropping releases.
        assert_eq!(*fake.calls.borrow(), [true, false, true, false]);
    }

    #[test]
    fn a_refused_hold_is_retried() {
        let fake = Fake::default();
        *fake.fail.borrow_mut() = true;
        let mut k = KeepAwake::with(fake.clone());
        assert!(!k.update(1));
        assert!(!k.update(1));
        *fake.fail.borrow_mut() = false;
        assert!(k.update(1));
        assert!(k.held());
        assert!(!k.update(0));
        assert_eq!(*fake.calls.borrow(), [true, true, true, false]);
    }

    #[test]
    fn dropping_an_unheld_request_does_nothing() {
        let fake = Fake::default();
        drop(KeepAwake::with(fake.clone()));
        assert!(fake.calls.borrow().is_empty());
    }
}
