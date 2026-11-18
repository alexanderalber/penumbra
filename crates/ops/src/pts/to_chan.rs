//! `chan.from_pts`: numbers about points as channels, so a network can react to them: `count`,
//! how many points there are, and `capacity`, how many there may be.
//!
//! A count the CPU knows comes out at once. A count on the GPU (after `pts.delete`) is read back
//! without the cook waiting for it: the channel follows a frame or two late, and keeps its last
//! value until the next one arrives. A render that needs every frame exact waits for it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{Channel, Chans, Error, Name};
use penumbra_gpu::{BufferRead, Points, ReadState};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::input;
use crate::gpu;

pub const KIND: &str = "chan.from_pts";

/// How long a render that needs every frame exact waits for the count.
const EXACT_WAIT: Duration = Duration::from_secs(2);

/// A readback older than this is given up: a lost device must not stop the node for good.
const STALE: Duration = Duration::from_secs(5);

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Numbers about points as channels: how many there are (count) and how many there may be (capacity). A count the GPU keeps, as after pts.delete, arrives a frame late",
        )
        .input("in", DataKind::Pts)
        .output("out", DataKind::Chan)
        .describe_output(
            "out",
            "`count`: the points there are. `capacity`: how many the buffers hold. A count kept on the GPU follows a frame or two late",
        ),
        || Box::new(ToChan::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct ToChan {
    /// The readback in flight and when it started.
    reading: Option<(BufferRead, Instant)>,
    spare: Option<BufferRead>,
    /// The input changed after the readback in flight started: read again once it is in.
    again: bool,
    /// The last count and capacity sent out.
    sent: Option<(u32, u32)>,
}

impl ToChan {
    fn out(&mut self, cx: &CookContext<'_>, count: u32, capacity: u32) -> Result<Cooked, CookError> {
        if self.sent == Some((count, capacity)) && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        self.sent = Some((count, capacity));
        let channel = |name: &str, v: u32| -> Result<Channel, Error> {
            Ok(Channel { name: Name::new(name)?, samples: vec![f64::from(v)] })
        };
        let chans =
            Chans::new(cx.rate().as_f64(), vec![channel("count", count)?, channel("capacity", capacity)?])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }

    /// Starts reading the count of `points` back; the node is woken when it arrives.
    fn start(&mut self, cx: &CookContext<'_>, points: &Points) -> Result<(), CookError> {
        let gpu = gpu(cx)?;
        let Some(header) = points.count().header().cloned() else { return Ok(()) };
        let waker = cx.waker();
        let ready: Option<Arc<dyn Fn() + Send + Sync>> = Some(Arc::new(move || waker.wake()));
        let read = header.read_async_notify(gpu, self.spare.take(), ready)?;
        self.reading = Some((read, Instant::now()));
        self.again = false;
        Ok(())
    }
}

impl Operator for ToChan {
    fn gpu_bytes(&self) -> u64 {
        self.reading.as_ref().map_or(0, |(r, _)| r.bytes()) + self.spare.as_ref().map_or(0, BufferRead::bytes)
    }

    fn trim(&mut self) {
        self.spare = None;
    }

    fn job_status(&self) -> Option<JobStatus> {
        (self.sent.is_none() && self.reading.is_some()).then_some(JobStatus::Running { progress: None })
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let points = input(cx)?;
        let capacity = points.capacity();
        if let Some(n) = points.count().known() {
            self.reading = None;
            return self.out(cx, n, capacity);
        }
        if self.reading.as_ref().is_some_and(|(_, started)| started.elapsed() > STALE) {
            self.reading = None;
        }
        let changed = cx.inputs_changed() || cx.first_cook() || self.sent.is_none();
        match (&self.reading, changed) {
            (None, true) => self.start(cx, points)?,
            (Some(_), true) => self.again = true,
            _ => {}
        }
        if cx.exact_frames()
            && let Some((read, _)) = &self.reading
        {
            read.wait(gpu(cx)?, EXACT_WAIT);
        }
        let state = self.reading.as_ref().map(|(r, _)| r.state());
        match state {
            Some(ReadState::Ready) => {
                let Some((read, _)) = self.reading.take() else { return Ok(Cooked::Unchanged) };
                let words = read.get::<u32>().map_err(CookError::from)?;
                let count = words.first().copied().unwrap_or(0).min(points.count().at_most());
                self.spare = Some(read);
                if self.again {
                    self.start(cx, points)?;
                }
                self.out(cx, count, capacity)
            }
            Some(ReadState::Failed) => {
                self.reading = None;
                Err(CookError::failed("the GPU could not read the count back"))
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
