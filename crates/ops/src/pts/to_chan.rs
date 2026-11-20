//! `chan.from_pts`: numbers about points as channels, so a network can react to them: `count`,
//! how many points there are, and `capacity`, how many there may be. With an `attribute` named,
//! also its values at the first points and its mean, least and greatest value over all points
//! (TouchDesigner's POP to CHOP and Analyze POP), gathered on the GPU ([`Probe`]) so only a few
//! bytes come back.
//!
//! A count the CPU knows comes out at once. Anything read from the GPU (a count after
//! `pts.delete`, an attribute's values) is read back without the cook waiting for it: the
//! channels follow a frame or two late, and keep their last values until the next ones arrive.
//! A render that needs every frame exact waits for them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec};
use penumbra_gpu::points::probe::{Ask, MAX_FIRST, Probe, Probed};
use penumbra_gpu::points::{AttrType, canonical, default_of};
use penumbra_gpu::{BufferRead, Points, ReadState};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::input;
use crate::gpu;

pub const KIND: &str = "chan.from_pts";

/// How long a render that needs every frame exact waits for a readback.
const EXACT_WAIT: Duration = Duration::from_secs(2);

/// A readback that has not arrived after this long is given up: a lost device must not stop the
/// node for good.
const STALE: Duration = Duration::from_secs(5);

/// Whether a readback in `state`, started `age` ago, is given up. One that has arrived is taken
/// however old it is: the node may not have cooked for a while, and dropping it would cost a
/// second round for values that are already here.
pub(super) fn given_up(state: ReadState, age: Duration) -> bool {
    state != ReadState::Ready && age > STALE
}

/// Most points whose values become channels: four channels each stay under the channel limit.
pub const MAX_POINTS: u32 = 1000;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    const { assert!(MAX_POINTS <= MAX_FIRST) };
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Numbers about points as channels: how many there are (count) and may be (capacity), and an attribute's values at the first points and its mean, minimum and maximum over all of them. What the GPU keeps arrives a frame late",
        )
        .param(
            ParamSpec::text("attribute", "")
                .doc("An attribute to read, such as `position`, `color` or `age` (P, Color and other TouchDesigner names work). Empty: only count and capacity"),
        )
        .param(
            ParamSpec::int("points", 1)
                .clamp(0.0, f64::from(MAX_POINTS))
                .range(0.0, 64.0)
                .doc("With an attribute: how many of the first points give their values, a channel per point and component: tx0 ty0 tz0 for the first point's position, r0 g0 b0 a0 for its colour. At most 1000"),
        )
        .param(
            ParamSpec::bool("summary", true)
                .doc("With an attribute: its mean, minimum and maximum over all points, per component: tx_mean, tx_min, tx_max. Worked out on the GPU, so a million points cost little. As TouchDesigner's Analyze POP"),
        )
        .input("in", DataKind::Pts)
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`count`: the points there are. `capacity`: how many the buffers hold. With an attribute: its values and summary, one sample each. Components are named tx ty tz for position, nx ny nz for normal, r g b a for colour, u v for uv, <name>_x and so on for others; a one-number attribute by its name. An attribute name longer than 56 characters is cut to 56 in its channels. What comes from the GPU follows a frame or two late",
        ),
        || Box::new(ToChan::default()) as Box<dyn Operator>,
    ))
}

/// The most characters of an attribute's name its channels carry.
const BASE_LEN: usize = Name::MAX_LEN - 8;

/// The channel names of `name`'s components, a type `ty` (one name for a scalar).
pub fn component_names(name: &str, ty: AttrType) -> Vec<String> {
    let fixed: &[&str] = match (name, ty) {
        ("position", AttrType::Vec3) => &["tx", "ty", "tz"],
        ("normal", AttrType::Vec3) => &["nx", "ny", "nz"],
        ("color", AttrType::Vec4) => &["r", "g", "b", "a"],
        ("uv", AttrType::Vec2) => &["u", "v"],
        _ => &[],
    };
    if !fixed.is_empty() {
        return fixed.iter().map(|s| (*s).to_string()).collect();
    }
    // Room for the longest ending, `_x_mean` or `_x999`, within a channel name.
    let name = name.get(..BASE_LEN).unwrap_or(name);
    match ty.components() {
        1 => vec![name.to_string()],
        n => ["x", "y", "z", "w"].iter().take(n as usize).map(|c| format!("{name}_{c}")).collect(),
    }
}

