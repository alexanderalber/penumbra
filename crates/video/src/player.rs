//! Playback without waiting: two decoding threads with bounded queues, and the frame due at a
//! clip time handed to the cook thread.
//!
//! Each thread owns a [`Decoder`] and works through a *run*: one iteration of the clip (loop
//! count `iter`) from a start frame to the end. The run the cook thread shows from is the active
//! one. While the clip loops, the other thread decodes the start of the next iteration and then
//! waits with a full queue, so the jump back to frame 0 is a queue switch, not a seek (the video
//! spike measured 4 to 17 frame periods for that seek). It starts only once the active thread is
//! ahead (its queue full or its run decoded), so the two never compete for the CPU right where a
//! loop just began. A jump in time (a pause scrubbed elsewhere, a speed change that outruns the
//! decoder) seeks on the thread that is not needed for the loop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::{ClipInfo, Decoder, Frame, Limits, open};

/// Bytes of decoded frames one thread may queue.
const QUEUE_BYTES: usize = 192 << 20;
/// Frames one thread may queue at most and at least (the least wins over the bytes).
const QUEUE_MAX: usize = 16;
const QUEUE_MIN: usize = 2;
/// How far ahead of a thread's decoding a wanted frame may be before seeking beats catching up.
const CATCH_UP_SECONDS: f64 = 2.0;

/// What [`Player::show`] has for the cook thread.
#[derive(Clone, Debug)]
pub enum Shown {
    /// A new frame to upload.
    New(Frame),
    /// The frame due is the one shown last.
    Same,
    /// The frame due is not decoded yet; keep showing the last one.
    Late,
    /// Nothing to show yet (opening, or the first frame decoding).
    Waiting,
}

/// Counters since the player started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerStats {
    /// Frames shown.
    pub shown: u64,
    /// Frames that were due but not decoded in time (each counted once).
    pub late: u64,
    /// Seeks (jumps a thread had to make).
    pub seeks: u64,
    /// Loop points crossed onto a decoded start.
    pub loops: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    /// Changes whenever the run is replaced, so a frame decoded for an old run is dropped.
    id: u64,
    iter: u64,
    start: usize,
    /// Next frame to decode.
    next: usize,
    end: usize,
}

struct State {
    opened: Option<Result<ClipInfo, String>>,
    run: Option<Run>,
    queue: VecDeque<(usize, Result<Frame, String>)>,
    capacity: usize,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// Called after every decoded frame (the node's [`Player::new`] `on_frame`).
    on_frame: Option<OnFrame>,
}

/// Tells the owner a frame arrived, from a decoding thread.
pub type OnFrame = Arc<dyn Fn() + Send + Sync>;

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Plays one clip. Dropping it stops its threads (each finishes the frame it is decoding).
pub struct Player {
    path: PathBuf,
    workers: [Arc<Shared>; 2],
    info: Option<ClipInfo>,
    active: Option<usize>,
    /// (iteration, frame) shown last.
    current: Option<(u64, usize)>,
    /// The frame due last time but not ready (counted as late once).
    late_key: Option<(u64, usize)>,
    next_id: u64,
    stats: PlayerStats,
}

