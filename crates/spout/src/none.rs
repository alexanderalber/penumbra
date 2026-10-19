//! Off Windows there is no Spout: senders and receivers fail with a message.

use penumbra_gpu::{Gpu, Tex};

use crate::{Alpha, Connected, Format, Received, Sent};

const MISSING: &str = "Spout exists only on Windows";

pub struct Sender;

impl Sender {
    pub fn new(_gpu: &Gpu, _name: &str) -> Result<Self, String> {
        Err(MISSING.into())
    }

    pub fn name(&self) -> &str {
        ""
    }

    pub fn counts(&self) -> (u64, u64) {
        (0, 0)
    }

    pub fn sharing(&self) -> Option<(u32, u32, Format)> {
        None
    }

    pub fn send(&mut self, _gpu: &Gpu, _tex: &Tex, _format: Format, _alpha: Alpha) -> Result<Sent, String> {
        Err(MISSING.into())
    }
}

pub struct Receiver;

impl Receiver {
    pub fn new(_gpu: &Gpu) -> Result<Self, String> {
        Err(MISSING.into())
    }

    pub fn connected(&self) -> Option<Connected> {
        None
    }

    pub fn frames(&self) -> u64 {
        0
    }

    pub fn disconnect(&mut self) {}

    pub fn receive(
        &mut self,
        _gpu: &Gpu,
        _name: &str,
        _alpha: Alpha,
        _again: bool,
    ) -> Result<Received, String> {
        Err(MISSING.into())
    }
}

pub struct Watch;

impl Watch {
    pub fn new(_name: &str, _wake: impl Fn() + Send + 'static) -> Self {
        Self
    }
}
