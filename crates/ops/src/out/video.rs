//! `out.video`: records its input to a movie file (H.264 in MP4, or ProRes in MOV).
//!
//! The node never waits. While it records, every frame of the recording's rate converts the
//! input to Y'CbCr planes on the GPU and starts reading them back
//! ([`penumbra_gpu::YuvReader`]); the encoder's thread ([`penumbra_record`]) waits for them,
//! encodes and writes. When the encoder falls behind, the queue and the readback ring fill up
//! and frames are dropped (counted); the frame before then lasts longer, so the file keeps time
//! with the show.
//!
//! Frames follow the show clock: frame k of the file is the picture at k / rate seconds after
//! the start. While the clock stands still nothing is recorded; a jump back in time or of more
//! than a second forward continues the file without a gap.
//!
//! `record` is the one switch: the panel's record button and `video_out.start` / `.stop` set it
//! (as edits), a binding drives it. A recording starts when it is on and stops when it turns
//! off, the node is deleted, its project closed or the app quit; stopping finishes the file.
//! `record` opens off whatever the project file says, so opening a project never starts a
//! recording by itself. A recording that could not start or ended by itself (an error) is not
//! tried again until `record` turns off and on.
//!
//! Sound: with `audio` at `all` the file also gets what every `out.audio` of the project plays,
//! summed as their devices get it (after reverb, level and limiter, before `audio.mute`), through
//! a tap of the [`Outputs`] hub. Each block goes where its show time falls in the file, by the
//! same start the frames count from, so picture and sound stay together through dropped frames
//! and jumps. Blocks of a pass reach the node at its next cook. An MP4 gets AAC, a MOV 24-bit
//! PCM ([`penumbra_record::audio`]). Loops made of the file are silent.
//!
//! With `loop` set, a finished recording is made into a loop as well
//! ([`penumbra_record::looping`]), on a thread of the record hub's; the node warns while it runs.
//! The params count as they are when the recording stops.

use penumbra_audio::{Outputs, Tap};
use penumbra_core::{Error, NodeId, ParamSpec, Rate, Resolves, Tick, Unit, Value};
use penumbra_gpu::{Gpu, MappedPlanes, PlaneLayout, Tex, YuvReader};
use penumbra_graph::{
    CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};
use penumbra_record::looping::{Method, Spec};
use penumbra_record::{AudioBlock, Codec, Fps, Frame, H264Preset, Hub, Layout, Planes, Recording, Settings};

use crate::audio::block::sample_at;
use crate::on_gpu;

