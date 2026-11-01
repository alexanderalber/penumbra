//! The beat tracker on a sound file: decode, spectrogram, the network's chunks in parallel,
//! peaks, and a cache of the result.
//!
//! Decoding runs on the calling thread. A chunk goes to rayon's pool as soon as its frames
//! are decoded, so the network works while the rest of the file is still being read. At most
//! [`IN_FLIGHT`] chunks run at once; each one also spreads its own work over the pool. A panic
//! inside the network counts as a failed chunk.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use penumbra_nn::Tensor;
use penumbra_nn::models::BeatThis;

use super::cache;
use super::{
    BORDER_FRAMES, BeatError, Beats, CHUNK_FRAMES, ChunkPlan, FPS, Frontend, Logits, N_MELS, NO_BEAT,
    Partial, Spectrogram, cancelled, pick, pick_final,
};
use crate::file;

/// The network file penumbra ships: Beat This! `final0`, its weights quantized (int6 in the
/// RoFormer, int8 elsewhere).
pub const MODEL_FILE: &str = "beat_this-final0-q6.safetensors";
/// Chunks run at once. One chunk of the full network needs about 100 MB while it runs, and
/// more chunks at once gain little: each one already keeps the pool busy.
pub const IN_FLIGHT: usize = 3;
/// How many directories above the executable's own the model is looked for: the release zip
/// has `assets` beside the executable, a checkout three levels up (`target/debug/deps` for
/// tests).
const MODEL_SEARCH_DEPTH: usize = 4;
/// The largest network file read.
const MAX_MODEL_BYTES: u64 = 512 << 20;
/// Share of the progress bar for decoding; the network takes the rest.
const DECODE_SHARE: f64 = 0.05;
/// The longest sound tracked. Its spectrogram stays in memory, about 92 MB per hour.
pub const MAX_SECONDS: f64 = 6.0 * 3600.0;

/// About how many chunks a sound of `seconds` gives, without building the plan.
fn chunks_for(seconds: f64) -> usize {
    let step = (CHUNK_FRAMES - 2 * BORDER_FRAMES) as f64;
    let frames = (seconds.clamp(0.0, MAX_SECONDS) * FPS).ceil();
    // `as`: at most a few thousand.
    (frames / step).ceil().max(1.0) as usize
}

/// The shipped network file: `assets/models/` beside the executable or a few levels above it.
pub fn model_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(MODEL_SEARCH_DEPTH + 1)
        .map(|d| d.join("assets").join("models").join(MODEL_FILE))
        .find(|p| p.is_file())
}

/// A loaded Beat This! network and an id for its weights, which the cache keys on.
pub struct BeatModel {
    net: BeatThis,
    id: String,
}

impl BeatModel {
    /// Loads a network file (written by `tools/beat_this_convert.py`, quantized or F32).
    pub fn load(path: &Path) -> Result<BeatModel, BeatError> {
        let fail = |e: String| BeatError::Network(format!("{}: {e}", path.display()));
        let size = std::fs::metadata(path).map_err(|e| fail(e.to_string()))?.len();
        if size > MAX_MODEL_BYTES {
            return Err(fail(format!("{size} bytes, more than a network file can have")));
        }
        let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
        let loaded = catch_unwind(AssertUnwindSafe(|| {
            let st = penumbra_nn::SafeTensors::parse(&bytes)?;
            let net = BeatThis::from_safetensors(&st)?;
            Ok::<_, penumbra_nn::Error>((net, st.metadata().get("model_id").cloned()))
        }));
        let (net, name) = match loaded {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(fail(e.to_string())),
            Err(_) => return Err(fail("loading failed".into())),
        };
        let name = name.unwrap_or_else(|| {
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        });
        let name: String = name
            .chars()
            .take(64)
            .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
            .collect();
        Ok(BeatModel { net, id: format!("{name}-{:016x}", fnv1a(&bytes)) })
    }

    /// Loads the shipped network ([`model_path`]).
    pub fn shipped() -> Result<BeatModel, BeatError> {
        let path = model_path().ok_or_else(|| {
            BeatError::Network(format!(
                "the beat network {MODEL_FILE} is not in assets/models beside penumbra"
            ))
        })?;
        BeatModel::load(&path)
    }