/// What one readback is for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Asked {
    name: String,
    ty: AttrType,
    first: u32,
    summary: bool,
}

#[derive(Default)]
struct ToChan {
    /// The readback in flight, when it started, and what it reads: the header of a count kept
    /// on the GPU (`None`), or a probe of an attribute.
    reading: Option<(BufferRead, Instant, Option<Asked>)>,
    spare: Option<BufferRead>,
    /// The input changed after the readback in flight started: read again once it is in.
    again: bool,
    /// What went out last.
    sent: Option<Vec<Channel>>,
    probe: Probe,
}

fn channel(name: &str, v: f64) -> Result<Channel, Error> {
    Ok(Channel { name: Name::new(name)?, samples: vec![v] })
}

impl ToChan {
    fn out(&mut self, cx: &CookContext<'_>, channels: Vec<Channel>) -> Result<Cooked, CookError> {
        if self.sent.as_ref() == Some(&channels) && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.sent = Some(channels.clone());
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }

    /// Starts reading back what `asked` wants of `points` (the count alone for `None`); the
    /// node is woken when it arrives.
    fn start(
        &mut self,
        cx: &CookContext<'_>,
        points: &Points,
        asked: Option<Asked>,
    ) -> Result<(), CookError> {
        let gpu = gpu(cx)?;
        let buffer = match &asked {
            Some(a) => {
                self.probe.fill(gpu, points, Ask { name: &a.name, first: a.first, summary: a.summary })?
            }
            None => match points.count().header() {
                Some(h) => h.clone(),
                None => return Ok(()),
            },
        };
        let waker = cx.waker();
        let ready: Option<Arc<dyn Fn() + Send + Sync>> = Some(Arc::new(move || waker.wake()));
        let read = buffer.read_async_notify(gpu, self.spare.take(), ready)?;
        self.reading = Some((read, Instant::now(), asked));
        self.again = false;
        Ok(())
    }
}

/// What `name` reads in `points`: its canonical name and type. `Err` for an attribute the
/// points lack and penumbra does not know.
fn attribute(points: &Points, name: &str) -> Result<(String, AttrType), CookError> {
    let name = canonical(name).to_string();
    match (points.attr(&name), default_of(&name)) {
        (Some(a), _) => Ok((name, a.ty())),
        (None, Some(s)) => Ok((name, s.ty)),
        (None, None) => {
            let have: Vec<&str> = points.attrs().iter().map(|a| a.name()).collect();
            Err(CookError::failed(format!(
                "the points have no attribute `{name}`; they have {}",
                have.join(", ")
            )))
        }
    }
}

/// The channels of a probe's readback.
fn probed(asked: &Asked, p: &Probed, capacity: u32) -> Result<Vec<Channel>, Error> {
    let mut out = vec![channel("count", f64::from(p.count))?, channel("capacity", f64::from(capacity))?];
    let names = component_names(&asked.name, asked.ty);
    for (i, v) in p.first.iter().enumerate().take(asked.first as usize) {
        for (c, x) in names.iter().zip(v) {
            out.push(channel(&format!("{c}{i}"), f64::from(*x))?);
        }
    }
    if asked.summary {
        for (what, v) in [("mean", p.mean), ("min", p.min), ("max", p.max)] {
            for (c, x) in names.iter().zip(v) {
                out.push(channel(&format!("{c}_{what}"), f64::from(x))?);
            }
        }
    }
    Ok(out)
}

impl Operator for ToChan {
    fn gpu_bytes(&self) -> u64 {
        self.reading.as_ref().map_or(0, |(r, _, _)| r.bytes())
            + self.spare.as_ref().map_or(0, BufferRead::bytes)
            + self.probe.bytes()
    }

