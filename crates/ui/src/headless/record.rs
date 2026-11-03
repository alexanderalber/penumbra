//! Recording a headless run as numbered PNG frames on a stepped show clock: every recorded frame
//! moves the show by exactly `1 / fps` seconds, cooks, draws the UI once and saves it, however
//! long that takes. So a recording is the same on a fast and a slow machine, and a sound file
//! the show plays lines up with the frames when an editor puts both on a timeline.
//!
//! Beside the frames, `marks.jsonl` says which frames each script line covered (its first and
//! last frame), for an edit that cuts or zooms by script step.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use penumbra_core::lock;
use penumbra_engine::Engine;
use serde_json::{Value as Json, json};

use crate::app::PenumbraApp;

/// Frames waiting to be encoded, at most; the run waits when the encoders fall behind.
const QUEUE: usize = 8;
/// Threads encoding PNGs.
const ENCODERS: usize = 3;
/// Most harness frames spent waiting for the UI to show a step's pass.
const CATCH_UP: usize = 8;
/// How long the engine may take for one step.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// Recorded frames per second accepted.
pub const FPS: std::ops::RangeInclusive<u32> = 1..=240;

struct Picture {
    file: PathBuf,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// PNG encoders on threads of their own.
struct Writer {
    tx: Option<SyncSender<Picture>>,
    threads: Vec<JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}

impl Writer {
    fn new() -> Result<Writer, String> {
        let (tx, rx) = sync_channel::<Picture>(QUEUE);
        let rx = Arc::new(Mutex::new(rx));
        let error = Arc::new(Mutex::new(None));
        let mut threads = Vec::new();
        for i in 0..ENCODERS {
            let (rx, error): (Arc<Mutex<Receiver<Picture>>>, _) = (rx.clone(), error.clone());
            let thread = std::thread::Builder::new()
                .name(format!("record png {i}"))
                .spawn(move || {
                    loop {
                        let next = lock(&rx).recv();
                        let Ok(p) = next else { return };
                        let written = penumbra_gpu::encode_png_rgba8(p.width, p.height, p.rgba)
                            .map_err(|e| e.0)
                            .and_then(|png| {
                                std::fs::write(&p.file, png).map_err(|e| format!("{}: {e}", p.file.display()))
                            });
                        if let Err(e) = written {
                            lock(&error).get_or_insert(e);
                        }
                    }
                })
                .map_err(|e| format!("cannot start a PNG encoder: {e}"))?;
            threads.push(thread);
        }
        Ok(Writer { tx: Some(tx), threads, error })
    }

    fn write(&self, picture: Picture) -> Result<(), String> {
        if let Some(e) = lock(&self.error).clone() {
            return Err(e);
        }
        let tx = self.tx.as_ref().ok_or("the recording has ended")?;
        tx.send(picture).map_err(|_| "the PNG encoders have stopped".to_string())
    }

    /// Waits until every frame is written; the first error, if any.
    fn finish(mut self) -> Result<(), String> {
        self.tx = None;
        for t in self.threads.drain(..) {
            if t.join().is_err() {
                return Err("a PNG encoder failed".into());
            }
        }
        match lock(&self.error).clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// One recording in progress.
pub struct Recorder {
    dir: PathBuf,
    pub fps: u32,
    /// Show frames per recorded frame.
    per_frame: u64,
    /// Recorded frames so far.
    pub frames: u64,
    writer: Writer,
    marks: Vec<Json>,
    engine: Arc<Engine>,
}

/// The `fps` of a `record` line, if it starts a recording.
pub fn fps_of(line: &Json) -> Option<u64> {
    line.get("record").and_then(|r| r.get("fps")).and_then(Json::as_u64)
}

impl Recorder {
    /// Starts recording into `dir` at `fps` frames per second: the show clock steps from now
    /// on. `fps` must divide the show rate (30 of 60, say).
    pub fn start(engine: Arc<Engine>, dir: &Path, fps: u32) -> Result<Recorder, String> {
        if !FPS.contains(&fps) {
            return Err(format!("`fps` is {fps}; give {} to {}", FPS.start(), FPS.end()));
        }
        let rate = engine.on_cook(|st| st.clock.rate()).map_err(|e| e.0)?;
        // Show frames per recorded frame: rate / fps, a whole number.
        let (num, den) = (u64::from(rate.num()), u64::from(rate.den()));
        let wanted = u64::from(fps) * den;
        if num % wanted != 0 {
            return Err(format!(
                "the show runs at {rate}; record at a rate that divides it ({fps} does not)"
            ));
        }
        let per_frame = num / wanted;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let busy = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("frame_"));
        if busy {
            return Err(format!("{} already holds frames; record into an empty folder", dir.display()));
        }
        let writer = Writer::new()?;
        engine.set_stepping(true).map_err(|e| e.0)?;
        Ok(Recorder { dir: dir.to_path_buf(), fps, per_frame, frames: 0, writer, marks: Vec::new(), engine })
    }

    /// Show seconds as recorded frames, at least one.
    pub fn frames_in(&self, seconds: f64) -> u64 {
        // `as`: clamped seconds of at most 600 times at most 240 frames.
        ((seconds.clamp(0.0, 600.0) * f64::from(self.fps)).round() as u64).max(1)
    }

    /// Moves the show one recorded frame on, draws the UI (feeding one synthetic event) and
    /// queues the picture.
    pub fn frame(&mut self, harness: &mut egui_kittest::Harness<'_, PenumbraApp>) -> Result<(), String> {
        let pass = self.engine.step(self.per_frame, STEP_TIMEOUT).map_err(|e| e.0)?;
        super::step(harness);
        for _ in 0..CATCH_UP {
            if harness.state().drawn_pass() >= pass {
                break;
            }
            super::step(harness);
        }
        let image = harness.render()?;
        let (width, height) = image.dimensions();
        let file = self.dir.join(format!("frame_{:06}.png", self.frames));
        self.writer.write(Picture { file, width, height, rgba: image.into_raw() })?;
        self.frames += 1;
        Ok(())
    }

    /// Notes that script line `line` (counted from 1 in the file; `what`: its method or kind)
    /// covered the frames from `first` up to now.
    pub fn mark(&mut self, line: usize, what: &str, first: u64) {
        if self.frames > first {
            self.marks.push(json!({"line": line, "what": what, "first": first, "last": self.frames - 1}));
        }
    }

    /// Writes the last frames and the marks and puts the clock back on the timer.
    pub fn stop(self) -> Result<Json, String> {
        let stepping = self.engine.set_stepping(false).map_err(|e| e.0);
        self.writer.finish()?;
        stepping?;
        let marks: String = self.marks.iter().map(|m| format!("{m}\n")).collect();
        let file = self.dir.join("marks.jsonl");
        std::fs::write(&file, marks).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(json!({
            "dir": self.dir.display().to_string(),
            "frames": self.frames,
            "fps": self.fps,
            "seconds": self.frames as f64 / f64::from(self.fps),
        }))
    }
}