    /// The weights' name and a hash of the file: another file gives another id.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn net(&self) -> &BeatThis {
        &self.net
    }

    /// One chunk through the network: `frames * N_MELS` values in, a beat and a downbeat
    /// logit per frame out. A panic counts as an error.
    pub fn run(&self, input: Vec<f32>, frames: usize) -> Result<(Vec<f32>, Vec<f32>), BeatError> {
        let out = catch_unwind(AssertUnwindSafe(|| {
            let t = Tensor::new(vec![frames, N_MELS], input)?;
            self.net.forward(&t)
        }));
        match out {
            Ok(Ok(l)) => Ok((l.beat, l.downbeat)),
            Ok(Err(e)) => Err(BeatError::Network(e.to_string())),
            Err(_) => Err(BeatError::Network("the network failed on a chunk".into())),
        }
    }
}

/// 64-bit FNV-1a: a stable hash for cache names and ids (not against forgery).
pub(super) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// How far [`analyze_file`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    /// Seconds of sound decoded.
    pub decoded: f64,
    /// The sound's length: the header's word while decoding (`None` when it has none), exact
    /// after.
    pub length: Option<f64>,
    /// Whether decoding has finished.
    pub decode_done: bool,
    /// Chunks through the network.
    pub chunks_done: usize,
    /// Chunks in all: estimated from `length` while decoding, exact after; 0 when unknown.
    pub chunks: usize,
}

impl Progress {
    /// The whole job's share done, 0 to 1.
    pub fn fraction(&self) -> f64 {
        let decode = if self.decode_done {
            1.0
        } else {
            self.length.filter(|l| *l > 0.0).map_or(0.0, |l| (self.decoded / l).clamp(0.0, 1.0))
        };
        let net = if self.chunks == 0 {
            0.0
        } else {
            (self.chunks_done as f64 / self.chunks as f64).clamp(0.0, 1.0)
        };
        if self.decode_done && self.chunks == 0 {
            return 1.0;
        }
        DECODE_SHARE * decode + (1.0 - DECODE_SHARE) * net
    }
}

/// Beat and downbeat times of a sound file. With `cache_dir`, a result for this file (its size
/// and modification time) and this model is read from there instead, and a new result is
/// written there; a cache entry that cannot be read is computed again. `cancel` is checked
/// for every decoded block and between chunks. `progress` hears how far the job is, from the
/// calling thread. Call it from a thread of its own: it blocks until the job is done.
pub fn analyze_file(
    path: &Path,
    model: &BeatModel,
    cache_dir: Option<&Path>,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Beats, BeatError> {
    analyze_file_with(path, model, cache_dir, cancel, progress, &mut |_| {})
}

/// [`analyze_file`], and `partial` hears the beats that are final so far ([`pick_final`]) each
/// time the run of chunks done from the start of the file grows. A result from the cache
/// gives no partial results.
pub fn analyze_file_with(
    path: &Path,
    model: &BeatModel,
    cache_dir: Option<&Path>,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&Progress),
    partial: &mut dyn FnMut(&Partial),
) -> Result<Beats, BeatError> {
    let key = cache_dir.and_then(|dir| cache::Key::of(path, model.id(), dir));
    if let Some(key) = &key
        && let Some(beats) = cache::load(key)
    {
        progress(&Progress { decode_done: true, ..Progress::default() });
        return Ok(beats);
    }
    let beats = compute(path, model, cancel, progress, partial)?;
    if let Some(key) = &key
        && let Err(e) = cache::store(key, &beats)
    {
        log::warn!("beat cache: {e}");
    }
    Ok(beats)
}

