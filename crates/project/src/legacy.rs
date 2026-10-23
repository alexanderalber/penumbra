//! What older builds kept beside the project file, taken into the project once as it opens.
//!
//! Before node positions were part of the project, the editor kept them in `layout.ron` beside
//! `project.ron`, keyed by path. A project none of whose nodes has a position takes them from
//! there as it opens ([`positions`]); the editor leaves them out the next time it writes its
//! layout file, which keeps only view state from then on.

use std::collections::BTreeMap;
use std::path::Path;

use penumbra_core::NodePath;
use penumbra_graph::{Network, Op};
use serde::Deserialize;

/// The editor's layout file in the project directory.
pub const LAYOUT_FILE: &str = "layout.ron";
/// A larger layout file is not read (positions of even a large project are a few hundred KiB).
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The part of an old `layout.ron` taken over; everything else in it is the editor's.
#[derive(Deserialize, Default)]
struct OldLayout {
    #[serde(default)]
    nodes: BTreeMap<String, (f32, f32)>,
}

fn read(dir: &Path) -> Option<OldLayout> {
    let file = dir.join(LAYOUT_FILE);
    let meta = std::fs::metadata(&file).ok()?;
    if meta.len() > MAX_BYTES {
        log::warn!(
            "{} is larger than {} MiB; its positions are not taken over",
            file.display(),
            MAX_BYTES >> 20
        );
        return None;
    }
    let text = std::fs::read_to_string(&file).ok()?;
    match ron::from_str::<OldLayout>(&text) {
        Ok(layout) => Some(layout),
        Err(e) => {
            log::warn!("{}: {e}; its positions are not taken over", file.display());
            None
        }
    }
}

/// The ops that give the nodes of `net` the positions the layout file in `dir` holds for them;
/// none when any node has a position already (the project was written by a build that keeps
/// them) or there is no such file. Positions that are no place are left out.
pub fn positions(dir: &Path, net: &Network) -> Vec<Op> {
    if net.is_empty() || net.nodes().any(|n| n.pos.is_some()) {
        return Vec::new();
    }
    let Some(old) = read(dir) else { return Vec::new() };
    old.nodes
        .into_iter()
        .filter_map(|(path, (x, y))| {
            let path: NodePath = path.parse().ok()?;
            net.resolve(&path)?;
            let pos = penumbra_graph::check_pos([x, y]).ok()?;
            Some(Op::Place { path, pos: Some(pos) })
        })
        .collect()
}
