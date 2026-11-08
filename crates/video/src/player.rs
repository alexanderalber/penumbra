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
//!
//! A clip that does not loop has no use for the second thread at its end, so that thread decodes
//! the *cue* instead: where [`Player::restart`] starts playback over. A restart then is a queue
//! switch as well. A looping clip whose cue is its start gets the same from the loop's pre-roll.
//!
//! [`Player::scrub`] serves a position that a channel moves anywhere: the threads then leave the
//! queues and fill a cache of frames around the position (see the `scrub` module).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::scrub::{Cache, Role};
use crate::{ClipInfo, Decoder, Frame, Limits, guarded, open};
use penumbra_core::lock;

/// Bytes of decoded frames one thread may queue.
const QUEUE_BYTES: usize = 192 << 20;
/// Frames one thread may queue at most and at least (the least wins over the bytes).
const QUEUE_MAX: usize = 16;
const QUEUE_MIN: usize = 2;
/// How far ahead of a thread's decoding a wanted frame may be before seeking beats catching up.
const CATCH_UP_SECONDS: f64 = 2.0;
/// How long a scrubbing thread with nothing to do waits before it looks at the cache again.
const SCRUB_IDLE: std::time::Duration = std::time::Duration::from_millis(5);

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
    /// Restarts ([`Player::restart`]).
    pub restarts: u64,
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
    /// Scrubbing: the part of the cache's window this thread fills (instead of a run).
    scrub: Option<Role>,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// The frames around a scrubbed position, the same for both threads.
    cache: Arc<Mutex<Cache>>,
    /// Called after every decoded frame (the node's [`Player::new`] `on_frame`).
    on_frame: Option<OnFrame>,
}

/// Tells the owner a frame arrived, from a decoding thread.
pub type OnFrame = Arc<dyn Fn() + Send + Sync>;

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
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
    /// The iteration playback (re)started at: the `t` of [`Player::show`] counts from it.
    epoch: u64,
    /// The iteration [`Player::show`] wanted last.
    last_iter: u64,
    /// Seconds into the clip where a restart begins.
    cue: f64,
    /// A restart happened and no frame was asked for since (so its jump is not a loop).
    restarting: bool,
    /// The threads fill the cache around a scrubbed position ([`Player::scrub`]).
    scrubbing: bool,
    cache: Arc<Mutex<Cache>>,
    /// The cache's seek count already added to the stats.
    cache_seeks: u64,
}

