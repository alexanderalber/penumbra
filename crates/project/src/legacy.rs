//! What older builds kept beside the project file, taken into the project once as it opens.
//!
//! Before node positions and the thumbnail mode were part of the project, the editor kept them
//! in `layout.ron` beside `project.ron` (positions keyed by path, the thumbnail mode and rate
//! among its options). A project takes them from there as it opens ([`take_over`]): the
//! positions when none of its nodes has one, the thumbnail settings when it has none of its
//! own. The editor leaves them out the next time it writes its layout file, which keeps only
//! view state from then on.

use std::collections::BTreeMap;
use std::path::Path;

use penumbra_core::{Name, NodePath};
use penumbra_graph::{Network, Op, THUMBNAIL_EVERY, THUMBNAILS, ThumbnailMode};
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
    #[serde(default)]
    options: Option<OldOptions>,
}

/// The editor's options as older builds wrote them (plain values, no `Some`).
#[derive(Deserialize, Default)]
struct OldOptions {
    #[serde(default, deserialize_with = "present")]
    thumbnails: Option<ThumbnailMode>,
    #[serde(default, deserialize_with = "present")]
    thumbnail_every: Option<u32>,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
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

/// The ops that take over what the layout file in `dir` holds that is the project's now: node
/// positions when no node of `net` has one, the thumbnail mode and rate when `net` has no
/// thumbnail setting. None when there is no such file. Values that are no position or no
/// setting are left out.
pub fn take_over(dir: &Path, net: &Network) -> Vec<Op> {
    let Some(old) = read(dir) else { return Vec::new() };
    let mut ops = Vec::new();
    if !net.is_empty() && net.nodes().all(|n| n.pos.is_none()) {
        ops.extend(old.nodes.into_iter().filter_map(|(path, (x, y))| {
            let path: NodePath = path.parse().ok()?;
            net.resolve(&path)?;
            let pos = penumbra_graph::check_pos([x, y]).ok()?;
            Some(Op::Place { path, pos: Some(pos) })
        }));
    }
    let own = [THUMBNAILS, THUMBNAIL_EVERY].iter().any(|n| net.settings().contains_key(*n));
    if let Some(options) = old.options.filter(|_| !own) {
        let settings = [
            (THUMBNAILS, options.thumbnails.map(|m| penumbra_core::Value::Text(m.key().into()))),
            (THUMBNAIL_EVERY, options.thumbnail_every.map(|e| penumbra_core::Value::Int(i64::from(e)))),
        ];
        for (name, value) in settings {
            let (Ok(name), Some(value)) = (Name::new(name), value) else { continue };
            // Only what is a value of the setting, and differs from its default.
            if penumbra_graph::check_setting(&name, Some(&value)).is_ok_and(|v| v.is_some()) {
                ops.push(Op::Setting { name, value: Some(value) });
            }
        }
    }
    ops
}
