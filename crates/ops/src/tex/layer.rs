//! `tex.layer`: one layer of a Photoshop file (PSD, PSB) or of a layer directory (a directory of
//! images, one layer each, bottom first by file name), or the whole picture. Opened and decoded
//! off the cook thread; the parsed file is kept, so picking another layer only decodes that
//! layer. Reloads when the file (or the directory's list) changes.

use std::path::PathBuf;
use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Table};
use penumbra_gpu::Tex;
use penumbra_gpu::layers::Source;
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.layer",
            "One layer of a Photoshop file (PSD/PSB) or of a folder of images, or the whole picture; updates when the file changes",
        )
        .param(
            ParamSpec::file("file", &["psd", "psb"])
                .doc("A .psd or .psb, or a directory of images (layers bottom first by name); relative to the project"),
        )
        .param(ParamSpec::text("layer", "").doc(
            "A layer's name, its path (Group/Layer) or #index from the layers table; empty: the whole picture",
        ))
        .param(
            ParamSpec::bool("placed", true)
                .more()
                .doc("The layer at its place on the canvas, at the canvas's size; off: only the layer's own rectangle"),
        )
        .output("out", DataKind::Tex)
        .output("layers", DataKind::Table)
        .describe_output("out", "The chosen layer, or the whole picture")
        .describe_output(
            "layers",
            "Every layer, bottom first: index, name, path, group, visible, opacity, blend, left, top, width, height",
        ),
        || Box::new(Layer::default()) as Box<dyn Operator>,
    ))
}

struct Loaded {
    source: Arc<Source>,
    path: PathBuf,
    tex: Tex,
    table: Table,
    warning: Option<String>,
}

#[derive(Default)]
struct Layer {
    slot: AsyncSlot<Arc<Loaded>>,
    watch: Option<FileWatch>,
    /// What the job in flight or the last result was asked for.
    asked: Option<(PathBuf, String, bool)>,
}

impl Operator for Layer {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        // Before anything is remembered: a cook before the GPU arrives must not count as the
        // load started, or the file never loads once it is there.
        let gpu = super::gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let layer = cx.text("layer")?.trim().to_string();
        let placed = cx.bool("placed")?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        if moved {
            self.watch = Some(cx.watch_file(&path));
        }
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        let key = (path.clone(), layer.clone(), placed);
        if moved || changed || self.asked.as_ref() != Some(&key) {
            self.asked = Some(key);
            let gpu = gpu.clone();
            // The parsed file is reused unless the file changed or another one is named.
            let keep = self.slot.latest().filter(|r| !changed && r.path == path).map(|r| r.source.clone());
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| {
                let max = gpu.max_texture_size();
                let source = match keep {
                    Some(s) => s,
                    None => Arc::new(Source::open(&p, max).map_err(|e| e.0)?),
                };
                let (decoded, warning) = if layer.is_empty() {
                    source.merged(max).map_err(|e| e.0)?
                } else {
                    let info = source.find(&layer).map_err(|e| e.0)?;
                    (source.layer(info, placed, max).map_err(|e| e.0)?, None)
                };
                let tex = gpu.upload(&decoded, &p.display().to_string()).map_err(|e| e.0)?;
                let table = table(&source).map_err(|e| e.0)?;
                Ok(Arc::new(Loaded { source, path: p, tex, table, warning }))
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        let Some(r) = self.slot.latest() else { return Err(CookError::Pending) };
        if let Some(w) = &r.warning {
            cx.warn(w.clone());
        }
        if fresh {
            Ok(Cooked::Outputs(vec![
                Data::new(DataKind::Tex, r.tex.clone()),
                Data::new(DataKind::Table, r.table.clone()),
            ]))
        } else {
            Ok(Cooked::Unchanged)
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

/// The `layers` table: a header, then the layers bottom first.
fn table(source: &Source) -> Result<Table, Error> {
    let mut rows = vec![
        ["index", "name", "path", "group", "visible", "opacity", "blend", "left", "top", "width", "height"]
            .map(str::to_string)
            .to_vec(),
    ];
    for l in source.layers() {
        let mut row = vec![
            l.index.to_string(),
            l.name.clone(),
            l.path.clone(),
            u8::from(l.group).to_string(),
            u8::from(l.visible).to_string(),
            format!("{:.3}", l.opacity),
            l.blend.clone(),
        ];
        row.extend(l.rect.iter().map(i64::to_string));
        rows.push(row);
    }
    Table::new(rows)
}