    fn trim(&mut self) {
        self.spare = None;
        self.probe.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        (self.sent.is_none() && self.reading.is_some()).then_some(JobStatus::Running { progress: None })
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let points = input(cx)?;
        let capacity = points.capacity();
        let wanted = cx.text("attribute")?.trim().to_string();
        let asked = if wanted.is_empty() {
            None
        } else {
            let (name, ty) = attribute(points, &wanted)?;
            let first = u32::try_from(cx.int("points")?.clamp(0, i64::from(MAX_POINTS))).unwrap_or(0);
            Some(Asked { name, ty, first, summary: cx.bool("summary")? })
        };
        if asked.is_none()
            && let Some(n) = points.count().known()
        {
            self.reading = None;
            return self
                .out(cx, vec![channel("count", f64::from(n))?, channel("capacity", f64::from(capacity))?]);
        }
        // A readback given up is read anew: the change it was to show (or one that came while it
        // was in flight) would otherwise never arrive.
        let stale =
            self.reading.as_ref().is_some_and(|(r, started, _)| given_up(r.state(), started.elapsed()));
        if stale {
            self.reading = None;
        }
        let changed =
            stale || cx.inputs_changed() || cx.params_changed() || cx.first_cook() || self.sent.is_none();
        match (&self.reading, changed) {
            (None, true) => self.start(cx, points, asked.clone())?,
            (Some(_), true) => self.again = true,
            _ => {}
        }
        if cx.exact_frames()
            && let Some((read, _, _)) = &self.reading
        {
            read.wait(gpu(cx)?, EXACT_WAIT);
        }
        let state = self.reading.as_ref().map(|(r, _, _)| r.state());
        match state {
            Some(ReadState::Ready) => {
                let Some((read, _, was)) = self.reading.take() else { return Ok(Cooked::Unchanged) };
                let channels = match &was {
                    None => {
                        let words = read.get::<u32>().map_err(CookError::from)?;
                        let count = words.first().copied().unwrap_or(0).min(points.count().at_most());
                        vec![channel("count", f64::from(count))?, channel("capacity", f64::from(capacity))?]
                    }
                    Some(a) => {
                        let values = read.get::<[f32; 4]>().map_err(CookError::from)?;
                        let p = Probed::of(&values)
                            .ok_or_else(|| CookError::failed("the readback is too short"))?;
                        probed(a, &p, capacity)?
                    }
                };
                self.spare = Some(read);
                // Read again for what changed meanwhile, or for what is asked now.
                if self.again || was != asked {
                    self.start(cx, points, asked)?;
                }
                self.out(cx, channels)
            }
            Some(ReadState::Failed) => {
                self.reading = None;
                Err(CookError::failed("the GPU could not read the points back"))
            }
            Some(ReadState::Waiting) | None => {
                if self.sent.is_some() {
                    Ok(Cooked::Unchanged)
                } else {
                    Err(CookError::Pending)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node that did not cook for a while finds its readback arrived long ago, and takes it.
    #[test]
    fn a_readback_that_arrived_is_taken_however_old() {
        let old = STALE + Duration::from_secs(60);
        assert!(!given_up(ReadState::Ready, old));
        assert!(given_up(ReadState::Waiting, old));
        assert!(given_up(ReadState::Failed, old));
        assert!(!given_up(ReadState::Waiting, STALE / 2));
    }

    /// An attribute name as long as names may be still gives channels: its first point, its
    /// thousandth and its summary.
    #[test]
    fn long_attribute_names_give_channels() {
        let name = "a".repeat(64);
        let p =
            Probed { count: 1000, mean: [1.0; 4], min: [0.0; 4], max: [2.0; 4], first: vec![[0.5; 4]; 1000] };
        for ty in [AttrType::F32, AttrType::Vec3] {
            let asked = Asked { name: name.clone(), ty, first: 1000, summary: true };
            let out = probed(&asked, &p, 1024).unwrap();
            let per = component_names(&name, ty).len();
            assert_eq!(out.len(), 2 + 1000 * per + 3 * per);
            let names: std::collections::HashSet<&str> = out.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names.len(), out.len(), "every channel has a name of its own");
        }
    }
}