/// The network's logits for a whole spectrogram, its chunks in parallel. `progress` hears
/// `(chunks done, chunks in all)`.
pub fn run_parallel(
    spect: &Spectrogram,
    model: &BeatModel,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Logits, BeatError> {
    rayon::in_place_scope(|scope| {
        let mut jobs = Jobs::new(scope, model);
        jobs.finish(spect, cancel, progress, &mut |_| {})
    })
}

/// The logits that are final while chunks are still running: those of the first chunks in a
/// row, each one's frames without its borders. Only chunks of [`CHUNK_FRAMES`] rows at the
/// regular places (`k * step - BORDER_FRAMES`) count. Every plan of a longer piece has them,
/// and where the moved last chunk overlaps them, they win, as in [`super::Stitcher::finish`].
#[derive(Default)]
struct Prefix {
    beat: Vec<f32>,
    downbeat: Vec<f32>,
    /// The next chunk to add.
    next: usize,
}

impl Prefix {
    fn key(k: usize) -> (isize, usize) {
        // `as`: frame offsets are far below isize::MAX.
        ((k * (CHUNK_FRAMES - 2 * BORDER_FRAMES)) as isize - BORDER_FRAMES as isize, CHUNK_FRAMES)
    }

    /// Adds the chunks done next in a row. With the final `plan`, only its chunks before the
    /// last count. Whether it grew.
    fn grow(&mut self, done: &Outputs, plan: Option<&ChunkPlan>) -> bool {
        let before = self.next;
        loop {
            let key = Self::key(self.next);
            let in_plan = plan.is_none_or(|p| {
                self.next + 1 < p.len()
                    && p.chunks().get(self.next).is_some_and(|c| (c.start, c.frames) == key)
            });
            let Some((b, d)) = done.get(&key).filter(|_| in_plan) else { break };
            let keep = BORDER_FRAMES..CHUNK_FRAMES - BORDER_FRAMES;
            let clean = |v: &f32| if v.is_nan() { NO_BEAT } else { *v };
            self.beat.extend(b.get(keep.clone()).unwrap_or(&[]).iter().map(clean));
            self.downbeat.extend(d.get(keep).unwrap_or(&[]).iter().map(clean));
            self.next += 1;
        }
        self.next > before
    }
}

/// The chunks sent to the pool and their outputs, keyed by (first frame, frames).
struct Jobs<'a, 'scope, 'm> {
    scope: &'a rayon::Scope<'scope>,
    model: &'m BeatModel,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    running: BTreeSet<(isize, usize)>,
    done: Outputs,
    prefix: Prefix,
}

/// The chunks' beat and downbeat logits, keyed by (first frame, frames).
type Outputs = BTreeMap<(isize, usize), (Vec<f32>, Vec<f32>)>;

type Done = ((isize, usize), Result<(Vec<f32>, Vec<f32>), BeatError>);

impl<'a, 'scope, 'm: 'scope> Jobs<'a, 'scope, 'm> {
    fn new(scope: &'a rayon::Scope<'scope>, model: &'m BeatModel) -> Self {
        let (tx, rx) = mpsc::channel();
        Jobs {
            scope,
            model,
            tx,
            rx,
            running: BTreeSet::new(),
            done: BTreeMap::new(),
            prefix: Prefix::default(),
        }
    }

    fn spawn(&mut self, key: (isize, usize), input: Vec<f32>) {
        let tx = self.tx.clone();
        let model = self.model;
        self.running.insert(key);
        self.scope.spawn(move |_| {
            let out = model.run(input, key.1);
            // The receiver is gone only when the job already failed or was cancelled.
            let _ = tx.send((key, out));
        });
    }

    fn take(&mut self, (key, out): Done) -> Result<(), BeatError> {
        self.running.remove(&key);
        self.done.insert(key, out?);
        Ok(())
    }

    /// Takes the outputs that have arrived, without waiting.
    fn poll(&mut self) -> Result<(), BeatError> {
        while let Ok(d) = self.rx.try_recv() {
            self.take(d)?;
        }
        Ok(())
    }

    /// The beats final so far, when the chunks done in a row from the start grew.
    fn grown(&mut self, plan: Option<&ChunkPlan>) -> Option<Partial> {
        self.prefix.grow(&self.done, plan).then(|| pick_final(&self.prefix.beat, &self.prefix.downbeat))
    }

    /// Runs the chunks of the finished spectrogram that are not done yet and joins all
    /// outputs. `progress` hears `(chunks done, chunks in all)`, `partial` the beats final so
    /// far whenever they grow.
    fn finish(
        &mut self,
        spect: &Spectrogram,
        cancel: Option<&AtomicBool>,
        progress: &mut dyn FnMut(usize, usize),
        partial: &mut dyn FnMut(&Partial),
    ) -> Result<Logits, BeatError> {
        let plan = ChunkPlan::new(spect.frames());
        let keys: Vec<(isize, usize)> = plan.chunks().iter().map(|c| (c.start, c.frames)).collect();
        let total = keys.len();
        let mut next = 0;
        loop {
            self.poll()?;
            if let Some(p) = self.grown(Some(&plan)) {
                partial(&p);
            }
            let done = keys.iter().filter(|k| self.done.contains_key(k)).count();
            progress(done, total);
            if done == total {
                break;
            }
            if cancelled(cancel) {
                return Err(BeatError::Cancelled);
            }
            while self.running.len() < IN_FLIGHT && next < total {
                let k = next;
                next += 1;
                let Some(key) = keys.get(k).copied() else { break };
                if self.done.contains_key(&key) || self.running.contains(&key) {
                    continue;
                }
                let input = plan.input(spect, k).unwrap_or_default();
                self.spawn(key, input);
            }
            if self.running.is_empty() {
                return Err(BeatError::Network("chunks are missing and none is running".into()));
            }
            match self.rx.recv() {
                Ok(d) => self.take(d)?,
                Err(_) => return Err(BeatError::Network("the network's workers stopped".into())),
            }
        }
        let mut stitcher = plan.stitcher();
        for (k, key) in keys.iter().enumerate() {
            let Some((b, d)) = self.done.remove(key) else {
                return Err(BeatError::Network(format!("chunk {k} has no output")));
            };
            stitcher.put(k, b, d)?;
        }
        stitcher.finish()
    }
}