/// Kind of the movie recording node.
pub const VIDEO_OUT: &str = "out.video";
/// Bytes the readback ring of one recording may hold (the ring has 3 to 10 frames).
const RING_BYTES: usize = 256 << 20;
/// The H.264 preset a node has unless set: the fastest. Recording busy 1080p60 pictures inside
/// the running show on an 8-core, 16-thread CPU, it ran at 75 to 105 frames per second where
/// balanced ran at 62 to 74, with files no larger.
pub const DEFAULT_PRESET: H264Preset = H264Preset::Fast;
/// Where recordings go when `file` is empty, relative to the project.
pub const FOLDER: &str = "recordings";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let preset = H264Preset::NAMES.iter().fold(
        ParamSpec::choice("preset", &H264Preset::NAMES).doc(
            "How hard the H.264 encoder works per frame: faster drops fewer frames of a busy picture, \
             slower keeps more fine detail",
        ),
        |spec, name| {
            let doc = match *name {
                "fast" => {
                    "the fastest: Baseline profile, constant quality 23; files no larger than balanced's, \
                     fine detail a little softer; 20 to 45 % faster than balanced on busy pictures"
                }
                "balanced" => "High profile, constant quality 18, the encoder's quickest search",
                _ => {
                    "High profile, quality 18, a wider search: slightly smaller files, a third slower than \
                     balanced; for calm pictures, smaller sizes or lower rates"
                }
            };
            spec.choice_doc(name, doc)
        },
    );
    let preset = ParamSpec { default: Value::Text(DEFAULT_PRESET.name().to_string()), ..preset };
    let codec = Codec::NAMES.iter().fold(
        ParamSpec::choice("codec", &Codec::NAMES).doc("What the picture is encoded as (the file's extension follows it)"),
        |spec, name| {
            let doc = match *name {
                "h264" => "H.264 in MP4: small files any player opens; 4:2:0, 8 bits, high constant quality",
                "prores_422" => "Apple ProRes 422 in MOV: for editing, every frame a keyframe; about 37 MB/s at 1080p60",
                "prores_422_hq" => "Apple ProRes 422 HQ in MOV: as 422 at a higher rate, about 55 MB/s at 1080p60",
                _ => "Apple ProRes 4444 in MOV with alpha: 4:4:4, 12 bits, the transparency kept; the largest files",
            };
            spec.choice_doc(name, doc)
        },
    );
    let looping = ["off"].into_iter().chain(Method::NAMES).collect::<Vec<_>>();
    let looping = ParamSpec::choice("loop", &looping)
        .doc(
            "After recording stops, also write a loop of the file as <name>_loop: a movie a player can \
             repeat without a jump. The node shows looping... until it is written",
        )
        .choice_doc("off", "no loop")
        .choice_doc(
            "cut",
            "keeps the stretch whose end looks most like its start, at least loop_min long; for content \
             that comes back to similar pictures",
        )
        .choice_doc(
            "fade",
            "cross-fades the last loop_fade seconds into the start; works for any content, the fade may show \
             as a soft double picture",
        )
        .choice_doc("pingpong", "plays forward, then backward; seamless, motion reverses at both ends")
        .choice_doc("cut_fade", "cuts, then cross-fades over the seam that remains");
    types.register(FnNodeType::new(
        NodeDesc::new(VIDEO_OUT, "Records its input to a movie file (H.264 or ProRes)")
            .param(ParamSpec::bool("record", false).opens_at_default().doc(
                "Recording while on: turning record on starts a new file, turning record off finishes the \
                 file. The record button above and video_out.start / .stop switch this param; bind it to a \
                 channel to start and stop from OSC or a button. A project always opens with record off",
            ))
            .param(codec)
            .param(
                ParamSpec::choice("audio", &["all", "off"])
                    .doc("Whether the file gets sound, fixed when the recording starts")
                    .choice_doc(
                        "all",
                        "what every out.audio of the project plays, added up as the speakers get it \
                         (audio.mute silences the room, not the file); AAC in an MP4, 24-bit PCM in a MOV. \
                         No out.audio: no sound",
                    )
                    .choice_doc("off", "no sound"),
            )
            .param(preset)
            .param(ParamSpec::text("file", "").doc(
                "The movie file, relative to the project; empty records to recordings/<node name>. A file that \
                 exists is never overwritten: the recording gets the next free name with _2, _3, ...",
            ))
            .param(
                ParamSpec::float("rate", 0.0)
                    .clamp(0.0, 240.0)
                    .range(0.0, 120.0)
                    .unit(Unit::Hertz)
                    .special_resolved(0.0, "show rate", Resolves::ShowRate)
                    .doc(
                        "Frames per second of the file. Frames follow the show clock: a slower show makes \
                         frames last longer, a faster show leaves some out",
                    ),
            )
            .param(
                ParamSpec::choice("size", &["input", "fixed"])
                    .doc("The picture size of the file, fixed when the recording starts")
                    .choice_doc("input", "the input's size when recording starts (rounded down to even)")
                    .choice_doc("fixed", "width x height; the input is scaled to it, aspect not kept"),
            )
            .param(ParamSpec::int("width", 1920).clamp(16.0, 8192.0).range(16.0, 3840.0).doc(
                "Width of the file in pixels with `size` fixed (rounded down to even)",
            ))
            .param(ParamSpec::int("height", 1080).clamp(16.0, 8192.0).range(16.0, 2160.0).doc(
                "Height of the file in pixels with `size` fixed (rounded down to even)",
            ))
            .param(ParamSpec::int("threads", 0).clamp(0.0, 64.0).range(0.0, 16.0).more().special(0.0, "automatic").doc(
                "Encoder threads; automatic is all of this machine's logical cores but two. Fewer threads \
                 leave more to the rest of the show (video decoding, scripts) but drop frames of busy \
                 pictures sooner",
            ))
            .param(looping)
            .param(
                ParamSpec::float("loop_min", 5.0)
                    .clamp(0.0, 3600.0)
                    .range(1.0, 60.0)
                    .unit(Unit::Seconds)
                    .more()
                    .doc("Shortest loop `cut` may keep. A longer minimum repeats less often but may find a worse seam"),
            )
            .param(
                ParamSpec::float("loop_fade", 1.0)
                    .clamp(0.0, 600.0)
                    .range(0.1, 10.0)
                    .unit(Unit::Seconds)
                    .more()
                    .doc("Length of the cross-fade at the seam. The loop gets this much shorter than the recording"),
            )
            .input("in", DataKind::Tex)
            .describe_input("in", "The picture to record")
            // Reads the clock: a frame is due at the recording's rate whether or not the input
            // changed.
            .time_dependent(),
        || Box::new(VideoOut::default()) as Box<dyn Operator>,
    )
    .inactive(inactive)
    .warm_up(on_gpu(|gpu| gpu.yuv_pipeline().map(|_| ()))))
}

