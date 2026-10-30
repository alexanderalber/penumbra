//! A diagnosis of the level normalisation on real music, scaled down to a quiet microphone's
//! level, with one loud click in it. Ignored: it reads WAV files outside the repository. Run
//! with `cargo test -p penumbra-audio --test analysis_diagnose -- --ignored --nocapture`.
//!
//! For every file it writes a CSV of the raw values, a peak follower like the one the analysis
//! used before percentile ranging, and the normalised outputs, per cook at 60 cooks per second,
//! and prints a summary per stretch of time.

// A measuring tool that only runs by hand: a WAV it cannot parse may fail it.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::io::Write;
use std::path::Path;

use penumbra_audio::analysis::{Analyzer, Settings};

const DIR: &str = "E:/misc/temp";
const OUT: &str = "E:/misc/temp/s82/row82";

/// Mono samples and the rate of a 16-bit PCM WAV.
fn read_wav(path: &Path) -> Option<(Vec<f32>, f64)> {
    let bytes = std::fs::read(path).ok()?;
    let u16_at = |i: usize| bytes.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut i = 12;
    let (mut channels, mut rate, mut bits, mut format) = (0usize, 0u32, 0u16, 0u16);
    while i + 8 <= bytes.len() {
        let id = bytes.get(i..i + 4)?;
        let len = u32_at(i + 4)? as usize;
        let body = i + 8;
        if id == b"fmt " {
            format = u16_at(body)?;
            channels = usize::from(u16_at(body + 2)?);
            rate = u32_at(body + 4)?;
            bits = u16_at(body + 14)?;
        } else if id == b"data" {
            let width = match (format, bits) {
                (1, 16) => 2,
                (3, 32) => 4,
                _ => return None,
            };
            if channels == 0 {
                return None;
            }
            let data = bytes.get(body..(body + len).min(bytes.len()))?;
            let frames = data.len() / (width * channels);
            let mono = (0..frames)
                .map(|f| {
                    (0..channels)
                        .map(|c| {
                            let k = width * (f * channels + c);
                            if width == 2 {
                                f32::from(i16::from_le_bytes([data[k], data[k + 1]])) / 32768.0
                            } else {
                                f32::from_le_bytes([data[k], data[k + 1], data[k + 2], data[k + 3]])
                            }
                        })
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            return Some((mono, f64::from(rate)));
        }
        i = body + len + (len & 1);
    }
    None
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// The old normaliser: the loudest so far, falling 1.5 dB per second.
struct Follower(f64);

impl Follower {
    fn update(&mut self, x: f64, seconds: f64) -> f64 {
        self.0 = (self.0 * 10f64.powf(-1.5 * seconds / 20.0)).max(x);
        self.0
    }
}

#[test]
#[ignore = "reads WAV files outside the repository"]
fn diagnose_quiet_music_with_a_click() {
    let files = ["keygen/halloween-1.wav", "keygen/keygen-1.wav", "keygen/keygen-2.wav", "pend/a.wav"];
    let _ = std::fs::create_dir_all(OUT);
    let target_db = -45.0;
    let click_at = 10.0;
    for name in files {
        let path = Path::new(DIR).join(name);
        let Some((mut x, rate)) = read_wav(&path) else {
            println!("{name}: not read");
            continue;
        };
        x.truncate((rate * 30.0) as usize);
        let rms = (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        let gain = 10f64.powf(target_db / 20.0) / rms.max(1e-9);
        for v in &mut x {
            *v = (f64::from(*v) * gain) as f32;
        }
        // A knock on the microphone: 5 ms of a decaying burst at half full scale.
        let at = (click_at * rate) as usize;
        let mut seed = 1u32;
        for i in 0..(0.005 * rate) as usize {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let r = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            if let Some(s) = x.get_mut(at + i) {
                *s += 0.5 * r * (-(i as f32) / (0.001 * rate as f32)).exp();
            }
        }
        // Stretches: before the click, its first 4 s, the 9 s after, the rest.
        let spans = [
            (3.0, click_at),
            (click_at + 0.5, click_at + 4.0),
            (click_at + 4.0, click_at + 13.0),
            (click_at + 13.0, 30.0),
        ];
        report(name, &x, rate, &spans, &format!("scaled to {target_db} dBFS RMS, click at {click_at} s"));
    }
}

/// The room as [`capture_the_room`] recorded it, unscaled.
#[test]
#[ignore = "reads WAV files outside the repository"]
fn diagnose_the_room() {
    for name in ["room.wav", "room-ch2.wav"] {
        let Some((x, rate)) = read_wav(&Path::new(OUT).join(name)) else {
            println!("{name}: not read");
            continue;
        };
        report(name, &x, rate, &[(3.0, 10.0), (10.0, 20.0)], "as recorded");
    }
}

/// Runs the analysis over `x` in cooks of 1/60 s, writes a CSV and prints a summary per span.
fn report(name: &str, x: &[f32], rate: f64, spans: &[(f64, f64)], what: &str) {
    let _ = std::fs::create_dir_all(OUT);
    {
        let raw_settings = Settings { normalize: false, ..Settings::default() };
        let norm_settings = Settings::default();
        let mut raw = Analyzer::new(rate).unwrap();
        let mut norm = Analyzer::new(rate).unwrap();
        let block = (rate / 60.0).round() as usize;
        let mut followers = [Follower(0.0), Follower(0.0), Follower(0.0), Follower(0.0)];
        let stem = name.replace(['/', '.'], "_");
        let mut csv = std::fs::File::create(Path::new(OUT).join(format!("{stem}.csv"))).unwrap();
        writeln!(
            csv,
            "t,rms_db,low_db,mid_db,high_db,f_level_db,f_low_db,f_mid_db,f_high_db,level,low,mid,high,spectrum_mean"
        )
        .unwrap();
        let mut stats: Vec<[Vec<f64>; 9]> = spans.iter().map(|_| Default::default()).collect();
        for (k, chunk) in x.chunks(block).enumerate() {
            let t = (k + 1) as f64 * block as f64 / rate;
            let r = raw.process(chunk, &raw_settings);
            let n = norm.process(chunk, &norm_settings);
            let rv = [r.rms, r.low, r.mid, r.high];
            let fv: Vec<f64> = rv
                .iter()
                .zip(followers.iter_mut())
                .map(|(v, f)| f.update(*v, chunk.len() as f64 / rate))
                .collect();
            let sm = n.spectrum.iter().sum::<f64>() / n.spectrum.len().max(1) as f64;
            writeln!(
                csv,
                "{t:.4},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.4},{:.4},{:.4},{:.4},{sm:.4}",
                db(rv[0]),
                db(rv[1]),
                db(rv[2]),
                db(rv[3]),
                db(fv[0]),
                db(fv[1]),
                db(fv[2]),
                db(fv[3]),
                n.level,
                n.low,
                n.mid,
                n.high
            )
            .unwrap();
            for (s, &(a, b)) in stats.iter_mut().zip(spans) {
                if t >= a && t < b {
                    for (j, v) in
                        [db(rv[0]), db(rv[1]), db(rv[2]), db(rv[3]), n.level, n.low, n.mid, n.high, sm]
                            .into_iter()
                            .enumerate()
                    {
                        s[j].push(v);
                    }
                }
            }
        }
        println!("\n{name} at {rate} Hz, {what}");
        println!(
            "  span          | raw dBFS median: rms low mid high | out P10/P50/P90: level low mid high | spectrum mean"
        );
        for (s, &(a, b)) in stats.iter_mut().zip(spans) {
            let mut line = format!("  {a:>4.1}-{b:>4.1} s | ");
            for v in s.iter_mut().take(4) {
                line += &format!("{:6.1} ", percentile(v, 0.5));
            }
            line += "| ";
            for v in s.iter_mut().skip(4).take(4) {
                line += &format!(
                    "{:.2}/{:.2}/{:.2}  ",
                    percentile(v, 0.1),
                    percentile(v, 0.5),
                    percentile(v, 0.9)
                );
            }
            line += &format!("| {:.2}", percentile(&mut s[8], 0.5));
            println!("{line}");
        }
    }
}

/// Writes 32-bit float mono.
fn write_wav(path: &Path, rate: u32, x: &[f32]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data = (x.len() * 4) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data.to_le_bytes())?;
    for v in x {
        f.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

/// Records 20 s from the Focusrite into `room.wav` (channel 1), one attempt: when nothing
/// arrives within five seconds it gives up.
#[test]
#[ignore = "needs the audio interface"]
fn capture_the_room() {
    use penumbra_audio::{Cursor, Hub};
    let _ = std::fs::create_dir_all(OUT);
    let hub = Hub::new();
    let key = "Focusrite".to_string();
    hub.set_wanted(std::slice::from_ref(&key));
    let mut cursor = Cursor::default();
    let mut x = Vec::new();
    let mut x2 = Vec::new();
    let mut rate = 0;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 25.0 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(b) = hub.read(&key, &mut cursor, 1 << 20) {
            rate = b.rate;
            if let Some(c) = b.channels.first() {
                x.extend_from_slice(c);
            }
            if let Some(c) = b.channels.get(1) {
                x2.extend_from_slice(c);
            }
        }
        if x.is_empty() && start.elapsed().as_secs_f64() > 5.0 {
            println!("nothing arrived in 5 s: {:?}", hub.stream_status(&key));
            return;
        }
        if rate > 0 && x.len() >= rate as usize * 20 {
            break;
        }
    }
    println!("{:?}", hub.stream_status(&key));
    for (name, x) in [("room.wav", &x), ("room-ch2.wav", &x2)] {
        let rms = (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt();
        println!("{name}: {} samples at {rate} Hz, {:.1} dBFS RMS", x.len(), db(rms));
        write_wav(&Path::new(OUT).join(name), rate, x).unwrap();
    }
}

/// Time per cook of the analysis: 30 s of music at 48 and 96 kHz in cooks of 1/60 s.
#[test]
#[ignore = "a measurement, reads a WAV file outside the repository"]
fn cost_per_cook() {
    let Some((x, _)) = read_wav(&Path::new(DIR).join("pend/a.wav")) else {
        println!("not read");
        return;
    };
    let x: Vec<f32> = x.iter().map(|v| v * 0.02).collect();
    for rate in [48_000.0, 96_000.0] {
        let mut a = Analyzer::new(rate).unwrap();
        let s = Settings::default();
        let block = (rate / 60.0) as usize;
        let start = std::time::Instant::now();
        let mut cooks = 0;
        let mut sink = 0.0;
        for _ in 0..2 {
            for chunk in x.chunks(block) {
                sink += a.process(chunk, &s).level;
                cooks += 1;
            }
        }
        let us = start.elapsed().as_secs_f64() * 1e6 / f64::from(cooks);
        println!("{rate} Hz: {us:.1} us per cook over {cooks} cooks ({sink:.1})");
    }
}

/// Material with and without a beat: `(label, samples, rate)`. Real recordings are cut to
/// 40 s; the ones that are missing are left out.
fn beat_material() -> Vec<(String, Vec<f32>, f64)> {
    let mut out: Vec<(String, Vec<f32>, f64)> = Vec::new();
    let r = 48_000.0;
    out.push(("white noise".into(), common::white(r, 30.0, 0.1, 3), r));
    out.push(("pink noise".into(), common::pink(r, 30.0, 0.3, 4), r));
    out.push(("speech-like".into(), common::speech(r, 30.0, 5), r));
    out.push(("speech-like 2".into(), common::speech(r, 30.0, 17), r));
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        out.push((format!("house 124 @{rate}"), common::house(rate, 20.0, |_| 124.0, 0.3).0, rate));
    }
    let (mut h, _) = common::house(r, 12.0, |_| 124.0, 0.3);
    h.extend(std::iter::repeat_n(0.0, (r * 12.0) as usize));
    out.push(("house 12 s, then zeros".into(), h, r));
    let (mut h, _) = common::house(r, 12.0, |_| 124.0, 0.3);
    h.extend(common::white(r, 12.0, 0.0003, 9));
    out.push(("house 12 s, then -76 dB hiss".into(), h, r));
    let (mut h, _) = common::house(r, 12.0, |_| 124.0, 0.3);
    h.extend(common::speech(r, 15.0, 21));
    out.push(("house 12 s, then speech".into(), h, r));
    out.push((
        "house 120 then 128 at 12 s".into(),
        common::house(r, 30.0, |t| if t < 12.0 { 120.0 } else { 128.0 }, 0.1).0,
        r,
    ));
    let files = [
        "keygen/halloween-1.wav",
        "keygen/halloween-2.wav",
        "keygen/keygen-1.wav",
        "keygen/keygen-2.wav",
        "pend/a.wav",
        "pend/st.wav",
        "synthwave-prof/stem-ALL.wav",
        "synthwave-prof/stem-pad.wav",
        "s99p3/ambience_rain.wav",
        "s99p3/ambience_water.wav",
        "asfasdf/483684__danielmcadams__zoom-h4n-super-close-lightning-strike-and-thunderstorm.wav",
        "s82/row82/room-ch2.wav",
        "s82/row82/room.wav",
    ];
    for name in files {
        let Some((mut x, rate)) = read_wav(&Path::new(DIR).join(name)) else {
            println!("{name}: not read");
            continue;
        };
        x.truncate((rate * 40.0) as usize);
        let short = name.rsplit('/').next().unwrap_or(name).chars().take(28).collect::<String>();
        if name == "keygen/keygen-1.wav" {
            let mut y = x.clone();
            y.truncate((rate * 20.0) as usize);
            y.extend(std::iter::repeat_n(0.0, (rate * 12.0) as usize));
            out.push(("keygen-1 20 s, then zeros".into(), y, rate));
        }
        out.push((format!("{short} ({:.0} s @{rate})", x.len() as f64 / rate), x, rate));
    }
    out
}

/// Confidence, lock and ticks over time on [`beat_material`], in cooks of 1/60 s.
#[test]
#[ignore = "a measurement, reads WAV files outside the repository"]
fn diagnose_beat() {
    let s = Settings::default();
    println!(
        "material                         | conf P10/P50/P90 after 6 s | locked share, first lock | ticks/s | bpm end | conf per 2 s"
    );
    let only = std::env::var("PN_ONLY").ok();
    for (label, x, rate) in beat_material() {
        if only.as_ref().is_some_and(|o| !label.contains(o.as_str())) {
            continue;
        }
        let mut a = Analyzer::new(rate).unwrap();
        let block = (rate / 60.0).round() as usize;
        let mut conf = Vec::new();
        let mut timeline: Vec<Vec<f64>> = Vec::new();
        let mut kicks: Vec<usize> = Vec::new();
        let mut locks: Vec<bool> = Vec::new();
        let mut first_lock = None;
        let mut ticks = 0usize;
        let mut late = 0.0;
        let mut bpm = 0.0;
        let mut lock_line = String::new();
        let mut last_lock = None;
        for (k, chunk) in x.chunks(block).enumerate() {
            let t = (k + 1) as f64 * block as f64 / rate;
            let f = a.process(chunk, &s);
            let locked = lock_of(&f);
            if locked && first_lock.is_none() {
                first_lock = Some(t);
            }
            if last_lock != Some(locked) {
                lock_line += &format!(" {}@{t:.1}", if locked { "L" } else { "U" });
                last_lock = Some(locked);
            }
            let bin = (t / 2.0) as usize;
            if timeline.len() <= bin {
                timeline.resize(bin + 1, Vec::new());
            }
            timeline[bin].push(f.confidence);
            if kicks.len() <= bin {
                kicks.resize(bin + 1, 0);
            }
            kicks[bin] += usize::from(f.kick);
            if t > 6.0 {
                conf.push(f.confidence);
                locks.push(locked);
                ticks += usize::from(f.tick);
                late += chunk.len() as f64 / rate;
            }
            bpm = f.bpm;
        }
        let share = locks.iter().filter(|l| **l).count() as f64 / locks.len().max(1) as f64;
        let line: Vec<String> = timeline.iter_mut().map(|v| format!("{:.2}", percentile(v, 0.5))).collect();
        println!(
            "{label:<32} | {:.2}/{:.2}/{:.2} | {share:.2}, {} | {:.2} | {bpm:.1} | {}",
            percentile(&mut conf.clone(), 0.1),
            percentile(&mut conf.clone(), 0.5),
            percentile(&mut conf, 0.9),
            first_lock.map_or("never".to_string(), |t| format!("{t:.1} s")),
            ticks as f64 / late.max(1e-9),
            line.join(" ")
        );
        println!("{:<32}   lock changes:{lock_line}", "");
        let kicks: Vec<String> = kicks.iter().map(|k| k.to_string()).collect();
        println!("{:<32}   kicks per 2 s: {}", "", kicks.join(" "));
    }
}

/// Whether the analysis says it has a beat.
fn lock_of(f: &penumbra_audio::Features) -> bool {
    f.locked
}
