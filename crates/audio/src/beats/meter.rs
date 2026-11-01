//! Regular bars from the network's downbeat logits.
//!
//! The downbeats the network marks one by one make bars of irregular length on DJ material:
//! a mix of two hours gave bars of one, two or five beats in a fifth of all bars. Here every
//! beat looks at the [`WINDOW`] beats around it instead. For each meter (4 and 3 beats per
//! bar) and each phase (which beat of the bar comes first) it averages the downbeat logits of
//! the beats that would begin a bar. The phase with the highest mean wins, and the beat begins
//! a bar when it falls on that phase.
//!
//! Three rules keep the bars steady:
//! - **4 over 3:** 3/4 wins only where its lead over the other phases is [`THREE_LEAD`] times
//!   that of 4/4 and at least [`THREE_MIN_LEAD`]. On 4/4 music each phase of 3/4 mixes all
//!   four beats of the bar, so 3/4 leads by little there. Where neither meter leads, in a
//!   breakdown without downbeats, 4/4 stays.
//! - **Hold:** a new phase of the same meter takes over only when its mean beats the current
//!   phase's by [`HOLD`] of the lead. Otherwise near ties flip the phase back and forth.
//! - **No short bars:** where the phase changes, a bar would begin less than
//!   `beats_per_bar / 2 + 1` beats after the last one; that bar start is dropped, and the
//!   bar before grows instead.
//!
//! Each decision depends on the beats up to [`WINDOW`]` / 2` ahead and on the decisions
//! before it. So with only the start of the piece known, the bars of all beats but the last
//! [`WINDOW`]` / 2` are already final ([`bars_decided`]).

/// Beats around each beat that vote on its bar: 32 before and 31 after it.
pub const WINDOW: usize = 64;
/// How many times larger 3/4's lead must be than 4/4's for 3/4 to win.
pub const THREE_LEAD: f32 = 2.0;
/// The smallest lead, in logits, with which 3/4 wins. 4/4 leads by about 20 on clear 4/4
/// music, a synthetic waltz's 3/4 by 14; where 3/4 won on a DJ mix without this, by at most 2.4.
pub const THREE_MIN_LEAD: f32 = 4.0;
/// The share of the lead by which a new phase must beat the current one.
pub const HOLD: f32 = 0.2;
/// The largest downbeat logit counted, either sign. The network gives about -15 to 14.
pub const LOGIT_LIMIT: f32 = 30.0;

/// The beats (indices into `logits`, one logit per beat) that begin a bar.
pub fn bars(logits: &[f32]) -> Vec<usize> {
    bars_decided(logits, logits.len())
}

/// The bar starts among the first `decided` beats, with the window cut at the end of
/// `logits`. With `decided <= logits.len() - WINDOW / 2` they are those [`bars`] gives for any
/// longer piece that begins with `logits`.
pub fn bars_decided(logits: &[f32], decided: usize) -> Vec<usize> {
    let n = logits.len();
    let mut out = Vec::new();
    // The meter and phase in force.
    let mut current: Option<(usize, usize)> = None;
    let mut last_start: Option<usize> = None;
    for i in 0..decided.min(n) {
        let window = logits.get(i.saturating_sub(WINDOW / 2)..(i + WINDOW / 2).min(n)).unwrap_or(&[]);
        let first = i.saturating_sub(WINDOW / 2);
        let four = vote::<4>(window, first);
        let three = vote::<3>(window, first);
        let (meter, mut phase, lead, means) =
            if three.lead >= THREE_MIN_LEAD && three.lead > THREE_LEAD * four.lead.max(0.0) {
                (3, three.phase, three.lead, three.means.to_vec())
            } else {
                (4, four.phase, four.lead, four.means.to_vec())
            };
        if let Some((m, p)) = current
            && m == meter
            && p != phase
        {
            let (now, new) = (means.get(p).copied(), means.get(phase).copied());
            if let (Some(now), Some(new)) = (now, new)
                && new - now < HOLD * lead
            {
                phase = p;
            }
        }
        current = Some((meter, phase));
        if i % meter != phase {
            continue;
        }
        if last_start.is_some_and(|l| i - l < meter / 2 + 1) {
            continue;
        }
        out.push(i);
        last_start = Some(i);
    }
    out
}

/// One meter's vote over a window of beats.
struct Vote<const M: usize> {
    /// The mean downbeat logit of the beats `first + k` with `(first + k) % M == p`, by `p`.
    means: [f32; M],
    phase: usize,
    /// How far the best phase's mean lies above the mean of the others' means.
    lead: f32,
}

