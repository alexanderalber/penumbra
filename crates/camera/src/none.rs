//! The backend for systems without camera support yet (V4L2 on Linux is planned): listing
//! finds nothing, opening explains why.

use crate::frame::{Matrix, Planes};
use crate::mode::{DeviceInfo, Mode, Wanted};

const MISSING: &str = "camera input is not built for this system yet";

pub(crate) fn devices() -> Result<Vec<DeviceInfo>, String> {
    Err(MISSING.into())
}

pub(crate) struct Opened {
    pub device: String,
    pub mode: Mode,
    pub modes: Vec<Mode>,
}

pub(crate) struct Sample {
    pub planes: Planes,
    pub matrix: Matrix,
    pub full_range: bool,
    pub stamp: Option<i64>,
    pub delay_ms: Option<f64>,
    /// Milliseconds spent taking the frame out of the camera's buffer.
    pub copy_ms: f64,
}

pub(crate) struct Capture;

impl Capture {
    pub fn open(_pattern: &str, _wanted: &Wanted) -> Result<(Self, Opened), String> {
        Err(MISSING.into())
    }

    pub fn read(&mut self) -> Result<Option<Sample>, String> {
        Err(MISSING.into())
    }
}
