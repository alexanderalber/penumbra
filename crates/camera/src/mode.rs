//! Devices, their modes, and picking a mode from what a node asks for.

use serde::Serialize;

use crate::frame::PixelFormat;

/// One way a device can deliver frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Frames per second as `num / den` (59.94 is 60000 / 1001).
    pub rate: (u32, u32),
    pub format: PixelFormat,
    /// The backend's own index of the mode.
    pub(crate) index: u32,
}

impl Mode {
    pub fn fps(&self) -> f64 {
        if self.rate.1 == 0 { 0.0 } else { f64::from(self.rate.0) / f64::from(self.rate.1) }
    }

    /// `1920x1080@60 NV12`, `1280x720@59.94 YUY2`: what `mode` takes.
    pub fn label(&self) -> String {
        format!("{}x{}@{} {}", self.width, self.height, fps_text(self.fps()), self.format.name())
    }

    pub fn info(&self) -> ModeInfo {
        ModeInfo {
            label: self.label(),
            width: self.width,
            height: self.height,
            fps: self.fps(),
            format: self.format.name().to_string(),
            readable: self.format.readable(),
        }
    }
}

/// Up to two decimals, without trailing zeros.
fn fps_text(fps: f64) -> String {
    let s = format!("{fps:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A mode as listed for people and commands.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModeInfo {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub format: String,
    /// Whether penumbra reads this pixel format (MJPEG, RGB and 10-bit are listed but not read).
    pub readable: bool,
}

/// A camera of this machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    /// The system's identifier (a symbolic link on Windows).
    pub id: String,
    /// Largest first, each size by rate.
    pub modes: Vec<ModeInfo>,
    /// Why the modes could not be read (the device is busy, for instance).
    pub error: Option<String>,
}

/// What a `mode` text asks for: any of a size (`1920x1080`), a rate (`@60`, also attached to
/// the size) and a pixel format name, in any order. Empty asks for nothing in particular.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wanted {
    pub size: Option<(u32, u32)>,
    pub fps: Option<f64>,
    pub format: Option<PixelFormat>,
}

impl Wanted {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut w = Self::default();
        let bad =
            || format!("`{text}` is not a mode; write it like `1920x1080@60 NV12` (each part optional)");
        for word in text.split_whitespace() {
            let (size, fps) = match word.split_once('@') {
                Some((s, f)) => (s, Some(f)),
                None => (word, None),
            };
            if let Some(f) = fps {
                let f: f64 = f.trim_end_matches("fps").parse().map_err(|_| bad())?;
                if !(f > 0.0 && f < 10_000.0) {
                    return Err(bad());
                }
                w.fps = Some(f);
            }
            if size.is_empty() {
                continue;
            }
            if let Some((a, b)) = size.split_once(['x', 'X']) {
                let (a, b) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
                w.size = Some((a, b));
                continue;
            }
            let format = [PixelFormat::Nv12, PixelFormat::Yuy2, PixelFormat::Uyvy, PixelFormat::I420]
                .into_iter()
                .find(|f| f.name().eq_ignore_ascii_case(size));
            w.format = Some(format.ok_or_else(bad)?);
        }
        Ok(w)
    }
}

/// The mode a node gets: of the readable modes that match what it asks for, the largest, then
/// the fastest, then NV12 before the packed formats.
pub fn choose(modes: &[Mode], wanted: &Wanted) -> Result<Mode, String> {
    let fits = |m: &&Mode| {
        m.format.readable()
            && wanted.size.is_none_or(|s| s == (m.width, m.height))
            && wanted.fps.is_none_or(|f| (m.fps() - f).abs() < 0.5)
            && wanted.format.is_none_or(|f| f == m.format)
    };
    modes
        .iter()
        .filter(fits)
        .max_by(|a, b| {
            let area = |m: &Mode| u64::from(m.width) * u64::from(m.height);
            area(a)
                .cmp(&area(b))
                .then(a.fps().total_cmp(&b.fps()))
                .then(b.format.rank().cmp(&a.format.rank()))
        })
        .copied()
        .ok_or_else(|| {
            let mut have: Vec<String> =
                modes.iter().filter(|m| m.format.readable()).map(Mode::label).collect();
            have.dedup();
            if have.is_empty() {
                "the device offers no pixel format penumbra reads (NV12, YUY2, UYVY, I420)".to_string()
            } else {
                format!("no mode matches; the device offers {}", have.join(", "))
            }
        })
}

/// Sorts modes for listing: largest first, then fastest, then by format preference.
pub fn sort(modes: &mut [Mode]) {
    modes.sort_by(|a, b| {
        let area = |m: &Mode| u64::from(m.width) * u64::from(m.height);
        area(b).cmp(&area(a)).then(b.fps().total_cmp(&a.fps())).then(a.format.rank().cmp(&b.format.rank()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(width: u32, height: u32, rate: (u32, u32), format: PixelFormat) -> Mode {
        Mode { width, height, rate, format, index: 0 }
    }

    #[test]
    fn modes_are_picked_by_what_is_asked() {
        let modes = [
            m(1920, 1080, (60, 1), PixelFormat::Yuy2),
            m(1920, 1080, (60, 1), PixelFormat::Nv12),
            m(1920, 1080, (30, 1), PixelFormat::Nv12),
            m(1280, 720, (60000, 1001), PixelFormat::Nv12),
            m(3840, 2160, (30, 1), PixelFormat::Other),
        ];
        let pick = |t: &str| choose(&modes, &Wanted::parse(t).unwrap()).map(|m| m.label());
        assert_eq!(pick("").unwrap(), "1920x1080@60 NV12");
        assert_eq!(pick("yuy2").unwrap(), "1920x1080@60 YUY2");
        assert_eq!(pick("@30").unwrap(), "1920x1080@30 NV12");
        assert_eq!(pick("1280x720@59.94 NV12").unwrap(), "1280x720@59.94 NV12");
        assert_eq!(pick("1280x720@60").unwrap(), "1280x720@59.94 NV12");
        assert!(pick("3840x2160").unwrap_err().contains("offers 1920x1080@60 YUY2"));
        assert!(Wanted::parse("big").is_err());
        assert!(Wanted::parse("@0").is_err());
        assert!(Wanted::parse("1920xA").is_err());
    }

    #[test]
    fn listing_puts_the_largest_and_fastest_first() {
        let mut modes = [
            m(640, 480, (30, 1), PixelFormat::Yuy2),
            m(1920, 1080, (30, 1), PixelFormat::Nv12),
            m(1920, 1080, (60, 1), PixelFormat::Yuy2),
            m(1920, 1080, (60, 1), PixelFormat::Nv12),
        ];
        sort(&mut modes);
        let labels: Vec<String> = modes.iter().map(Mode::label).collect();
        assert_eq!(
            labels,
            ["1920x1080@60 NV12", "1920x1080@60 YUY2", "1920x1080@30 NV12", "640x480@30 YUY2"]
        );
    }
}
