//! Waits on one device from many threads at once, as the cook thread, the job threads and the
//! window do in the app.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::{AdapterChoice, Buffer, Gpu, ReadState};

/// What [`race`] counted.
#[derive(Debug)]
struct Race {
    /// Panics out of a wait or a poll, plus those the polls caught themselves.
    panics: usize,
    waits: usize,
    /// Readbacks still waiting long after their copy was submitted.
    stuck: usize,
}

/// Runs for `time`: three threads submit a copy and wait for its readback with no time or a
/// millisecond to spare, two threads only poll, one submits and polls. wgpu-core asserts
/// in a wait whose time runs out while another thread takes the finished work in, so a wait
/// that asks wgpu to block with a timeout panics here within a second.
fn race(gpu: &Gpu, time: Duration) -> Race {
    let stop = Arc::new(AtomicBool::new(false));
    let panics = Arc::new(AtomicUsize::new(0));
    let waits = Arc::new(AtomicUsize::new(0));
    let stuck = Arc::new(AtomicUsize::new(0));
    let caught_before = gpu.poll_panics();
    let mut threads = Vec::new();
    for t in 0..6 {
        let (gpu, stop, panics, waits, stuck) =
            (gpu.clone(), stop.clone(), panics.clone(), waits.clone(), stuck.clone());
        threads.push(std::thread::spawn(move || {
            let Ok(buffer) = Buffer::of::<u32>(&gpu, 64, "race") else { return };
            let count = |r: std::thread::Result<()>| {
                if r.is_err() {
                    panics.fetch_add(1, Ordering::Relaxed);
                }
            };
            while !stop.load(Ordering::Relaxed) {
                match t {
                    0..3 => {
                        let timeout = if t == 0 { Duration::ZERO } else { Duration::from_millis(1) };
                        let Ok(read) = buffer.read_async(&gpu, None) else { continue };
                        count(catch_unwind(AssertUnwindSafe(|| {
                            read.wait(&gpu, timeout);
                        })));
                        waits.fetch_add(1, Ordering::Relaxed);
                        // Every readback gets there in the end, polled by whoever polls.
                        let deadline = Instant::now() + Duration::from_secs(10);
                        while read.state() == ReadState::Waiting && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_micros(200));
                        }
                        if read.state() != ReadState::Ready {
                            stuck.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    3 => {
                        // A readback nobody waits for: a submission and a poll.
                        drop(buffer.read_async(&gpu, None));
                        count(catch_unwind(AssertUnwindSafe(|| gpu.poll())));
                    }
                    _ => count(catch_unwind(AssertUnwindSafe(|| gpu.poll()))),
                }
            }
        }));
    }
    let end = Instant::now() + time;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        if t.join().is_err() {
            panics.fetch_add(1, Ordering::Relaxed);
        }
    }
    let caught = usize::try_from(gpu.poll_panics() - caught_before).unwrap_or(usize::MAX);
    Race {
        panics: panics.load(Ordering::Relaxed) + caught,
        waits: waits.load(Ordering::Relaxed),
        stuck: stuck.load(Ordering::Relaxed),
    }
}

fn check(gpu: &Gpu, time: Duration) {
    let r = race(gpu, time);
    eprintln!("{} ({:?}): {r:?}", gpu.name(), gpu.info().device_type);
    assert!(r.waits > 0, "no wait ran: {r:?}");
    assert_eq!(r.panics, 0, "a wait or a poll panicked: {r:?}");
    assert_eq!(r.stuck, 0, "a readback never arrived: {r:?}");
}

#[test]
fn short_readback_waits_do_not_panic_while_other_threads_poll_and_submit() {
    let Some(gpu) = crate::testing::gpu() else { return };
    check(&gpu, Duration::from_secs(3));
}

/// The same on the fast GPU, where the race is rarer: `cargo test -p penumbra-gpu -- --ignored
/// on_the_hardware_adapter`.
#[test]
#[ignore = "needs a hardware adapter and runs for ten seconds"]
fn short_readback_waits_do_not_panic_on_the_hardware_adapter() {
    let Ok(gpu) = Gpu::headless(AdapterChoice::Hardware) else { return };
    check(&gpu, Duration::from_secs(10));
}

/// Makes every poll on this thread panic while it lives, as on a device whose loss wgpu treats
/// as fatal.
struct PanickingPolls;

impl PanickingPolls {
    fn new() -> Self {
        crate::testing::POLL_PANICS.set(true);
        Self
    }
}

impl Drop for PanickingPolls {
    fn drop(&mut self) {
        crate::testing::POLL_PANICS.set(false);
    }
}

#[test]
fn a_wait_gives_up_at_once_when_every_poll_panics() {
    // A device of its own: the panicking polls must not count against the other tests.
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    let buffer = Buffer::of::<u32>(&gpu, 64, "panics").unwrap();
    let before = gpu.poll_panics();
    let _panics = PanickingPolls::new();
    let t = Instant::now();
    assert!(gpu.wait(Duration::from_secs(10)).is_err());
    assert!(buffer.read::<u32>(&gpu).is_err());
    assert!(gpu.read(&crate::Tex::new(&gpu, 4, 4, "panics").unwrap()).is_err());
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    assert_eq!(gpu.poll_panics() - before, 3 * u64::from(crate::MAX_POLL_PANICS));
}

#[test]
fn a_wait_on_a_lost_device_ends_at_once() {
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    gpu.simulate_loss("test");
    let t = Instant::now();
    assert!(gpu.wait(Duration::from_secs(10)).is_err());
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
}

#[test]
fn a_readback_the_gpu_finished_arrives_however_long_nobody_polled() {
    // A device of its own: nothing else may poll it while the test does not.
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    let buffer = Buffer::of::<u32>(&gpu, 64, "unpolled").unwrap();
    let mut bytes = vec![0u8; 4 * 4 * 8];
    let tex = gpu.upload_rgba16f(4, 4, &mut bytes, "unpolled").unwrap();
    let mut read = buffer.read_async(&gpu, None).unwrap();
    let mut pending = gpu.read_async(&[&tex]).unwrap();
    read.set_timeout(Duration::from_millis(20));
    pending.set_timeout(Duration::from_millis(20));
    std::thread::sleep(Duration::from_millis(300));
    // Past the time, but no poll has looked since (a submission may have delivered them).
    assert_ne!(read.state(), ReadState::Failed);
    assert_ne!(pending.state(), ReadState::Failed);
    assert_eq!(read.wait(&gpu, Duration::from_secs(10)), ReadState::Ready);
    assert_eq!(pending.wait(&gpu, Duration::from_secs(10)), ReadState::Ready);
}

#[test]
fn a_readback_past_its_time_fails_after_a_poll_that_did_not_deliver() {
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    let buffer = Buffer::of::<u32>(&gpu, 64, "undelivered").unwrap();
    let mut read = buffer.read_async(&gpu, None).unwrap();
    read.set_timeout(Duration::ZERO);
    let _panics = PanickingPolls::new();
    let t = Instant::now();
    assert_eq!(read.wait(&gpu, Duration::from_secs(10)), ReadState::Failed);
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
}