impl Player {
    /// Starts opening `path` on two threads; nothing here blocks. `on_frame` is called from a
    /// decoding thread whenever a frame (or an error) is queued or the clip opened.
    pub fn new(path: &Path, limits: Limits, on_frame: Option<OnFrame>) -> Result<Self, String> {
        let make = || {
            Arc::new(Shared {
                state: Mutex::new(State {
                    opened: None,
                    run: None,
                    queue: VecDeque::new(),
                    capacity: QUEUE_MIN,
                    quit: false,
                }),
                wake: Condvar::new(),
                on_frame: on_frame.clone(),
            })
        };
        let workers = [make(), make()];
        for (i, w) in workers.iter().enumerate() {
            let shared = w.clone();
            let p = path.to_path_buf();
            std::thread::Builder::new()
                .name(format!("video {i}"))
                .spawn(move || work(&shared, &p, &limits))
                .map_err(|e| format!("cannot start a decoding thread: {e}"))?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            workers,
            info: None,
            active: None,
            current: None,
            late_key: None,
            next_id: 0,
            stats: PlayerStats::default(),
        })
    }

    /// Worker `i` (0 or 1; anything else is 1).
    fn w(&self, i: usize) -> &Shared {
        let [a, b] = &self.workers;
        if i == 0 { a } else { b }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The clip, once a thread has opened it.
    pub fn info(&mut self) -> Result<Option<&ClipInfo>, String> {
        if self.info.is_none() {
            for w in &self.workers {
                match &w.lock().opened {
                    Some(Ok(info)) => {
                        self.info = Some(info.clone());
                        break;
                    }
                    Some(Err(e)) => return Err(e.clone()),
                    None => {}
                }
            }
        }
        Ok(self.info.as_ref())
    }

    /// (loop pass, frame) of the frame shown last.
    pub fn position(&self) -> Option<(u64, usize)> {
        self.current
    }

    pub fn stats(&self) -> PlayerStats {
        self.stats
    }

    /// The frame due `t` seconds into playback (any value; negative is the start). Looping, `t`
    /// wraps around the clip; otherwise the last frame holds. Never waits.
    pub fn show(&mut self, t: f64, looping: bool) -> Result<Shown, String> {
        let Some(info) = self.info()?.cloned() else {
            return Ok(Shown::Waiting);
        };
        let d = info.duration();
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let (iter, local) = if looping {
            let k = (t / d).floor();
            (k as u64, t - k * d)
        } else {
            (0, t.min(d))
        };
        let index = info.index_at(local);
        let key = (iter, index);
        // Without keyframes a seek decodes from the start: catching up is never slower.
        let catch_up = if info.seeks {
            ((CATCH_UP_SECONDS * info.rate()).ceil() as usize).max(QUEUE_MAX)
        } else {
            usize::MAX
        };

        let result =
            if self.current == Some(key) { Ok(Shown::Same) } else { self.take(key, info.frames, catch_up) };
        if looping {
            self.preroll(iter + 1, info.frames);
        }
        result
    }

    /// Takes frame `key` from the thread that can deliver it, or sends a thread there.
    fn take(&mut self, key: (u64, usize), frames: usize, catch_up: usize) -> Result<Shown, String> {
        let (iter, index) = key;
        let serving = (0..2).find(|&i| {
            let s = self.w(i).lock();
            s.run.is_some_and(|r| {
                let first = s.queue.front().map_or(r.next, |(f, _)| *f);
                r.iter == iter && index >= first && index < r.end && index <= r.next.saturating_add(catch_up)
            })
        });
        let Some(w) = serving else {
            // Seek: the thread not active (the active one may be what the loop needs next).
            let w = match self.active {
                Some(a) => 1 - a.min(1),
                None => 0,
            };
            self.assign(w, iter, index, frames);
            if self.current.is_some() {
                self.stats.seeks += 1;
            }
            self.active = Some(w);
            return Ok(self.not_ready(key));
        };
        if self.active.is_some_and(|a| a != w) && index == 0 {
            self.stats.loops += 1;
        }
        self.active = Some(w);
        let shared = self.w(w);
        let mut s = shared.lock();
        while s.queue.front().is_some_and(|(f, _)| *f < index) {
            s.queue.pop_front();
        }
        let got = match s.queue.front() {
            Some((f, _)) if *f == index => s.queue.pop_front().map(|(_, r)| r),
            _ => None,
        };
        drop(s);
        shared.wake.notify_all();
        match got {
            Some(Ok(frame)) => {
                self.current = Some(key);
                self.late_key = None;
                self.stats.shown += 1;
                Ok(Shown::New(frame))
            }
            Some(Err(e)) => {
                // Counted as shown, so the error is reported once and playback moves on.
                self.current = Some(key);
                Err(e)
            }
            None => Ok(self.not_ready(key)),
        }
    }

    fn not_ready(&mut self, key: (u64, usize)) -> Shown {
        if self.current.is_none() {
            return Shown::Waiting;
        }
        if self.late_key != Some(key) {
            self.late_key = Some(key);
            self.stats.late += 1;
        }
        Shown::Late
    }

    /// Has the thread that is not active decode the start of iteration `iter`, once the active
    /// one is ahead.
    fn preroll(&mut self, iter: u64, frames: usize) {
        let Some(a) = self.active else { return };
        let o = 1 - a.min(1);
        let ready = self.w(o).lock().run.is_some_and(|r| r.iter == iter && r.start == 0);
        if ready {
            return;
        }
        let ahead = {
            let s = self.w(a).lock();
            s.run.is_some_and(|r| r.next >= r.end || s.queue.len() >= s.capacity)
        };
        if ahead {
            self.assign(o, iter, 0, frames);
        }
    }

    fn assign(&mut self, w: usize, iter: u64, start: usize, end: usize) {
        self.next_id += 1;
        let shared = self.w(w);
        let mut s = shared.lock();
        s.queue.clear();
        s.run = Some(Run { id: self.next_id, iter, start, next: start, end });
        drop(s);
        shared.wake.notify_all();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        for w in &self.workers {
            w.lock().quit = true;
            w.wake.notify_all();
        }
    }
}

/// A decoding thread: opens its own decoder, then decodes whatever its run asks for while the
/// queue has room.
fn work(shared: &Shared, path: &Path, limits: &Limits) {
    let notify = || {
        if let Some(f) = &shared.on_frame {
            f();
        }
    };
    let mut decoder: Box<dyn Decoder> = match open(path, limits) {
        Ok(d) => d,
        Err(e) => {
            shared.lock().opened = Some(Err(e));
            notify();
            return;
        }
    };
    shared.lock().opened = Some(Ok(decoder.info().clone()));
    notify();
    let mut sized = false;
    loop {
        let (id, index) = {
            let mut s = shared.lock();
            loop {
                if s.quit {
                    return;
                }
                if let Some(r) = s.run
                    && r.next < r.end
                    && s.queue.len() < s.capacity
                {
                    break (r.id, r.next);
                }
                s = shared.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = decoder.frame(index);
        let mut s = shared.lock();
        if !sized && let Ok(f) = &result {
            sized = true;
            s.capacity = (QUEUE_BYTES / f.byte_size().max(1)).clamp(QUEUE_MIN, QUEUE_MAX);
        }
        if let Some(r) = s.run.as_mut()
            && r.id == id
        {
            r.next += 1;
            s.queue.push_back((index, result));
            drop(s);
            notify();
        }
    }
}
