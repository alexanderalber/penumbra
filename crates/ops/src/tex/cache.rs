//! `tex.cache`: keeps the last frames of its input on the GPU and outputs one of them, `index`
//! frames back. While recording it takes a frame whenever the show time moves or the input
//! changes; copies stay on the GPU, capped by a memory budget.

use std::collections::VecDeque;

use penumbra_core::{Error, ParamSpec, Tick, Unit};
use penumbra_gpu::{Gpu, Tex};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, StateItem,
};

use super::{gpu, waiting};

const KIND: &str = "tex.cache";
/// Most bytes one cache holds (a 1080p frame is about 16 MB, a 4K frame 66 MB).
const BUDGET: u64 = 2 << 30;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(KIND, "The last frames of the input; outputs one of them, some frames back")
            .stateful()
            .param(
                ParamSpec::int("length", 30)
                    .clamp(1.0, 1000.0)
                    .range(1.0, 240.0)
                    .unit(Unit::Frames)
                    .doc("Frames kept (at most 2 GiB of them)"),
            )
            .param(
                ParamSpec::int("index", 0)
                    .clamp(0.0, 999.0)
                    .range(0.0, 239.0)
                    .unit(Unit::Frames)
                    .doc("How many frames back the output is; 0 is the newest"),
            )
            .param(ParamSpec::bool("record", true).doc("Take new frames; off holds what is kept"))
            .param(ParamSpec::bool("persist", true).more().doc(
                "While not recording, keep the held frames in the state snapshots, so they survive a restart",
            ))
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
        || {
            Box::new(Cache { frames: VecDeque::new(), tick: None, record: true, persist: true })
                as Box<dyn Operator>
        },
    ))
}

struct Cache {
    /// Newest first.
    frames: VecDeque<Tex>,
    tick: Option<Tick>,
    record: bool,
    persist: bool,
}

impl Cache {
    fn take(&mut self, gpu: &Gpu, src: &Tex, length: usize) -> Result<(), CookError> {
        let (w, h) = src.size();
        self.frames.retain(|t| t.size() == (w, h));
        self.frames.truncate(length);
        let slot = if self.frames.len() == length {
            match self.frames.pop_back() {
                Some(t) => t,
                None => Tex::new(gpu, w, h, KIND)?,
            }
        } else {
            Tex::new(gpu, w, h, KIND)?
        };
        gpu.copy(src, &slot)?;
        self.frames.push_front(slot);
        Ok(())
    }
}

impl Operator for Cache {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        self.record = cx.bool("record")?;
        self.persist = cx.bool("persist")?;
        let wanted = usize::try_from(cx.int("length")?).unwrap_or(1).max(1);
        let src = cx.input_as::<Tex>(0);
        let mut length = wanted;
        if let Some(src) = src {
            let fits = usize::try_from(BUDGET / src.bytes().max(1)).unwrap_or(usize::MAX).max(1);
            if fits < wanted {
                cx.warn(format!(
                    "{wanted} frames of {}x{} exceed 2 GiB; keeping {fits}",
                    src.width(),
                    src.height()
                ));
                length = fits;
            }
        }
        if self.record {
            let tick = cx.time().tick;
            let due = self.tick != Some(tick) || cx.inputs_changed() || self.frames.is_empty();
            self.tick = Some(tick);
            if due && let Some(src) = src {
                self.take(gpu, src, length)?;
            }
        }
        self.frames.truncate(length);
        let Some(last) = self.frames.len().checked_sub(1) else { return Err(waiting(cx)) };
        let index = usize::try_from(cx.int("index")?).unwrap_or(0).min(last);
        let frame = self.frames.get(index).cloned().ok_or_else(|| waiting(cx))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, frame)]))
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.record)
    }

    /// Only a held cache (not recording) is kept: a recording one refills by itself within
    /// `length` frames, and writing up to 2 GiB of it every snapshot would be a waste.
    fn save_state(&self) -> Option<Vec<StateItem>> {
        if self.record || !self.persist || self.frames.is_empty() {
            return None;
        }
        Some(self.frames.iter().map(|t| StateItem::Data(Data::new(DataKind::Tex, t.clone()))).collect())
    }

    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        let mut frames = VecDeque::with_capacity(items.len());
        for item in &items {
            match item {
                StateItem::Data(d) => match d.get::<Tex>() {
                    Some(t) => frames.push_back(t.clone()),
                    None => return Err("a cache keeps textures only".into()),
                },
                StateItem::Bytes(_) => return Err("a cache keeps textures only".into()),
            }
        }
        if frames.is_empty() {
            return Err("no frames".into());
        }
        self.frames = frames;
        Ok(())
    }
}
