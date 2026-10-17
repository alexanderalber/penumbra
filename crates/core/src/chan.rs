//! Channel data, what flows along a `chan` connection.
//!
//! A [`Chans`] is a set of named channels sharing one sample rate and one length, like a
//! TouchDesigner CHOP. Most nodes produce one sample per channel per show frame (an LFO, a
//! filter); audio and analysis nodes will produce many. The **last** sample is the value "now":
//! that is what a parameter bound to a channel reads.

use crate::{Error, Name};

/// Most channels in one [`Chans`].
pub const MAX_CHANNELS: usize = 4096;
/// Most samples over all channels of one [`Chans`] (8 MiB of `f64`).
pub const MAX_SAMPLES: usize = 1 << 20;

/// What the samples of a [`Chans`] are spread over, for showing them: time (the rate is
/// samples per second, the last sample is now) or a plain index (bands of a spectrum, pixels of
/// a row), where the rate says nothing about the samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleAxis {
    #[default]
    Time,
    Index,
}

/// One named channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: Name,
    pub samples: Vec<f64>,
}

impl Channel {
    /// The last sample, the value now.
    pub fn value(&self) -> f64 {
        self.samples.last().copied().unwrap_or(0.0)
    }
}

/// Named channels with a common rate and length. Built only through [`Chans::new`] or
/// [`Chans::single`], so names are unique, every channel has the same number of samples (at
/// least one) and all samples are finite.
#[derive(Clone, Debug, PartialEq)]
pub struct Chans {
    rate: f64,
    channels: Vec<Channel>,
    axis: SampleAxis,
    /// The samples are the newest of a window this long ([`Chans::window`]); 0 for none.
    window: usize,
}

impl Chans {
    /// Checks the invariants. Non-finite samples are replaced by 0 rather than refused, so one
    /// division by zero upstream does not stop a show.
    pub fn new(rate: f64, mut channels: Vec<Channel>) -> Result<Self, Error> {
        if !(rate.is_finite() && rate > 0.0) {
            return Err(Error::new(format!("sample rate {rate} must be positive")));
        }
        if channels.len() > MAX_CHANNELS {
            return Err(Error::new(format!("{} channels, at most {MAX_CHANNELS}", channels.len())));
        }
        let len = channels.first().map_or(1, |c| c.samples.len());
        if len == 0 {
            return Err(Error::new("a channel needs at least one sample"));
        }
        if len.saturating_mul(channels.len()) > MAX_SAMPLES {
            return Err(Error::new(format!("more than {MAX_SAMPLES} samples")));
        }
        for c in &mut channels {
            if c.samples.len() != len {
                return Err(Error::new(format!(
                    "channel `{}` has {} samples, the first has {len}",
                    c.name,
                    c.samples.len()
                )));
            }
            for s in &mut c.samples {
                if !s.is_finite() {
                    *s = 0.0;
                }
            }
        }
        for (i, c) in channels.iter().enumerate() {
            if channels.iter().take(i).any(|d| d.name == c.name) {
                return Err(Error::new(format!("channel `{}` appears twice", c.name)));
            }
        }
        Ok(Self { rate, channels, axis: SampleAxis::Time, window: 0 })
    }

    /// One channel with one sample.
    pub fn single(name: Name, value: f64, rate: f64) -> Result<Self, Error> {
        Self::new(rate, vec![Channel { name, samples: vec![value] }])
    }

    /// No channels at all (what a node with nothing to say produces).
    pub fn empty(rate: f64) -> Result<Self, Error> {
        Self::new(rate, Vec::new())
    }

    /// The same channels, their samples spread over `axis` ([`Chans::new`] makes them time).
    pub fn with_axis(mut self, axis: SampleAxis) -> Self {
        self.axis = axis;
        self
    }

    pub fn axis(&self) -> SampleAxis {
        self.axis
    }

    /// The same channels as the newest `window` samples of a window that is not full yet (a
    /// `chan.trail` still growing), for showing them at their place in it.
    pub fn with_window(mut self, window: usize) -> Self {
        self.window = window;
        self
    }

    /// How many samples the window the samples sit at the end of holds: [`Chans::len`] unless
    /// [`Chans::with_window`] made it longer.
    pub fn window(&self) -> usize {
        self.window.max(self.len())
    }

    /// The same channels, their samples spread over the axis and window of `other`; for a node
    /// whose output has the shape of its input.
    pub fn shaped_like(self, other: &Chans) -> Self {
        let window = if self.len() == other.len() { other.window } else { 0 };
        self.with_axis(other.axis).with_window(window)
    }

    /// Samples per second.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Samples per channel (1 for an empty set).
    pub fn len(&self) -> usize {
        self.channels.first().map_or(1, |c| c.samples.len())
    }

    /// Whether there are no channels.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn get(&self, name: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.name.as_str() == name)
    }

    /// The value now of channel `name`, or of the first channel when `name` is `None`.
    pub fn value(&self, name: Option<&str>) -> Option<f64> {
        match name {
            Some(n) => self.get(n).map(Channel::value),
            None => self.channels.first().map(Channel::value),
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &Name> + '_ {
        self.channels.iter().map(|c| &c.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str, samples: &[f64]) -> Channel {
        Channel { name: Name::new(name).unwrap(), samples: samples.to_vec() }
    }

    #[test]
    fn invariants() {
        let c = Chans::new(60.0, vec![ch("x", &[1.0, 2.0]), ch("y", &[3.0, f64::NAN])]).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c.value(Some("x")), Some(2.0));
        assert_eq!(c.value(Some("y")), Some(0.0), "NaN becomes 0");
        assert_eq!(c.value(None), Some(2.0));
        assert_eq!(c.value(Some("z")), None);
        assert!(Chans::new(60.0, vec![ch("x", &[1.0]), ch("x", &[2.0])]).is_err());
        assert!(Chans::new(60.0, vec![ch("x", &[1.0]), ch("y", &[1.0, 2.0])]).is_err());
        assert!(Chans::new(60.0, vec![ch("x", &[])]).is_err());
        assert!(Chans::new(0.0, vec![]).is_err());
        assert!(Chans::new(f64::NAN, vec![]).is_err());
        let e = Chans::empty(60.0).unwrap();
        assert!(e.is_empty());
        assert_eq!(e.value(None), None);
        assert!(Chans::new(1.0, vec![ch("x", &vec![0.0; MAX_SAMPLES + 1])]).is_err());
        assert_eq!(c.axis(), SampleAxis::Time);
        // A window is never shorter than the samples, and passes on only to the same length.
        assert_eq!(c.window(), 2);
        let c = c.with_window(10);
        assert_eq!(c.window(), 10);
        let same = Chans::new(60.0, vec![ch("z", &[0.0, 1.0])]).unwrap();
        assert_eq!(same.shaped_like(&c).window(), 10);
        let shorter = Chans::single(Name::new("z").unwrap(), 1.0, 60.0).unwrap();
        assert_eq!(shorter.shaped_like(&c).window(), 1);
        assert_eq!(c.with_axis(SampleAxis::Index).axis(), SampleAxis::Index);
    }
}
