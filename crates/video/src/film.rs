//! Movies through FilmCraft. Its sources keep their own decoder state and GOP cache, so asking
//! for consecutive frames decodes sequentially and anything else seeks to the keyframe before.

use std::path::Path;
use std::sync::Arc;

use filmcraft_media::{FrameRequest, MediaKind, SharedSource};
use filmcraft_time::Tick;

use crate::{ClipInfo, Decoder, Frame, Timing, guarded};

pub struct FilmDecoder {
    source: SharedSource,
    info: ClipInfo,
}

impl FilmDecoder {
    pub fn open(path: &Path) -> Result<Self, String> {
        let reader = filmcraft_media::reader::FileReader::open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let source = filmcraft_media::reader::open_reader(
            &name,
            Arc::new(reader),
            &filmcraft_codecs::reader_openers(),
            &filmcraft_codecs::openers(),
        )
        .map_err(|e| format!("{name}: {e}"))?;
        let media = source.info();
        if !matches!(media.kind, MediaKind::Movie) {
            return Err(format!("{name} is not a video (use tex.image_file for stills)"));
        }
        let video = media.video.as_ref().ok_or_else(|| format!("{name} has no video stream"))?;
        let (num, den) = (video.frame_rate.num, video.frame_rate.den);
        if num <= 0 || den <= 0 {
            return Err(format!("{name} has no frame rate"));
        }
        let frames = media.duration.seconds() * num as f64 / den as f64;
        if !(frames.is_finite() && frames >= 1.0) {
            return Err(format!("{name} has no frames"));
        }
        // The spike's count: a hundredth of a frame of tolerance against rounding in the duration.
        let frames = (frames + 0.01).floor() as usize;
        let info = ClipInfo {
            width: video.width,
            height: video.height,
            frames,
            timing: Timing::Rate { num, den },
            codec: video.codec.clone(),
            container: media.container.clone(),
            seeks: true,
        };
        Ok(Self { source, info })
    }
}

impl Decoder for FilmDecoder {
    fn info(&self) -> &ClipInfo {
        &self.info
    }

    fn frame(&mut self, index: usize) -> Result<Frame, String> {
        let Timing::Rate { num, den } = self.info.timing else {
            return Err("no frame rate".into());
        };
        let i = i64::try_from(index).map_err(|_| "frame index out of range".to_string())?;
        // The middle of the frame, so rounding in the container's timestamps cannot pick the
        // neighbour.
        let t = Tick::from_units(2 * i * den + den, 2 * num);
        let source = &self.source;
        guarded(|| source.video_frame(FrameRequest::full(t)).map(Frame::Pixels).map_err(|e| e.to_string()))
    }
}
