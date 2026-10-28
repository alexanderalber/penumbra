//! Which samples a generator on the show clock owes per cook: the block contract of
//! [`DataKind::Audio`](penumbra_graph::DataKind::Audio).
//!
//! The sample of tick `t` at rate `r` is `round(t * r / TICKS_PER_SECOND)`, computed in integers.
//! A cook at tick `now` after a cook at `prev` owes the samples `sample(prev)..sample(now)`, so
//! blocks join exactly and never drift, whatever the frame rate.

use penumbra_core::Tick;
use penumbra_core::time::TICKS_PER_SECOND;

/// Most time one block catches up after skipped frames; a longer gap starts over.
pub const MAX_GAP_SECONDS: f64 = 0.5;

/// The sample that tick `t` falls on at `rate` samples per second: rounded half up, exact for
/// any tick (in 128-bit integers).
pub fn sample_at(t: Tick, rate: u32) -> i64 {
    let tps = i128::from(TICKS_PER_SECOND);
    let x = i128::from(t.0) * i128::from(rate);
    // Floor division of x + tps / 2, also for negative ticks.
    let q = (x + tps / 2).div_euclid(tps);
    i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX })
}

/// How this cook's block relates to the one before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// It follows the previous block without a gap.
    Continue,
    /// The same block again (the tick has not moved).
    Again,
    /// No previous block to follow: the first cook, a seek backwards, a gap over
    /// [`MAX_GAP_SECONDS`] or another rate. One frame's worth, ending now.
    Fresh,
}

/// The samples one cook outputs, as sample numbers counted from tick 0: `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    pub step: Step,
}

impl Span {
    pub fn len(&self) -> usize {
        usize::try_from(self.end.saturating_sub(self.start)).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// The ticks of the last block a generator made, to tell what the next cook owes.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockClock {
    /// (start tick, end tick, rate) of the last block.
    last: Option<(Tick, Tick, u32)>,
}

impl BlockClock {
    /// The block owed at tick `now`, for a show whose frames are `frame_ticks` long, at `rate`
    /// samples per second. Remembers it as the last block.
    pub fn step(&mut self, now: Tick, frame_ticks: i64, rate: u32) -> Span {
        let max_gap = Tick::from_seconds(MAX_GAP_SECONDS).0;
        let (from, step) = match self.last {
            Some((start, end, r)) if r == rate && now == end => (start, Step::Again),
            Some((_, end, r)) if r == rate && now > end && now.0 - end.0 <= max_gap => (end, Step::Continue),
            _ => (Tick(now.0.saturating_sub(frame_ticks.max(1))), Step::Fresh),
        };
        self.last = Some((from, now, rate));
        let end = sample_at(now, rate);
        let mut start = sample_at(from, rate);
        if step == Step::Fresh {
            // A frame shorter than a sample still gives one sample.
            start = start.min(end.saturating_sub(1));
        }
        Span { start, end, step }
    }

    /// Forgets the last block: the next cook starts over.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::Rate;

    /// The sum of the block lengths over many frames is the elapsed time in samples, exactly.
    #[test]
    fn blocks_add_up_without_drift() {
        for (rate, fps, per_frame) in [(48_000, 60, None), (44_100, 60, Some([735, 736])), (48_000, 50, None)]
        {
            let show = Rate::fps(fps).unwrap();
            let mut clock = BlockClock::default();
            // The first cook (frame 0) is a fresh frame's worth; count from frame 1 on.
            clock.step(show.tick_of(0), show.frame_ticks(), rate);
            let frames: i64 = 60 * 60 * i64::from(fps);
            let mut total: i64 = 0;
            let mut prev_end = sample_at(show.tick_of(0), rate);
            for f in 1..=frames {
                let span = clock.step(show.tick_of(f), show.frame_ticks(), rate);
                assert_eq!(span.step, Step::Continue);
                assert_eq!(span.start, prev_end, "a gap or overlap at frame {f}");
                prev_end = span.end;
                if let Some(lens) = per_frame {
                    assert!(lens.contains(&span.len()), "{rate}/{fps}: {}", span.len());
                } else {
                    assert_eq!(span.len() as i64, i64::from(rate) / i64::from(fps));
                }
                total += span.len() as i64;
            }
            assert_eq!(total, frames * i64::from(rate) / i64::from(fps), "{rate} Hz at {fps} fps");
        }
    }

    #[test]
    fn a_fractional_frame_rate_alternates_and_does_not_drift() {
        let show = Rate::new(30_000, 1001).unwrap();
        let mut clock = BlockClock::default();
        clock.step(show.tick_of(0), show.frame_ticks(), 48_000);
        let mut total = 0;
        let frames = 30_000;
        for f in 1..=frames {
            total += clock.step(show.tick_of(f), show.frame_ticks(), 48_000).len() as i64;
        }
        // 30000 frames at 30000/1001 fps are 1001 s.
        assert_eq!(total, 1001 * 48_000);
    }

    #[test]
    fn skipped_frames_come_as_one_block_a_long_gap_starts_over() {
        let show = Rate::FPS_60;
        let ft = show.frame_ticks();
        let mut clock = BlockClock::default();
        let first = clock.step(show.tick_of(10), ft, 48_000);
        assert_eq!((first.step, first.len()), (Step::Fresh, 800));
        let skip = clock.step(show.tick_of(13), ft, 48_000);
        assert_eq!((skip.step, skip.start, skip.len()), (Step::Continue, first.end, 2400));
        let again = clock.step(show.tick_of(13), ft, 48_000);
        assert_eq!(again, Span { step: Step::Again, ..skip });
        // 30 frames are half a second: still caught up.
        let half = clock.step(show.tick_of(43), ft, 48_000);
        assert_eq!((half.step, half.len()), (Step::Continue, 24_000));
        let gap = clock.step(show.tick_of(74), ft, 48_000);
        assert_eq!((gap.step, gap.len()), (Step::Fresh, 800));
        let back = clock.step(show.tick_of(5), ft, 48_000);
        assert_eq!((back.step, back.len()), (Step::Fresh, 800));
        let other_rate = clock.step(show.tick_of(6), ft, 44_100);
        assert_eq!(other_rate.step, Step::Fresh);
    }

    #[test]
    fn sample_numbers_round_half_up_also_before_zero() {
        assert_eq!(sample_at(Tick(0), 48_000), 0);
        assert_eq!(sample_at(Tick(TICKS_PER_SECOND), 48_000), 48_000);
        assert_eq!(sample_at(Tick(-TICKS_PER_SECOND / 60), 48_000), -800);
        let tick_per_sample = TICKS_PER_SECOND / 48_000;
        assert_eq!(sample_at(Tick(tick_per_sample / 2), 48_000), 1);
        assert_eq!(sample_at(Tick(tick_per_sample / 2 - 1), 48_000), 0);
        assert_eq!(sample_at(Tick(-tick_per_sample / 2), 48_000), 0);
        // Far ticks do not overflow.
        assert!(sample_at(Tick(i64::MAX), 192_000) > 0);
        assert!(sample_at(Tick(i64::MIN), 192_000) < 0);
    }
}