/// Why param `name` of an `out.video` does not count with its other params; `None` while it
/// counts.
fn inactive(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let size = node.param(desc, "size").and_then(Value::as_str).unwrap_or("input");
    let codec = node.param(desc, "codec").and_then(Value::as_str).unwrap_or("h264");
    let method = node.param(desc, "loop").and_then(Value::as_str).and_then(Method::parse);
    match (size, name) {
        ("input", "width" | "height") => Some("`size` is input: the file takes the input's size"),
        _ if name == "preset" && codec != "h264" => Some("only the h264 codec has presets"),
        _ if name == "loop_min" && !method.is_some_and(Method::cuts) => {
            Some("only cut and cut_fade search a loop")
        }
        _ if name == "loop_fade" && !method.is_some_and(Method::fades) => {
            Some("only fade and cut_fade cross-fade")
        }
        _ => None,
    }
}

/// The GPU plane layout of an encoder's layout.
pub fn plane_layout(l: &Layout) -> PlaneLayout {
    let [y, cb, cr, a] = l.offsets().map(|o| o as u64);
    PlaneLayout {
        width: l.width,
        height: l.height,
        shift: l.shift,
        bits: l.bits,
        alpha: l.alpha,
        luma_stride: l.luma_stride(),
        chroma_stride: l.chroma_stride(),
        offsets: [y, cb, cr, a],
        bytes: l.bytes() as u64,
    }
}

/// Frames of `layout` the readback ring holds: as many as fit [`RING_BYTES`], 3 to 10.
pub fn ring_frames(layout: &Layout) -> usize {
    (RING_BYTES / layout.bytes().max(1)).clamp(3, 10)
}

/// A frame read back from the GPU, as the encoder's thread takes it.
struct GpuPlanes(MappedPlanes);

impl Planes for GpuPlanes {
    fn read(
        &mut self,
        timeout: std::time::Duration,
        f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.0.wait(timeout).map_err(|e| e.0)?;
        self.0.with_bytes(|b| f(b)).map_err(|e| e.0)?
    }
}

/// The loop the params ask for, made of a file in `codec`.
fn loop_spec(cx: &CookContext<'_>, codec: Codec) -> Result<Option<Spec>, CookError> {
    let Some(method) = Method::parse(cx.text("loop")?) else { return Ok(None) };
    let mut spec = Spec::new(method);
    spec.min_seconds = cx.float("loop_min")?.max(0.0);
    spec.fade_seconds = cx.float("loop_fade")?.max(0.0);
    spec.codec = Some(codec);
    Ok(Some(spec))
}

/// A recording under way and where it stands.
struct Take {
    recording: Recording,
    codec: Codec,
    reader: YuvReader,
    planes: PlaneLayout,
    queue: usize,
    /// Show time of frame 0.
    start: Tick,
    /// The last frame index taken or dropped.
    last: i64,
    /// Hears the project's sound, with `audio` all.
    tap: Option<Tap>,
}

#[derive(Default)]
struct VideoOut {
    hub: Option<(Hub, NodeId)>,
    take: Option<Take>,
    /// A recording with sound that stopped at the last cook, kept for one more: the sound of
    /// the pass that stopped it reaches the tap only after that pass.
    closing: Option<Take>,
    /// A recording was started (or tried) since `record` last turned on: not again until it
    /// turns off.
    tried: bool,
    /// Why the last start failed, shown while `record` stays on.
    failed: Option<String>,
}

impl Drop for VideoOut {
    fn drop(&mut self) {
        // The recording finishes its file on its own thread.
        self.take = None;
        if let Some((hub, node)) = &self.hub {
            hub.detach(*node);
        }
    }
}