/// The vote of meter `M` over `window`, whose first beat is beat `first` of the piece.
fn vote<const M: usize>(window: &[f32], first: usize) -> Vote<M> {
    let mut sums = [0.0f32; M];
    let mut counts = [0u32; M];
    for (k, v) in window.iter().enumerate() {
        let p = (first + k) % M;
        if let (Some(s), Some(c)) = (sums.get_mut(p), counts.get_mut(p)) {
            *s += if v.is_nan() { -LOGIT_LIMIT } else { v.clamp(-LOGIT_LIMIT, LOGIT_LIMIT) };
            *c += 1;
        }
    }
    let mut means = [f32::NEG_INFINITY; M];
    for ((m, s), c) in means.iter_mut().zip(sums).zip(counts) {
        if c > 0 {
            // `as`: at most WINDOW.
            *m = s / c as f32;
        }
    }
    let mut phase = 0;
    for (p, m) in means.iter().enumerate() {
        if means.get(phase).is_some_and(|b| m > b) {
            phase = p;
        }
    }
    let best = means.get(phase).copied().unwrap_or(0.0);
    let others: Vec<f32> =
        means.iter().enumerate().filter(|(p, m)| *p != phase && m.is_finite()).map(|(_, m)| *m).collect();
    let lead = if others.is_empty() || !best.is_finite() {
        0.0
    } else {
        // `as`: at most 3.
        best - others.iter().sum::<f32>() / others.len() as f32
    };
    Vote { means, phase, lead }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Logits for `n` beats: `hi` where `downbeat(i)`, `lo` elsewhere.
    fn logits(n: usize, downbeat: impl Fn(usize) -> bool) -> Vec<f32> {
        (0..n).map(|i| if downbeat(i) { 5.0 } else { -9.0 }).collect()
    }

    fn lengths(starts: &[usize]) -> Vec<usize> {
        starts.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn clean_four_four_stays_as_marked() {
        let l = logits(200, |i| i % 4 == 2);
        let b = bars(&l);
        assert_eq!(b, (2..200).step_by(4).collect::<Vec<_>>());
    }

    #[test]
    fn stray_and_missing_marks_do_not_break_the_bars() {
        // Every fourth beat from 1, but marks missing, doubled and one beat off here and there.
        let mut l = logits(400, |i| i % 4 == 1);
        for i in [41, 45, 133, 201, 305] {
            l[i] = -9.0;
        }
        for i in [50, 51, 99, 180, 263, 264, 330] {
            l[i] = 5.0;
        }
        let b = bars(&l);
        assert!(lengths(&b).iter().all(|n| *n == 4), "{:?}", lengths(&b));
        assert_eq!(b.first(), Some(&1));
    }

    #[test]
    fn a_dropped_beat_changes_the_phase_once() {
        // The beat tracker missed one beat at 200: the marks after it come one beat earlier.
        let l = logits(400, |i| if i < 200 { i % 4 == 0 } else { i % 4 == 3 });
        let b = bars(&l);
        let len = lengths(&b);
        let odd: Vec<usize> = len.iter().copied().filter(|n| *n != 4).collect();
        // One bar of three or seven, depending on where the vote turns.
        assert!(odd == [3] || odd == [7], "{len:?}");
        assert!(b.contains(&0) && b.contains(&399));
    }

    #[test]
    fn no_bar_of_one_or_two_beats() {
        // A phase change by two beats would leave a bar of two: it grows to six instead.
        let l = logits(400, |i| if i < 200 { i % 4 == 0 } else { i % 4 == 2 });
        let len = lengths(&bars(&l));
        assert!(len.iter().all(|n| *n >= 3), "{len:?}");
        assert_eq!(len.iter().filter(|n| **n != 4).count(), 1, "{len:?}");
    }

    #[test]
    fn three_four_wins_on_a_waltz_and_not_on_four_four() {
        let l = logits(300, |i| i % 3 == 1);
        let b = bars(&l);
        assert!(lengths(&b).iter().all(|n| *n == 3), "{:?}", lengths(&b));
        // 4/4 with noisy marks: never bars of three.
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let l: Vec<f32> = (0..600)
            .map(|i| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let noise = (rng % 1000) as f32 / 100.0 - 5.0;
                (if i % 4 == 0 { 2.0 } else { -6.0 }) + noise
            })
            .collect();
        assert!(lengths(&bars(&l)).iter().all(|n| *n == 4));
    }

    #[test]
    fn decided_bars_begin_those_of_any_longer_piece() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let l: Vec<f32> = (0..700)
            .map(|i| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let shift = if i < 300 {
                    0
                } else if i < 500 {
                    1
                } else {
                    3
                };
                let noise = (rng % 1000) as f32 / 80.0 - 6.0;
                (if (i + shift) % 4 == 0 { 3.0 } else { -7.0 }) + noise
            })
            .collect();
        let full = bars(&l);
        for known in (0..l.len()).step_by(3) {
            let part = bars_decided(&l[..known], known.saturating_sub(WINDOW / 2));
            assert_eq!(part[..], full[..part.len()], "{known}");
            let cut = known.saturating_sub(WINDOW / 2);
            assert_eq!(part.len(), full.iter().filter(|i| **i < cut).count(), "{known}");
        }
    }

    #[test]
    fn odd_input() {
        assert!(bars(&[]).is_empty());
        assert_eq!(bars(&[1.0]), vec![0]);
        let l = [f32::NAN, f32::INFINITY, -1e30, 0.0, 3.0, f32::NEG_INFINITY, 2.0];
        let _ = bars(&l);
        assert!(bars(&[-9.0; 50]).first() == Some(&0));
    }
}
