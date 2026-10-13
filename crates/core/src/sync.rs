//! Locking that survives a panic in another thread.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, also when a thread panicked while holding it. A panic is caught and reported where
/// it happens; the data is used as that thread left it, rather than every later lock failing too.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