/// The frame index of show time `now` in a recording at `fps` started at `start`.
fn index_at(now: Tick, start: Tick, fps: Fps) -> i64 {
    let elapsed = i128::from(now.0) - i128::from(start.0);
    let per = i128::from(fps.den.max(1)) * i128::from(penumbra_core::TICKS_PER_SECOND);
    let i = (elapsed * i128::from(fps.num)).div_euclid(per);
    i.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// The show time at which frame `index` of a recording started at `start` is due.
fn time_of(index: i64, start: Tick, fps: Fps) -> Tick {
    let t = i128::from(index) * i128::from(fps.den) * i128::from(penumbra_core::TICKS_PER_SECOND)
        / i128::from(fps.num.max(1));
    Tick((i128::from(start.0) + t).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

fn fps_of(rate: f64, show: Rate) -> Result<Fps, CookError> {
    if rate.is_nan() || rate <= 0.0 {
        return Ok(Fps { num: show.num(), den: show.den() });
    }
    Fps::from_f64(rate)
        .ok_or_else(|| CookError::failed(format!("`rate` {rate} is not 1 to 1000 frames per second")))
}

impl VideoOut {
    fn start(&mut self, cx: &CookContext<'_>, hub: &Hub, node: NodeId, now: Tick) -> Result<(), CookError> {
        self.take = None;
        let codec_name = cx.text("codec")?;
        let codec = Codec::parse(codec_name)
            .ok_or_else(|| CookError::failed(format!("unknown codec `{codec_name}`")))?;
        let tex = cx.input_as::<Tex>(0);
        let (w, h) = match cx.text("size")? {
            "fixed" => {
                let side = |v: i64| u32::try_from(v.clamp(16, 8192)).unwrap_or(16);
                (side(cx.int("width")?), side(cx.int("height")?))
            }
            _ => {
                tex.map(Tex::size).ok_or_else(|| CookError::failed("nothing to record: connect a picture"))?
            }
        };
        let (w, h) = (w & !1, h & !1);
        let fps = fps_of(cx.float("rate")?, cx.rate())?;
        let file = cx.text("file")?.trim();
        let mut path = if file.is_empty() {
            cx.resolve_path(FOLDER)?.join(cx.def().name.as_str())
        } else {
            cx.resolve_path(file)?
        };
        if path.extension().is_none() {
            path.set_extension(codec.extension());
        }
        let h264 = cx.text("preset").ok().and_then(H264Preset::parse).unwrap_or(DEFAULT_PRESET);
        let threads = match usize::try_from(cx.int("threads")?).unwrap_or(0) {
            0 => penumbra_record::writer::default_threads(),
            n => n,
        };
        let layout = codec.layout(w, h);
        layout.check().map_err(CookError::failed)?;
        let ring = ring_frames(&layout);
        let queue = ring.saturating_sub(1).max(1);
        let settings = Settings { path, codec, width: w, height: h, fps, threads, queue, h264 };
        let recording = hub.start(node, settings).map_err(CookError::failed)?;
        recording.then_loop(loop_spec(cx, codec)?);
        let tap = match (cx.text("audio")?, cx.resource::<Outputs>()) {
            ("all", Some(outputs)) => {
                recording.sound(outputs.lanes(), 0, 0);
                Some(outputs.tap())
            }
            _ => None,
        };
        self.take = Some(Take {
            codec,
            planes: plane_layout(&recording.layout()),
            recording,
            reader: YuvReader::new(ring),
            queue,
            start: now,
            last: -1,
            tap,
        });
        Ok(())
    }
}

impl Take {
    /// Hands the sound heard since the last cook to the recording, each block at the file's
    /// sample of its show time. Called before [`Take::frame`], which may move the start: the
    /// blocks are from the pass before, when the start was still where it was.
    fn listen(&self, outputs: Option<&Outputs>) {
        let Some(tap) = &self.tap else { return };
        for block in tap.take() {
            let at = block.first.saturating_sub(sample_at(self.start, block.rate));
            self.recording.push_audio(AudioBlock { at, rate: block.rate, channels: block.channels });
        }
        let sources = outputs.map_or(0, Outputs::lanes);
        self.recording.sound(sources, tap.dropped(), tap.refused());
    }

    /// Takes the frame due at `now`, if one is.
    fn frame(&mut self, gpu: &Gpu, tex: Option<&Tex>, now: Tick) -> Result<(), CookError> {
        let fps = self.recording.fps();
        let mut index = index_at(now, self.start, fps);
        if index == self.last {
            // Not due yet, or the clock stands still.
            return Ok(());
        }
        if index < self.last {
            // The clock went back: carry on after the last frame.
            index = self.last + 1;
            self.start = Tick(now.0 - (time_of(index, self.start, fps).0 - self.start.0));
        }
        let gap = index - self.last - 1;
        if f64::from(u32::try_from(gap).unwrap_or(u32::MAX)) > fps.as_f64() {
            // More than a second ahead (a jump in time): no gap in the file.
            index = self.last + 1;
            self.start = Tick(now.0 - (time_of(index, self.start, fps).0 - self.start.0));
        } else if gap > 0 {
            self.recording.skipped(u64::try_from(gap).unwrap_or(0));
        }
        self.last = index;
        self.recording.due(index);
        let Some(tex) = tex else {
            self.recording.dropped(1);
            return Ok(());
        };
        if !self.recording.has_room(self.queue) {
            self.recording.dropped(1);
            return Ok(());
        }
        match self.reader.grab(gpu, tex, &self.planes)? {
            Some(planes) => {
                if self.recording.push(Frame { index, planes: Box::new(GpuPlanes(planes)) }).is_err() {
                    self.recording.dropped(1);
                }
            }
            None => self.recording.dropped(1),
        }
        Ok(())
    }
}

impl Operator for VideoOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Hub>()
            .ok_or_else(|| CookError::failed("recording is not available in this process"))?;
        let node = cx.node();
        if self.hub.is_none() {
            self.hub = Some((hub.clone(), node));
        }
        hub.attach(node, cx.def().name.as_str());
        let record = cx.bool("record")?;
        let now = cx.time().tick;
        if self.take.as_ref().is_some_and(|t| !t.recording.is_running()) {
            // Ended by itself (an error): its status says why.
            self.take = None;
        }
        let outputs = cx.resource::<Outputs>();
        if let Some(take) = self.closing.take() {
            // Dropping it finishes the file.
            take.listen(outputs);
        }
        if let Some(take) = &self.take {
            take.listen(outputs);
        }
        if !record {
            // The loop follows the params as they are when the recording stops.
            if let Some(take) = self.take.take() {
                take.recording.then_loop(loop_spec(cx, take.codec).unwrap_or(None));
                if take.tap.is_some() && outputs.is_some_and(|o| o.lanes() > 0) {
                    self.closing = Some(take);
                }
            }
            self.tried = false;
            self.failed = None;
        } else if !self.tried {
            self.tried = true;
            match self.start(cx, hub, node, now) {
                Ok(()) => {}
                Err(CookError::Failed(e)) => self.failed = Some(e),
                Err(CookError::Pending) => self.tried = false,
            }
        }
        if let Some(e) = &self.failed {
            return Err(CookError::failed(format!(
                "not recording: {e}; turn record off and on to try again"
            )));
        }
        if let Some(take) = self.take.as_mut() {
            let gpu = crate::gpu(cx)?;
            take.frame(gpu, cx.input_as::<Tex>(0), now)?;
        }
        // The last recording's error and drops count while `record` is on; once off, the status
        // line of the panel and video_out.status still tell.
        if let Some(s) = hub.status(node).filter(|_| record) {
            if let Some(e) = s.error {
                return Err(CookError::failed(format!("{e}; turn record off and on to try again")));
            }
            if s.dropped > 0 {
                cx.warn(format!("{} frames dropped: the encoder did not keep up", s.dropped));
            }
            if s.recording && cx.input(0).is_none() {
                cx.warn("nothing connected: the file holds the last picture");
            }
        }
        if let Some(l) = hub.status(node).and_then(|s| s.looping) {
            if !l.done {
                cx.warn(format!("looping... {:.0} %", l.progress * 100.0));
            } else if let Some(e) = l.error {
                cx.warn(format!("the loop of the last recording failed: {e}"));
            }
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_the_show_clock() {
        let fps = Fps { num: 30, den: 1 };
        let start = Tick::from_seconds(10.0);
        assert_eq!(index_at(start, start, fps), 0);
        assert_eq!(index_at(Tick::from_seconds(10.0 + 1.0 / 30.0), start, fps), 1);
        assert_eq!(index_at(Tick::from_seconds(10.999), start, fps), 29);
        assert_eq!(index_at(Tick::from_seconds(9.9), start, fps), -3);
        let ntsc = Fps { num: 30000, den: 1001 };
        assert_eq!(index_at(time_of(1000, start, ntsc), start, ntsc), 1000);
        assert_eq!(time_of(30, start, fps), Tick::from_seconds(11.0));
    }

    #[test]
    fn rates() {
        let show = Rate::fps(60).unwrap();
        assert_eq!(fps_of(0.0, show).unwrap(), Fps { num: 60, den: 1 });
        assert_eq!(fps_of(25.0, show).unwrap(), Fps { num: 25, den: 1 });
        assert!(fps_of(f64::NAN, show).is_ok(), "NaN is not above 0: the show rate");
        assert!(fps_of(5000.0, show).is_err());
    }
}