/// Decodes `path` into the front end, starting the chunks that are complete on the way, then
/// runs the rest and picks the peaks.
fn compute(
    path: &Path,
    model: &BeatModel,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&Progress),
    partial: &mut dyn FnMut(&Partial),
) -> Result<Beats, BeatError> {
    let step = CHUNK_FRAMES - 2 * BORDER_FRAMES;
    rayon::in_place_scope(|scope| {
        let mut jobs = Jobs::new(scope, model);
        let mut frontend: Option<Frontend> = None;
        let mut failure: Option<BeatError> = None;
        let mut state = Progress::default();
        // The next chunk to start early: chunk k starts at frame k * step - BORDER_FRAMES.
        // Once the frames up to its end are decoded, the piece is longer than that, so the
        // chunk is not the piece's last one and its place and input are final.
        let mut early = 0usize;
        let mut last_report = -1.0f64;
        let mut sink = |info: &file::Info, block: &[f32]| -> bool {
            if cancelled(cancel) {
                failure = Some(BeatError::Cancelled);
                return false;
            }
            let fe = match &mut frontend {
                Some(fe) => fe,
                None => match Frontend::new(f64::from(info.rate), info.channels) {
                    Ok(fe) => {
                        state.length = info.seconds();
                        state.chunks = state.length.map_or(0, chunks_for);
                        frontend.insert(fe)
                    }
                    Err(e) => {
                        failure = Some(e);
                        return false;
                    }
                },
            };
            if fe.seconds_in() > MAX_SECONDS {
                failure = Some(BeatError::Input(format!(
                    "beats are tracked in sounds of up to {} hours, and this one is longer",
                    MAX_SECONDS / 3600.0
                )));
                return false;
            }
            fe.push_interleaved(block);
            if let Err(e) = jobs.poll() {
                failure = Some(e);
                return false;
            }
            if let Some(p) = jobs.grown(None) {
                partial(&p);
            }
            while jobs.running.len() < IN_FLIGHT {
                let end = early * step + CHUNK_FRAMES - BORDER_FRAMES;
                if fe.frames() < end {
                    break;
                }
                // `as`: frame offsets are far below isize::MAX.
                let start = (early * step) as isize - BORDER_FRAMES as isize;
                let input = early_input(fe.rows(), start);
                jobs.spawn((start, CHUNK_FRAMES), input);
                early += 1;
            }
            state.decoded = fe.seconds_in();
            state.chunks_done = jobs.done.len();
            state.chunks = state.chunks.max(early);
            if state.decoded - last_report >= 1.0 {
                last_report = state.decoded;
                progress(&state);
            }
            true
        };
        let decoded = file::decode_each(path, &mut sink);
        if let Some(e) = failure {
            return Err(e);
        }
        decoded.map_err(BeatError::Input)?;
        let Some(fe) = frontend else {
            return Err(BeatError::Input("the file holds no sound".into()));
        };
        let spect = fe.finish();
        state.decode_done = true;
        state.length = Some(spect.seconds());
        let logits = jobs.finish(
            &spect,
            cancel,
            &mut |done, all| {
                state.chunks_done = done;
                state.chunks = all;
                progress(&state);
            },
            partial,
        )?;
        Ok(pick(&logits.beat, &logits.downbeat))
    })
}

