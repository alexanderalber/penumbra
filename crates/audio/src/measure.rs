//! Measures how long a sound takes from [`Player::push`] to an input: a quiet click train goes
//! out through a player of its own, an input stream of its own records it, and a
//! cross-correlation finds each click in each input channel.
//!
//! What is measured, per click: the time from the `push` call whose block starts with the
//! click to the moment the input hands the click's first sample to the program. That moment is
//! known to about a millisecond: the input is read every millisecond, and the earliest
//! arrival of all reads fixes when each input sample became available. The input's own delay
//! (from capture to its callback, as the system reports it) is subtracted for an estimate of
//! the output's part: from the push to the sound reaching the converter, or, through an
//! interface's internal loopback, the interface's mixer.
//!
//! The clicks are short chirps from 1 to 8 kHz at most -20 dBFS. The call blocks for some
//! seconds; it never runs on the cook thread.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::capture::{Cursor, Hub};
use crate::limiter::db_to_gain;
use crate::playback::{Player, Settings, State};

/// The loudest click allowed, dBFS.
pub const MAX_LEVEL_DB: f64 = -20.0;
/// Rate the clicks are made at (the player resamples to the device).
const RATE: u32 = 48_000;
/// Length of a click, seconds.
const CLICK_SECONDS: f64 = 0.005;
/// Time between clicks, seconds.
const SPACING_SECONDS: f64 = 0.6;
/// How far after its push a click is looked for, seconds.
const SEARCH_SECONDS: f64 = 0.5;

/// What to measure.
#[derive(Clone, Debug)]
pub struct Spec {
    /// Part of the output's name (empty: the default output).
    pub output: String,
    /// Part of the input's name (empty: the default input).
    pub input: String,
    /// Input channels to look at, counted from 1 (empty: all).
    pub channels: Vec<usize>,
    /// Clicks to play (1 to 40).
    pub clicks: usize,
    /// The clicks' peak, dBFS (at most [`MAX_LEVEL_DB`]).
    pub level_db: f64,
    /// The player's settings.
    pub settings: Settings,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            output: String::new(),
            input: String::new(),
            channels: Vec::new(),
            clicks: 8,
            level_db: -30.0,
            settings: Settings::default(),
        }
    }
}

/// One input channel's result.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ChannelResult {
    /// Counted from 1.
    pub channel: usize,
    /// Clicks found clearly above the channel's noise.
    pub found: usize,
    /// Median time from push to arrival in the program, ms.
    pub round_trip_ms: Option<f64>,
    /// Slowest minus fastest click, ms.
    pub spread_ms: Option<f64>,
    /// The loudest click as recorded, dBFS.
    pub peak_db: f64,
    /// The best correlation peak over the noise before the first click.
    pub clarity: f64,
    /// Every click found, in the order played, ms.
    pub each_ms: Vec<f64>,
}

/// What a measurement found.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub output_rate: u32,
    pub input_rate: u32,
    pub input_channels: u16,
    pub clicks: usize,
    pub level_db: f64,
    pub channels: Vec<ChannelResult>,
    /// The channel with the most clicks found (then the clearest).
    pub best_channel: Option<usize>,
    /// Its median from push to arrival in the program, ms.
    pub round_trip_ms: Option<f64>,
    /// The input's own delay as the system reports it (capture to callback), ms.
    pub input_device_ms: f64,
    /// The output's part estimated: round trip minus the input's delay, ms.
    pub output_ms: Option<f64>,
    /// What the player itself reported meanwhile: its fill, its device buffer and their sum.
    pub player_fill_ms: f64,
    pub player_device_ms: f64,
    pub player_latency_ms: f64,
    pub player_callback_frames: usize,
    pub player_buffer_frames: u32,
    pub underruns: u64,
    pub note: String,
}

