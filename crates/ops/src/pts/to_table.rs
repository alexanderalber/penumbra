//! `table.from_pts`: the first points as rows of a table, a column per component of each
//! attribute, for looking at what points hold (TouchDesigner's POP to DAT).
//!
//! The first rows of every attribute are copied on the GPU into one buffer and read back without
//! the cook waiting: the table follows a frame or two late and keeps its last rows meanwhile.
//! A render that needs every frame exact waits for them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::table::MAX_CELLS;
use penumbra_core::{Curve, Error, ParamSpec, Table};
use penumbra_gpu::points::probe::first_rows;
use penumbra_gpu::points::{AttrType, HEADER_BYTES, canonical};
use penumbra_gpu::{BufferRead, Points, ReadState};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::input;
use super::to_chan::component_names;
use crate::gpu;

pub const KIND: &str = "table.from_pts";

const EXACT_WAIT: Duration = Duration::from_secs(2);
const STALE: Duration = Duration::from_secs(5);

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "The first points as rows of a table, a column per number of each attribute: to look at what the points hold. Read back from the GPU a frame late",
        )
        .param(
            ParamSpec::int("points", 100)
                .clamp(1.0, MAX_CELLS as f64)
                .range(1.0, 10_000.0)
                .curve(Curve::Log)
                .doc("How many of the first points become rows. A table holds at most 1 048 576 cells, so with many columns fewer rows fit"),
        )
        .param(ParamSpec::text("attributes", "").doc(
            "The attributes to show, separated by spaces (TouchDesigner names work). Empty: all of them",
        ))
        .input("in", DataKind::Pts)
        .output("out", DataKind::Table)
        .describe_output(
            "out",
            "The first row names the columns: tx ty tz for position, nx ny nz for normal, r g b a for colour, u v for uv, <name>_x and so on for others, a one-number attribute by its name. Then one row per point, in order. pts.from_table reads it back into the same attributes, except one-number attributes named like a component (x, r, u, alpha, index, a name ending in _x or _0)",
        ),
        || Box::new(ToTable::default()) as Box<dyn Operator>,
    ))
}

/// What one readback holds: the rows asked for and the attributes in the buffer's order.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    rows: u32,
    attrs: Vec<(String, AttrType)>,
    /// The count as the CPU knows it; `None`: read from the header at the buffer's start.
    known: Option<u32>,
}

#[derive(Default)]
struct ToTable {
    reading: Option<(BufferRead, Instant, Layout)>,
    again: bool,
    sent: Option<Table>,
}

impl ToTable {
    fn start(&mut self, cx: &CookContext<'_>, points: &Points, layout: Layout) -> Result<(), CookError> {
        let gpu = gpu(cx)?;
        let names: Vec<&str> = layout.attrs.iter().map(|(n, _)| n.as_str()).collect();
        let staging = first_rows(gpu, points, &names, layout.rows)?;
        let waker = cx.waker();
        let ready: Option<Arc<dyn Fn() + Send + Sync>> = Some(Arc::new(move || waker.wake()));
        let read = staging.read_async_notify(gpu, None, ready)?;
        self.reading = Some((read, Instant::now(), layout));
        self.again = false;
        Ok(())
    }
}

/// The table a readback of `layout` gives.
fn table_of(layout: &Layout, words: &[u32]) -> Result<Table, Error> {
    let count = layout.known.unwrap_or_else(|| words.first().copied().unwrap_or(0));
    let rows = layout.rows.min(count) as usize;
    let mut header = Vec::new();
    for (name, ty) in &layout.attrs {
        header.extend(component_names(name, *ty));
    }
    let mut table = vec![header];
    let mut columns: Vec<(&[u32], AttrType)> = Vec::new();
    let mut at = (HEADER_BYTES / 4) as usize;
    for (_, ty) in &layout.attrs {
        let len = layout.rows as usize * (ty.stride() / 4) as usize;
        columns.push((words.get(at..at + len).unwrap_or(&[]), *ty));
        at += len;
    }
    for i in 0..rows {
        let mut row = Vec::new();
        for (w, ty) in &columns {
            let per = (ty.stride() / 4) as usize;
            let el = w.get(i * per..(i + 1) * per).unwrap_or(&[]);
            for c in 0..ty.components() as usize {
                let word = el.get(c).copied().unwrap_or(0);
                row.push(match ty {
                    AttrType::U32 => word.to_string(),
                    AttrType::I32 => (word as i32).to_string(),
                    _ => f32::from_bits(word).to_string(),
                });
            }
        }
        table.push(row);
    }
    Table::new(table)
}

impl Operator for ToTable {
    fn gpu_bytes(&self) -> u64 {
        self.reading.as_ref().map_or(0, |(r, _, _)| r.bytes())
    }

    fn job_status(&self) -> Option<JobStatus> {
        (self.sent.is_none() && self.reading.is_some()).then_some(JobStatus::Running { progress: None })
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let points = input(cx)?;
        let wanted = cx.text("attributes")?;
        let attrs: Vec<(String, AttrType)> = if wanted.trim().is_empty() {
            points.attrs().iter().map(|a| (a.name().to_string(), a.ty())).collect()
        } else {
            let mut out = Vec::new();
            for w in wanted.split_whitespace() {
                let name = canonical(w);
                let a = points.attr(name).ok_or_else(|| {
                    let have: Vec<&str> = points.attrs().iter().map(|a| a.name()).collect();
                    CookError::failed(format!(
                        "the points have no attribute `{name}`; they have {}",
                        have.join(", ")
                    ))
                })?;
                if !out.iter().any(|(n, _): &(String, AttrType)| n == a.name()) {
                    out.push((a.name().to_string(), a.ty()));
                }
            }
            out
        };
        let columns: usize = attrs.iter().map(|(_, ty)| ty.components() as usize).sum::<usize>().max(1);
        let fit = (MAX_CELLS / columns).saturating_sub(1);
        let asked = usize::try_from(cx.int("points")?.max(1)).unwrap_or(1);
        if asked > fit {
            cx.warn(format!("{columns} columns leave room for {fit} rows in a table"));
        }
        let rows = asked.min(fit).min(points.count().at_most() as usize);
        let layout = Layout { rows: u32::try_from(rows).unwrap_or(0), attrs, known: points.count().known() };

        if self.reading.as_ref().is_some_and(|(_, started, _)| started.elapsed() > STALE) {
            self.reading = None;
        }
        let changed = cx.inputs_changed() || cx.params_changed() || cx.first_cook() || self.sent.is_none();
        match (&self.reading, changed) {
            (None, true) => self.start(cx, points, layout.clone())?,
            (Some(_), true) => self.again = true,
            _ => {}
        }
        if cx.exact_frames()
            && let Some((read, _, _)) = &self.reading
        {
            read.wait(gpu(cx)?, EXACT_WAIT);
        }
        match self.reading.as_ref().map(|(r, _, _)| r.state()) {
            Some(ReadState::Ready) => {
                let Some((read, _, was)) = self.reading.take() else { return Ok(Cooked::Unchanged) };
                let words = read.get::<u32>().map_err(CookError::from)?;
                let table = table_of(&was, &words)?;
                if self.again || was != layout {
                    self.start(cx, points, layout)?;
                }
                if self.sent.as_ref() == Some(&table) && !cx.first_cook() {
                    return Ok(Cooked::Unchanged);
                }
                self.sent = Some(table.clone());
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
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
