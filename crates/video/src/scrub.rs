//! Frames kept around a scrubbed position ([`crate::Player::scrub`]).
//!
//! A channel that says where the clip is can move it anywhere: forwards, backwards, in jumps, or
//! not at all. Queues of one pass in order do not serve that, so while scrubbing the player keeps
//! a cache of decoded frames around the position instead, shared by its two threads:
//!
//! - the thread *ahead* decodes the frames from the position on, in order (cheap: each is the
//!   next one after the last);
//! - the thread *behind* fills the frames before it. Going backwards in a long-GOP codec means
//!   seeking to the keyframe before and decoding forward, so it decodes a short chunk ending at
//!   the first missing frame behind the position, then the chunk before that, and so on. The
//!   decoder keeps its own cache of the GOP, so the chunks after the first are cheap.
//!
//! The cache holds at most [`CACHE_BYTES`] and [`CACHE_MAX`] frames. When full, the frame farthest
//! from the position goes (counted around the loop for a looping clip). The window both threads
//! fill is sized to fit, so they never decode a frame only to throw it away.

use std::collections::BTreeMap;

use crate::Frame;

/// Bytes of decoded frames the cache may hold.
pub const CACHE_BYTES: usize = 256 << 20;
/// Most frames the cache holds (small frames would otherwise make thousands).
pub const CACHE_MAX: usize = 600;
/// Frames the cache holds at least, whatever their size.
const CACHE_MIN: usize = 4;
/// Frames the thread behind decodes per seek.
const BACK_CHUNK: usize = 8;
/// Failed frames remembered (so a broken frame is not decoded over and over).
const FAILED_MAX: usize = 1024;

/// Which part of the window a thread fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Ahead,
    Behind,
}

/// The frames around the position, shared by the player and its threads.
#[derive(Default)]
pub struct Cache {
    /// Scrubbing is on: the threads fill the window.
    pub active: bool,
    /// Changes whenever the cache is emptied, so a frame decoded before is not put back.
    pub generation: u64,
    frames: BTreeMap<usize, Frame>,
    failed: BTreeMap<usize, String>,
    bytes: usize,
    /// Size of one frame, once one was decoded.
    frame_bytes: usize,
    focus: usize,
    total: usize,
    looping: bool,
    /// Decodes that were not the next frame after the decoder's last (seeks).
    pub seeks: u64,
}

impl Cache {
    /// Empties the cache and switches scrubbing on or off.
    pub fn reset(&mut self, active: bool) {
        self.active = active;
        self.generation += 1;
        self.frames.clear();
        self.failed.clear();
        self.bytes = 0;
    }

    /// Where the position is: frame `focus` of a clip of `total` frames. False when nothing
    /// changed.
    pub fn focus(&mut self, focus: usize, total: usize, looping: bool) -> bool {
        let changed = (self.focus, self.total, self.looping) != (focus, total, looping);
        (self.focus, self.total, self.looping) = (focus, total, looping);
        changed
    }

    /// The frame at the position.
    pub fn focus_frame(&self) -> usize {
        self.focus
    }

    /// The frame at `index` if it is decoded, or the error decoding it gave.
    pub fn get(&self, index: usize) -> Option<Result<Frame, String>> {
        if let Some(f) = self.frames.get(&index) {
            return Some(Ok(f.clone()));
        }
        self.failed.get(&index).map(|e| Err(e.clone()))
    }

    /// Bytes of frames held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Frames held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Frames the window holds: as many as fit, between [`CACHE_MIN`] and [`CACHE_MAX`], at
    /// most the clip.
    fn window(&self) -> usize {
        let fit = CACHE_BYTES.checked_div(self.frame_bytes).unwrap_or(CACHE_MIN);
        // One frame of room, so a frame a little larger than the first still fits.
        fit.saturating_sub(1).clamp(CACHE_MIN, CACHE_MAX).min(self.total.max(1))
    }

    /// The frame `k` frames from the position (negative: before it), wrapped around a
    /// looping clip, `None` beyond the ends of one that does not loop.
    fn at(&self, k: i64) -> Option<usize> {
        let total = i64::try_from(self.total).ok().filter(|t| *t > 0)?;
        let i = i64::try_from(self.focus).ok()?.saturating_add(k);
        let i = if self.looping { i.rem_euclid(total) } else { i };
        (0..total).contains(&i).then(|| usize::try_from(i).ok()).flatten()
    }

    /// Frames between `index` and the position (around the loop when looping).
    fn distance(&self, index: usize) -> usize {
        let d = index.abs_diff(self.focus);
        if self.looping && self.total > 0 { d.min(self.total - d.min(self.total)) } else { d }
    }

    fn missing(&self, index: usize) -> bool {
        !self.frames.contains_key(&index) && !self.failed.contains_key(&index)
    }

    /// What the thread in `role` decodes next. `chunk` is that thread's chunk in progress
    /// (next frame, last frame), kept between calls.
    pub fn next(&self, role: Role, chunk: &mut Option<(usize, usize)>) -> Option<usize> {
        if !self.active || self.total == 0 {
            return None;
        }
        let window = self.window();
        let ahead = window.div_ceil(2);
        let behind = window - ahead;
        match role {
            Role::Ahead => (0..ahead as i64).map_while(|k| self.at(k)).find(|&i| self.missing(i)),
            Role::Behind => {
                // The frame at the position comes first: a jump gets the whole CPU.
                if self.missing(self.focus) {
                    return None;
                }
                // Go on with the chunk while it is still behind the position and in the window.
                if let Some((next, last)) = *chunk {
                    let before = (1..=behind as i64).map_while(|k| self.at(-k)).any(|i| i == next);
                    if next <= last && before {
                        *chunk = Some((next + 1, last));
                        return Some(next);
                    }
                }
                *chunk = None;
                let (k, u) = (1..=behind as i64)
                    .map_while(|k| self.at(-k).map(|i| (k, i)))
                    .find(|&(_, i)| self.missing(i))?;
                // The chunk ends at `u` and starts up to BACK_CHUNK - 1 frames before it, inside
                // the window and without crossing the loop's seam.
                let room = (behind as i64 - k).clamp(0, BACK_CHUNK as i64 - 1) as usize;
                let first = u.saturating_sub(room);
                *chunk = Some((first + 1, u));
                Some(first)
            }
        }
    }

    /// Keeps a decoded frame, making room by dropping the frames farthest from the position.
    /// False when the frame itself is farther than everything held and there is no room.
    pub fn insert(&mut self, index: usize, frame: Result<Frame, String>) -> bool {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                if self.failed.len() >= FAILED_MAX
                    && let Some((&far, _)) = self.failed.iter().max_by_key(|(i, _)| self.distance(**i))
                {
                    self.failed.remove(&far);
                }
                self.failed.insert(index, e);
                return true;
            }
        };
        let size = frame.byte_size();
        if self.frame_bytes == 0 {
            self.frame_bytes = size.max(1);
        }
        if let Some(old) = self.frames.remove(&index) {
            self.bytes = self.bytes.saturating_sub(old.byte_size());
        }
        while !self.frames.is_empty() && (self.bytes + size > CACHE_BYTES || self.frames.len() >= CACHE_MAX) {
            let Some(far) = self.frames.keys().copied().max_by_key(|i| self.distance(*i)) else { break };
            if self.distance(far) <= self.distance(index) {
                return false;
            }
            if let Some(old) = self.frames.remove(&far) {
                self.bytes = self.bytes.saturating_sub(old.byte_size());
            }
        }
        self.bytes += size;
        self.frames.insert(index, frame);
        true
    }
}