impl Player {
    /// Starts opening `path` on two threads; nothing here blocks. `on_frame` is called from a
    /// decoding thread whenever a frame (or an error) is queued or the clip opened.
    pub fn new(path: &Path, limits: Limits, on_frame: Option<OnFrame>) -> Result<Self, String> {
        let cache = Arc::new(Mutex::new(Cache::default()));
        let make = || {
            Arc::new(Shared {
                state: Mutex::new(State {
                    opened: None,
                    run: None,
                    queue: VecDeque::new(),
                    capacity: QUEUE_MIN,
                    scrub: None,
                    quit: false,
                }),
                wake: Condvar::new(),
                cache: cache.clone(),
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
            epoch: 0,
            last_iter: 0,
            cue: 0.0,
            restarting: false,
            scrubbing: false,
            cache,
            cache_seeks: 0,
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

    /// (loop pass since the last restart, frame) of the frame shown last.
    pub fn position(&self) -> Option<(u64, usize)> {
        self.current.map(|(iter, frame)| (iter.saturating_sub(self.epoch), frame))
    }

    /// Where a restart begins, in seconds into the clip (clamped to it). Not looping, the
    /// second thread keeps the frames from there decoded, so the restart shows at once.
    pub fn set_cue(&mut self, t: f64) {
        self.cue = if t.is_finite() { t.max(0.0) } else { 0.0 };
    }

    /// Starts playback over: from the next [`Player::show`] on, `t` counts from the start of a
    /// new pass, so the caller passes the cue time again and moves on from there. Never waits;
    /// the frames come from the thread holding the cue or, if none does, from a seek.
    pub fn restart(&mut self) {
        self.epoch = self.last_iter.saturating_add(1);
        self.restarting = true;
        self.stats.restarts += 1;
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
        if self.scrubbing {
            self.set_scrubbing(false);
        }
        let d = info.duration();
        let key = self.key(&info, t, looping);
        let iter = key.0;
        // Without keyframes a seek decodes from the start: catching up is never slower.
        let catch_up = if info.seeks {
            ((CATCH_UP_SECONDS * info.rate()).ceil() as usize).max(QUEUE_MAX)
        } else {
            usize::MAX
        };

        let result =
            if self.current == Some(key) { Ok(Shown::Same) } else { self.take(key, info.frames, catch_up) };
        self.restarting = false;
        // Next for the second thread: the start of the next pass, or where a restart goes.
        let next = if looping { 0 } else { info.index_at(self.cue.min(d)) };
        self.preroll(iter + 1, next, info.frames);
        result
    }

    /// (iteration, frame) due `t` seconds into playback.
    fn key(&mut self, info: &ClipInfo, t: f64, looping: bool) -> (u64, usize) {
        let d = info.duration();
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let (iter, local) = if looping {
            // The hair of tolerance `index_at` has, in passes: a playhead summed from frame times
            // lands a little short of the end of a pass as often as on it. Without it the last
            // frame shows twice and the next pass's first frame not at all.
            let k = (t / d + 1e-6 / info.frames.max(1) as f64).floor();
            (self.epoch.saturating_add(k as u64), t - k * d)
        } else {
            (self.epoch, t.min(d))
        };
        self.last_iter = iter;
        (iter, info.index_at(local))
    }

    /// The frame at `t` seconds into playback, as [`Player::show`], for a position that may move
    /// anywhere between calls: forwards, backwards, in jumps or not at all. The threads keep the
    /// frames around it decoded (see the `scrub` module), so moving back and forth over them shows
    /// each at once; a jump outside them is late until decoded. Calling [`Player::show`] again
    /// plays on from there and lets the cache go.
    pub fn scrub(&mut self, t: f64, looping: bool) -> Result<Shown, String> {
        let Some(info) = self.info()?.cloned() else {
            return Ok(Shown::Waiting);
        };
        if !self.scrubbing {
            self.set_scrubbing(true);
        }
        let key = self.key(&info, t, looping);
        let (moved, got, seeks) = {
            let mut c = lock(&self.cache);
            let moved = c.focus(key.1, info.frames, looping);
            (moved, c.get(key.1), c.seeks)
        };
        self.stats.seeks += seeks.saturating_sub(self.cache_seeks);
        self.cache_seeks = seeks;
        if moved {
            self.poke();
        }
        self.restarting = false;
        if self.current.is_some_and(|(_, i)| i == key.1) {
            // The frame shown, maybe in another pass of the loop.
            self.current = Some(key);
            return Ok(Shown::Same);
        }
        match got {
            Some(Ok(frame)) => {
                self.current = Some(key);
                self.late_key = None;
                self.stats.shown += 1;
                Ok(Shown::New(frame))
            }
            Some(Err(e)) => {
                // Counted as shown, so the error is reported once.
                self.current = Some(key);
                Err(e)
            }
            None => Ok(self.not_ready(key)),
        }
    }

    /// Bytes and frames the scrub cache holds.
    pub fn cached(&self) -> (usize, usize) {
        let c = lock(&self.cache);
        (c.bytes(), c.len())
    }

    /// Moves both threads from their runs to the cache, or back.
    fn set_scrubbing(&mut self, on: bool) {
        self.scrubbing = on;
        lock(&self.cache).reset(on);
        for (i, w) in self.workers.iter().enumerate() {
            let mut s = w.lock();
            s.run = None;
            s.queue.clear();
            s.scrub = on.then_some(if i == 0 { Role::Ahead } else { Role::Behind });
            drop(s);
            w.wake.notify_all();
        }
        self.active = None;
    }

    /// Wakes both threads after the position moved. Each one's lock is taken first, so a thread
    /// that has just found nothing to do in the cache is waiting by now and gets the wake.
    fn poke(&self) {
        for w in &self.workers {
            drop(w.lock());
            w.wake.notify_all();
        }
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
        if self.active.is_some_and(|a| a != w) && index == 0 && !self.restarting {
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

    /// Has the thread that is not active decode iteration `iter` from frame `start`, once the
    /// active one is ahead.
    fn preroll(&mut self, iter: u64, start: usize, frames: usize) {
        let Some(a) = self.active else { return };
        let o = 1 - a.min(1);
        let ready = self.w(o).lock().run.is_some_and(|r| r.iter == iter && r.start == start);
        if ready {
            return;
        }
        let ahead = {
            let s = self.w(a).lock();
            s.run.is_some_and(|r| r.next >= r.end || s.queue.len() >= s.capacity)
        };
        if ahead {
            self.assign(o, iter, start, frames);
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

/// One frame, with a panic in the decoder turned into an error frame. A thread that died here
/// would leave its clip frozen on the last frame with nothing reported.
pub(crate) fn decode(decoder: &mut dyn Decoder, index: usize) -> Result<Frame, String> {
    guarded(|| decoder.frame(index))
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
    // The frame this thread decoded last, and the chunk it fills behind a scrubbed position.
    let mut last: Option<usize> = None;
    let mut chunk = None;
    // The cache had no room for a scrubbed frame: nothing to do until the position moves.
    let mut full_at: Option<(u64, usize)> = None;
    enum Job {
        Run(u64, usize),
        Scrub { generation: u64, index: usize, focus: usize },
    }
    loop {
        let job = {
            let mut s = shared.lock();
            loop {
                if s.quit {
                    return;
                }
                if let Some(r) = s.run
                    && r.next < r.end
                    && s.queue.len() < s.capacity
                {
                    break Job::Run(r.id, r.next);
                }
                if let Some(role) = s.scrub {
                    let c = lock(&shared.cache);
                    let stuck = full_at.is_some_and(|at| at == (c.generation, c.focus_frame()));
                    if !stuck && let Some(i) = c.next(role, &mut chunk) {
                        break Job::Scrub { generation: c.generation, index: i, focus: c.focus_frame() };
                    }
                    drop(c);
                    // The other thread's frames change what is left to do here, and it does not
                    // wake this one: look again soon.
                    s = shared.wake.wait_timeout(s, SCRUB_IDLE).unwrap_or_else(PoisonError::into_inner).0;
                    continue;
                }
                s = shared.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
        };
        match job {
            Job::Run(id, index) => {
                let result = decode(decoder.as_mut(), index);
                last = Some(index);
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
            Job::Scrub { generation, index, focus } => {
                let seek = last.is_none_or(|l| l.checked_add(1) != Some(index));
                let result = decode(decoder.as_mut(), index);
                last = Some(index);
                let mut c = lock(&shared.cache);
                if c.generation != generation {
                    continue;
                }
                if seek {
                    c.seeks += 1;
                }
                // Refused for the position it was decoded for: the same frame would come next
                // again. Refused because the position moved meanwhile: the next pick is another.
                full_at = None;
                if !c.insert(index, result) {
                    chunk = None;
                    if c.focus_frame() == focus {
                        full_at = Some((generation, focus));
                    }
                }
                drop(c);
                notify();
            }
        }
    }
}
