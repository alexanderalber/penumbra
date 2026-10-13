//! The ticker behind `out.pacer`: a thread that sends the cook thread a vsync at a projector's
//! refresh rate, as a real output window does, so the show clock is paced as it will be at the
//! show. Without it the cook thread waits for the next frame on a channel timeout, which runs on
//! the system's coarse timer and starts some frames late. The ticker sleeps (a high-resolution
//! wait) until just before each tick and yields the rest of the way.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long before a tick the sleep ends and yielding takes over.
const SPIN: Duration = Duration::from_micros(1500);
/// How often an idle ticker looks whether it should tick or stop.
const IDLE: Duration = Duration::from_millis(20);

#[derive(Default)]
pub struct Ticker {
    /// Nanoseconds between ticks; 0 while no `out.pacer` is active.
    period_ns: AtomicU64,
    stop: AtomicBool,
}

impl Ticker {
    /// Ticks at `hz`, or not at all.
    pub fn set(&self, hz: Option<f64>) {
        let ns = hz.filter(|h| h.is_finite() && *h > 0.0).map_or(0, |h| (1e9 / h).round() as u64);
        self.period_ns.store(ns, Ordering::Relaxed);
    }

    pub fn ticking(&self) -> bool {
        self.period_ns.load(Ordering::Relaxed) > 0
    }

    pub fn stop(&self, stop: bool) {
        self.stop.store(stop, Ordering::Relaxed);
    }

    /// The thread's loop: calls `tick` at every tick until stopped or `tick` says the cook
    /// thread is gone. A tick more than a period late starts the count afresh, so a stall is
    /// not made up by a burst.
    pub fn run(&self, tick: impl Fn(Instant) -> bool) {
        let mut next: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            let ns = self.period_ns.load(Ordering::Relaxed);
            if ns == 0 {
                next = None;
                std::thread::sleep(IDLE);
                continue;
            }
            let period = Duration::from_nanos(ns);
            let now = Instant::now();
            let mut at = next.unwrap_or(now + period);
            if now > at + period {
                at = now;
            }
            loop {
                let left = at.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                if left > SPIN {
                    std::thread::sleep(left - SPIN);
                } else {
                    std::thread::yield_now();
                }
            }
            if !tick(Instant::now()) {
                return;
            }
            next = Some(at + period);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn ticks_at_the_rate_until_stopped() {
        let ticker = Arc::new(Ticker::default());
        ticker.set(Some(100.0));
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let (t, seen) = (ticker.clone(), ticks.clone());
        let thread = std::thread::spawn(move || {
            t.run(|at| {
                let mut s = seen.lock().unwrap();
                s.push(at);
                s.len() < 30
            })
        });
        thread.join().unwrap();
        let ticks = ticks.lock().unwrap();
        assert_eq!(ticks.len(), 30);
        let span = ticks.last().unwrap().duration_since(*ticks.first().unwrap()).as_secs_f64();
        assert!((span - 0.29).abs() < 0.03, "29 periods of 10 ms took {span} s");
        // Off and stopped: the loop ends.
        ticker.set(None);
        assert!(!ticker.ticking());
        ticker.stop(true);
        ticker.run(|_| true);
    }
}
