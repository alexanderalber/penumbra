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
        let (num, den) = reduced(num, den).ok_or_else(|| format!("{name} has no usable frame rate"))?;
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
        let t = frame_time(index, num, den).ok_or_else(|| format!("frame {index} is out of range"))?;
        let source = &self.source;
        guarded(|| source.video_frame(FrameRequest::full(t)).map(Frame::Pixels).map_err(|e| e.to_string()))
    }
}

/// The largest frame rate denominator accepted. Real files use 1, 1001 or a timescale like 90000;
/// beyond this, the time of a late frame would no longer fit the arithmetic.
const MAX_RATE_DEN: i64 = 1 << 40;

/// A frame rate `num / den` in lowest terms, or `None` when it is not positive or its
/// denominator is beyond [`MAX_RATE_DEN`].
pub(crate) fn reduced(num: i64, den: i64) -> Option<(i64, i64)> {
    if num <= 0 || den <= 0 {
        return None;
    }
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let (num, den) = (num / a, den / a);
    (den <= MAX_RATE_DEN).then_some((num, den))
}

/// The time asked for when frame `index` is wanted, or `None` when it does not fit.
///
/// A quarter into the frame, so rounding in the container's timestamps cannot pick the
/// neighbour. Not the middle: FilmCraft rounds the time to the nearest unit of the track's
/// timescale, and in a file whose unit is one frame (penumbra's recordings) the middle of frame
/// i rounds up to frame i + 1.
pub(crate) fn frame_time(index: usize, num: i64, den: i64) -> Option<Tick> {
    let i = i64::try_from(index).ok()?;
    let units = i.checked_mul(4)?.checked_mul(den)?.checked_add(den)?;
    let per_second = num.checked_mul(4)?;
    (per_second > 0).then(|| Tick::from_units(units, per_second))
}