/// A full chunk's input from the rows decoded so far: [`CHUNK_FRAMES`] rows from `start`,
/// zero before frame 0. The same values [`ChunkPlan::input`] gives from the whole spectrogram.
fn early_input(rows: &[f32], start: isize) -> Vec<f32> {
    let mut buf = vec![0.0f32; CHUNK_FRAMES * N_MELS];
    for (i, dst) in buf.as_chunks_mut::<N_MELS>().0.iter_mut().enumerate() {
        // `as`: row offsets are far below isize::MAX.
        let Ok(f) = usize::try_from(start + i as isize) else { continue };
        if let Some(src) = rows.get(f * N_MELS..(f + 1) * N_MELS) {
            dst.copy_from_slice(src);
        }
    }
    buf
}

/// The F-measure of beat times against a reference, as `mir_eval.beat.f_measure` without
/// trimming: an estimate `e` matches a reference beat `r` with `e - window <= r <= e + window`
/// (each beat matches at most once), precision and recall from the matches, their harmonic
/// mean. 0 when either list is empty. Both lists ascending. The window is computed as
/// mir_eval does: for times on the 20 ms frame grid, a difference of one frame falls inside
/// a 20 ms window or outside it by rounding, and the two ways differ by 0.2 % on a 2 h mix.
pub fn f_measure(reference: &[f64], estimate: &[f64], window: f64) -> f64 {
    if reference.is_empty() || estimate.is_empty() {
        return 0.0;
    }
    // On a line, matching greedily from the left finds the largest matching.
    let (mut i, mut j, mut matched) = (0, 0, 0usize);
    while let (Some(r), Some(e)) = (reference.get(i), estimate.get(j)) {
        if *r >= e - window && *r <= e + window {
            matched += 1;
            i += 1;
            j += 1;
        } else if r < e {
            i += 1;
        } else {
            j += 1;
        }
    }
    if matched == 0 {
        return 0.0;
    }
    let p = matched as f64 / estimate.len() as f64;
    let r = matched as f64 / reference.len() as f64;
    2.0 * p * r / (p + r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_measure_counts_matches_within_the_window() {
        let r = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(f_measure(&r, &r, 0.02), 1.0);
        assert_eq!(f_measure(&r, &[], 0.02), 0.0);
        // Three of four found, one extra: p = 3/4, r = 3/4.
        let e = [1.01, 2.0, 2.5, 4.019];
        assert!((f_measure(&r, &e, 0.02) - 0.75).abs() < 1e-12);
        // One frame apart at 50 frames per second is inside a 20 ms window.
        assert_eq!(f_measure(&[0.64], &[0.62], 0.02), 1.0);
        assert_eq!(f_measure(&[0.62], &[0.64], 0.02), 1.0);
        // Two estimates near one reference beat match once.
        let e = [0.99, 1.01];
        let f = f_measure(&r, &e, 0.02);
        assert!((f - 2.0 * 0.5 * 0.25 / 0.75).abs() < 1e-12);
    }

    #[test]
    fn early_chunks_match_the_plan() {
        // Every early chunk is a chunk of the final plan, with the same input.
        let step = CHUNK_FRAMES - 2 * BORDER_FRAMES;
        for frames in [1494, 1500, 2981, 2982, 2988, 5000, 10_000] {
            let rows: Vec<f32> = (0..frames * N_MELS).map(|i| i as f32).collect();
            let spect = Spectrogram::from_rows(rows.clone());
            let plan = ChunkPlan::new(frames);
            let mut k = 0;
            while k * step + CHUNK_FRAMES - BORDER_FRAMES <= frames {
                let start = (k * step) as isize - BORDER_FRAMES as isize;
                let at = plan.chunks().iter().position(|c| (c.start, c.frames) == (start, CHUNK_FRAMES));
                let at = at.unwrap_or_else(|| panic!("{frames} frames: early chunk {k} not in the plan"));
                assert_eq!(plan.input(&spect, at).unwrap(), early_input(&rows, start), "{frames}/{k}");
                k += 1;
            }
        }
    }

    /// 32 s of clicks at 120 beats per minute, every fourth one louder, as a stereo WAV.
    fn clicks(dir: &Path) -> PathBuf {
        let rate = 44_100u32;
        let n = 32 * rate as usize;
        let mut samples = vec![0.0f64; 2 * n];
        for beat in 0..64 {
            let at = beat * rate as usize / 2;
            let gain = if beat % 4 == 0 { 0.9 } else { 0.4 };
            for i in 0..400.min(n - at) {
                let env = (-(i as f64) / 60.0).exp();
                let v = gain * env * (i as f64 * 0.35).sin();
                samples[2 * (at + i)] = v;
                samples[2 * (at + i) + 1] = v;
            }
        }
        let path = dir.join("clicks.wav");
        std::fs::write(&path, crate::file::wav_bytes(rate, 2, crate::file::WavFormat::Float32, &samples))
            .unwrap();
        path
    }

    #[test]
    fn a_file_gives_the_beats_of_the_sequential_chain_and_is_cached() {
        let dir = std::env::temp_dir().join(format!("penumbra-beat-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sound = clicks(&dir);
        let model = BeatModel::shipped().unwrap();
        assert!(model.id().starts_with("beat_this-final0-q6-"), "{}", model.id());

        // The sequential chain on the decoded samples.
        let mut all = Vec::new();
        let mut format = (0, 0);
        crate::file::decode_each(&sound, &mut |info, block| {
            format = (info.rate, info.channels);
            all.extend_from_slice(block);
            true
        })
        .unwrap();
        let net = |x: &[f32], frames: usize| model.run(x.to_vec(), frames);
        let want =
            super::super::track(&all, format.1, f64::from(format.0), net, None, &mut |_, _| {}).unwrap();
        assert!((60..=68).contains(&want.beats.len()), "{} beats", want.beats.len());

        let cache = dir.join("cache");
        let mut last = Progress::default();
        let mut partials: Vec<Partial> = Vec::new();
        let got = analyze_file_with(&sound, &model, Some(&cache), None, &mut |p| last = *p, &mut |p| {
            partials.push(p.clone())
        })
        .unwrap();
        assert_eq!(got, want);
        // The first chunk (29.76 s) was final before the second: its beats begin the result.
        assert_eq!(partials.len(), 1);
        let p = &partials[0];
        assert!(p.beats.beats.len() >= 55, "{} beats", p.beats.beats.len());
        assert_eq!(p.beats.beats[..], want.beats[..p.beats.beats.len()]);
        assert_eq!(p.beats.downbeats[..], want.downbeats[..p.beats.downbeats.len()]);
        // The bars of the last 32 beats known wait for the beats after them.
        assert!(p.final_until.is_some_and(|t| t > 11.0 && t < 14.0), "{:?}", p.final_until);
        assert_eq!(last.fraction(), 1.0);
        assert_eq!((last.chunks_done, last.chunks), (2, 2));
        // The second run reads the cache.
        let start = std::time::Instant::now();
        assert_eq!(analyze_file(&sound, &model, Some(&cache), None, &mut |_| {}).unwrap(), want);
        assert!(start.elapsed().as_millis() < 200, "{:?}", start.elapsed());
        // A corrupt cache entry is computed again (and the cancel flag shows it is not read).
        for entry in std::fs::read_dir(&cache).unwrap() {
            std::fs::write(entry.unwrap().path(), "penumbra-beats 2 garbage").unwrap();
        }
        let stop = AtomicBool::new(true);
        let r = analyze_file(&sound, &model, Some(&cache), Some(&stop), &mut |_| {});
        assert_eq!(r, Err(BeatError::Cancelled));
        // Not a sound file.
        let junk = dir.join("junk.mp3");
        std::fs::write(&junk, vec![0x55u8; 5000]).unwrap();
        assert!(matches!(analyze_file(&junk, &model, None, None, &mut |_| {}), Err(BeatError::Input(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_example_track_keeps_its_bars() {
        // examples/beat-grid/music.wav: 32 bars of 4/4, a beat every 10 670 samples at 22 050 Hz,
        // the first on 0 s, a breakdown in bars 17 to 24. The bars voted from the logits are the
        // marked downbeats, each on its bar's first beat.
        let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
        let model = BeatModel::shipped().unwrap();
        let got = analyze_file(&music, &model, None, None, &mut |_| {}).unwrap();
        let bars = super::super::BeatGrid::from_beats(&got).downbeats();
        assert_eq!(bars, got.downbeats);
        assert!((32..=33).contains(&bars.len()), "{} bars", bars.len());
        for (k, t) in bars.iter().enumerate() {
            let want = (k * 4 * 10_670) as f64 / 22_050.0;
            assert!((t - want).abs() <= 0.02, "bar {k} at {t} s, not {want} s");
        }
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
