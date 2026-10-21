//! The ticker: a thread that sends the cook thread a tick at the show rate, as a real output
//! window sends its vsync, so the show clock runs as evenly without a projector as with one. The
//! cook thread's own wait for the next frame is a channel timeout, which runs on the system's
//! coarse timer and starts some frames late on Windows. The ticker sleeps (a high-resolution
//! wait) until just before each tick and yields the rest of the way.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long before a tick the sleep ends and yielding takes over.
const SPIN: Duration = Duration::from_micros(1500);
/// How often an idle ticker looks whether it should tick or stop.
const IDLE: Duration = Duration::from_millis(20);

#[derive(Default)]
pub struct Ticker {
    /// Nanoseconds between ticks; 0 while off.
    period_ns: AtomicU64,
    stop: AtomicBool,
}

impl Ticker {
    /// Ticks every `period`, or not at all.
    pub fn set(&self, period: Option<Duration>) {
        let ns = period.map_or(0, |p| u64::try_from(p.as_nanos()).unwrap_or(u64::MAX));
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
    ///
    /// The thread runs at a raised priority: it sleeps nearly all the time, but at normal
    /// priority a machine with every core busy (software rendering, an encoder, a compile) wakes
    /// it a scheduler quantum late, 15 to 40 ms, which drops ticks and stutters the show clock.
    pub fn run(&self, tick: impl Fn(Instant) -> bool) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        if !penumbra_system::process::raise_thread_priority() {
            log::debug!("the ticker runs at normal priority");
        }
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
        ticker.set(Some(Duration::from_millis(10)));
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
        // Never faster than the rate (the first tick may be late, which shortens the span a
        // little); slower only by what a machine busy with the rest of the test suite costs
        // single ticks at the ticker's raised priority.
        assert!((0.27..0.38).contains(&span), "29 periods of 10 ms took {span} s");
        // Off and stopped: the loop ends.
        ticker.set(None);
        assert!(!ticker.ticking());
        ticker.stop(true);
        ticker.run(|_| true);
    }
}
