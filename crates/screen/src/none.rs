//! Off Windows there is no screen capture yet: a capture fails with a message.

use penumbra_gpu::{Gpu, Tex};

use crate::{Options, Sources, Status, Target};

const MISSING: &str = "screen capture exists only on Windows so far";

pub struct Capture;

impl Capture {
    pub fn new(
        _gpu: &Gpu,
        _target: Target,
        _options: Options,
        _wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Err(MISSING.into())
    }

    pub fn take(&mut self, _gpu: &Gpu) -> Result<Option<Tex>, String> {
        Err(MISSING.into())
    }

    pub fn status(&self) -> Status {
        Status { error: Some(MISSING.into()), ..Status::default() }
    }

    pub fn frames(&self) -> u64 {
        0
    }

    pub fn bytes(&self) -> u64 {
        0
    }
}

pub fn sources() -> Sources {
    Sources { error: Some(MISSING.into()), ..Sources::default() }
}