/// The click at `rate`: a Hann-windowed chirp from 1 to 8 kHz with peak `amp`.
fn click(rate: u32, amp: f64) -> Vec<f64> {
    let r = f64::from(rate);
    // `as`: a few hundred samples.
    let n = (CLICK_SECONDS * r).round() as usize;
    let (f0, f1) = (1000.0, 8000.0);
    (0..n)
        .map(|i| {
            let t = i as f64 / r;
            let k = (f1 - f0) / CLICK_SECONDS;
            let phase = std::f64::consts::TAU * (f0 * t + 0.5 * k * t * t);
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n.max(1) as f64).cos();
            amp * w * phase.sin()
        })
        .collect()
}

/// Plays the clicks and finds them. Blocks for about `clicks * 0.6 + 2` seconds.
pub fn run(spec: &Spec) -> Result<Report, String> {
    let level = if spec.level_db.is_finite() { spec.level_db.min(MAX_LEVEL_DB) } else { MAX_LEVEL_DB };
    let clicks = spec.clicks.clamp(1, 40);
    let hub = Hub::new();
    let input_key = spec.input.trim().to_string();
    hub.set_wanted(std::slice::from_ref(&input_key));
    // The player starts first: a loopback input delivers nothing while its output is idle.
    let player = Player::open(&spec.output, spec.settings);
    let start = Instant::now();
    let mut input: Option<crate::capture::StreamStatus> = None;
    let mut cursor = Cursor::default();
    let mut recorded: Vec<Vec<f32>> = Vec::new();
    // The earliest time (since `start`) any input sample can have arrived, as `t - frames / rate`.
    let mut origin = f64::INFINITY;
    let mut pushed: u64 = 0;
    let tone = click(RATE, db_to_gain(level));
    // Per click: when the block starting with it was pushed (since `start`), and its sample.
    let mut times: Vec<f64> = Vec::new();
    let mut starts: Vec<u64> = Vec::new();
    let mut next_click: Option<u64> = None;
    let mut playing_since: Option<f64> = None;
    let mut status = player.status();
    let mut fail = None;
    loop {
        let now = start.elapsed().as_secs_f64();
        // `as`: sample counts of a few seconds.
        let due = (now * f64::from(RATE)) as u64;
        while pushed < due {
            let mut end = due;
            if let Some(c) = next_click
                && pushed < c
            {
                end = end.min(c);
            }
            let starts_click = next_click == Some(pushed);
            if starts_click {
                end = end.max(pushed + 1);
                starts.push(pushed);
                next_click = None;
            }
            let block: Vec<f64> = (pushed..end)
                .map(|n| {
                    starts
                        .last()
                        .and_then(|&s| n.checked_sub(s))
                        .and_then(|k| usize::try_from(k).ok())
                        .and_then(|k| tone.get(k))
                        .copied()
                        .unwrap_or(0.0)
                })
                .collect();
            if let Err(e) = player.push(RATE, &[&block]) {
                fail = Some(e);
                break;
            }
            if starts_click {
                times.push(start.elapsed().as_secs_f64());
            }
            pushed = end;
        }
        if fail.is_some() {
            break;
        }
        if input.is_none() {
            match hub.stream_status(&input_key) {
                Some(st) if st.open && st.rate > 0 && st.callbacks > 2 => {
                    // A first read places the cursor at the newest sample.
                    let _ = hub.read(&input_key, &mut cursor, 0);
                    recorded = vec![Vec::new(); usize::from(st.channels).max(1)];
                    input = Some(st);
                }
                st if now > 6.0 => {
                    let why = st.and_then(|s| s.error).map(|e| format!(": {e}")).unwrap_or_default();
                    fail = Some(format!("the input did not open within six seconds{why}"));
                    break;
                }
                _ => {}
            }
        }
        let in_rate = input.as_ref().map_or(0, |s| s.rate);
        if input.is_some()
            && let Some(block) = hub.read(&input_key, &mut cursor, usize::MAX)
        {
            let t = start.elapsed().as_secs_f64();
            for (r, c) in recorded.iter_mut().zip(block.channels) {
                r.extend(c);
            }
            let frames = recorded.first().map_or(0, Vec::len);
            // `as`: frame counts.
            origin = origin.min(t - frames as f64 / f64::from(in_rate));
        }
        status = player.status();
        if playing_since.is_none() && status.open && status.engine.state == State::Playing {
            playing_since = Some(now);
        }
        match playing_since {
            Some(since) if input.is_some() && times.len() < clicks && next_click.is_none() => {
                let wanted = since + 0.5 + SPACING_SECONDS * times.len() as f64;
                if now >= wanted {
                    next_click = Some(pushed.max(due));
                }
            }
            None if now > 8.0 => {
                fail = Some(format!(
                    "the output did not start within eight seconds{}",
                    status.error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
                ));
                break;
            }
            _ => {}
        }
        if times.len() >= clicks && times.last().is_some_and(|t| now > t + 0.6) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    player.pause();
    drop(player);
    // The input as it stands at the end: its reported delay has settled by now.
    if let Some(st) = hub.stream_status(&input_key)
        && input.is_some()
    {
        input = Some(st);
    }
    hub.shutdown();
    if let Some(e) = fail {
        return Err(e);
    }
    let Some(input) = input else { return Err("the input did not open".into()) };
    let in_rate = input.rate;
    let in_channels = usize::from(input.channels).max(1);
    // The input's delay as reported while it ran.
    let input_ms = input.device_ms;
    let template = click(in_rate, 1.0);
    let mut report = Report {
        output_device: status.device.clone(),
        input_device: input.device.clone(),
        output_rate: status.rate,
        input_rate: in_rate,
        input_channels: input.channels,
        clicks,
        level_db: level,
        input_device_ms: input_ms,
        player_fill_ms: status.engine.fill_ms,
        player_device_ms: status.device_ms,
        player_latency_ms: status.latency_ms,
        player_callback_frames: status.callback_frames,
        player_buffer_frames: status.buffer_frames,
        underruns: status.engine.underruns,
        ..Report::default()
    };
    let wanted: Vec<usize> =
        if spec.channels.is_empty() { (1..=in_channels).collect() } else { spec.channels.clone() };
    for ch in wanted {
        let Some(samples) = ch.checked_sub(1).and_then(|i| recorded.get(i)) else {
            continue;
        };
        report.channels.push(find_clicks(ch, samples, &template, &times, origin, in_rate));
    }
    let best = report
        .channels
        .iter()
        .filter(|c| c.found > 0)
        .max_by(|a, b| a.found.cmp(&b.found).then(a.clarity.total_cmp(&b.clarity)));
    if let Some(b) = best {
        report.best_channel = Some(b.channel);
        report.round_trip_ms = b.round_trip_ms;
        report.output_ms = b.round_trip_ms.map(|r| r - input_ms);
    }
    report.note = if report.best_channel.is_some() {
        "round_trip_ms: from the push of the block starting with a click to the input handing its first sample \
         to the program; output_ms: that minus the input's own delay as the system reports it"
            .into()
    } else {
        "no clicks found on any channel: is the output routed back to this input (an interface's loopback, a \
         cable, a microphone that is switched on)?"
            .into()
    };
    Ok(report)
}

fn find_clicks(
    ch: usize,
    x: &[f32],
    template: &[f64],
    times: &[f64],
    origin: f64,
    rate: u32,
) -> ChannelResult {
    let r = f64::from(rate);
    let energy: f64 = template.iter().map(|v| v * v).sum::<f64>().max(1e-12);
    let corr_at = |k: usize| -> f64 {
        let Some(w) = x.get(k..k + template.len()) else { return 0.0 };
        w.iter().zip(template).map(|(a, b)| f64::from(*a) * b).sum::<f64>() / energy
    };
    let to_frame = |t: f64| -> usize { ((t - origin) * r).max(0.0) as usize };
    // Noise: the strongest correlation in the quarter second before the first click.
    let noise = times.first().map_or(0.0, |&t0| {
        let (a, b) = (to_frame(t0 - SPACING_SECONDS), to_frame(t0));
        (a..b).step_by(1).map(|k| corr_at(k).abs()).fold(0.0, f64::max)
    });
    let mut lags = Vec::new();
    let mut best = 0.0f64;
    let mut peak = 0.0f32;
    for &t in times {
        let (a, b) = (to_frame(t), to_frame(t + SEARCH_SECONDS));
        let mut top = (0.0f64, a);
        for k in a..b {
            let c = corr_at(k).abs();
            if c > top.0 {
                top = (c, k);
            }
        }
        best = best.max(top.0);
        if top.0 > 6.0 * noise.max(1e-6) && top.0 > 1e-4 {
            let arrival = origin + top.1 as f64 / r;
            lags.push((arrival - t) * 1000.0);
            if let Some(w) = x.get(top.1..top.1 + template.len()) {
                peak = w.iter().fold(peak, |m, v| m.max(v.abs()));
            }
        }
    }
    let each = lags.clone();
    lags.sort_by(f64::total_cmp);
    let median = if lags.is_empty() { None } else { lags.get(lags.len() / 2).copied() };
    let spread = match (lags.first(), lags.last()) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    };
    ChannelResult {
        channel: ch,
        found: lags.len(),
        round_trip_ms: median,
        spread_ms: spread,
        peak_db: 20.0 * f64::from(peak.max(1e-9)).log10(),
        clarity: best / noise.max(1e-6),
        each_ms: each,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_click_is_short_and_quiet() {
        let c = click(48_000, db_to_gain(MAX_LEVEL_DB));
        assert_eq!(c.len(), 240);
        let peak = c.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(peak <= db_to_gain(MAX_LEVEL_DB) + 1e-12);
    }

    #[test]
    fn clicks_are_found_where_they_were_put() {
        let rate = 96_000;
        let template = click(rate, 1.0);
        let mut x = vec![0.0f32; rate as usize * 4];
        // Clicks pushed at 1.0 s and 1.6 s arrive 37.5 ms later; the input started at 0.2 s.
        let origin = 0.2;
        let times = [1.0, 1.6];
        for t in times {
            let at = ((t + 0.0375 - origin) * f64::from(rate)) as usize;
            for (k, v) in click(rate, 0.05).iter().enumerate() {
                x[at + k] += *v as f32;
            }
        }
        let r = find_clicks(3, &x, &template, &times, origin, rate);
        assert_eq!(r.found, 2);
        assert!((r.round_trip_ms.unwrap() - 37.5).abs() < 0.05, "{r:?}");
        let silent = find_clicks(1, &vec![0.0; x.len()], &template, &times, origin, rate);
        assert_eq!(silent.found, 0);
    }

    /// Plays quiet clicks (-30 dBFS) on the default output and looks for them on the input
    /// named by `PN_MEASURE_INPUT` (default `Focusrite`).
    #[test]
    #[ignore = "plays quiet clicks on this machine's default output"]
    fn measures_the_loopback() {
        for d in crate::playback::output_devices().unwrap() {
            println!("output: {d:?}");
        }
        for d in crate::capture::devices().unwrap() {
            println!("input: {d:?}");
        }
        let input = std::env::var("PN_MEASURE_INPUT").unwrap_or_else(|_| "Focusrite".into());
        let output = std::env::var("PN_MEASURE_OUTPUT").unwrap_or_default();
        let latency: f64 =
            std::env::var("PN_MEASURE_LATENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(50.0);
        let buffer: f64 = std::env::var("PN_MEASURE_BUFFER").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let spec = Spec {
            output,
            input,
            settings: Settings { latency_ms: latency, device_buffer_ms: buffer },
            ..Spec::default()
        };
        let r = run(&spec).unwrap();
        println!("{r:#?}");
    }
}
