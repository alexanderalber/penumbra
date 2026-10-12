//! A counting global allocator for `pn`: how many bytes the Rust heap holds right now, so the
//! soak test can tell a leak in our code (the Rust heap grows) from memory the GPU driver or the
//! operating system holds for the process (private bytes grow, the Rust heap does not).
//!
//! The only `unsafe` in `pn`: `GlobalAlloc` is an unsafe trait. Every method forwards to the
//! system allocator unchanged and only counts.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static LIVE: AtomicU64 = AtomicU64::new(0);

pub struct Counting;

/// Bytes currently allocated through the Rust heap.
pub fn live() -> u64 {
    LIVE.load(Ordering::Relaxed)
}

// SAFETY: every method calls `System` with the caller's arguments, which carry the caller's
// guarantees, and returns its result unchanged; the counter is only arithmetic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded as is.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(new_size as u64, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }
}
