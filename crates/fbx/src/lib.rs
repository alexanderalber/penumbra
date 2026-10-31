//! A reader for binary FBX files, version 7.x, little endian, in safe Rust.
//!
//! The reader is built in layers, and each layer stays public, so that animation and skinning
//! can be added on top without reparsing:
//!
//! 1. [`tree`]: the binary node tree. Every record, value and array as stored in the file.
//! 2. [`Document`]: the objects by id, their connections, and their properties with the
//!    defaults from the property templates filled in.
//! 3. [`transform`]: a `Model`'s local transform as a function of its property values, and the
//!    world transforms of the whole hierarchy ([`Hierarchy`]). Animated values can be fed into
//!    the same functions per frame.
//! 4. [`geometry`]: a `Geometry` object's polygons, normals, texture coordinates and material
//!    indices, with FBX's mapping and reference modes resolved.
//! 5. [`anim`]: takes, layers and curves, and the models' transform properties at any time,
//!    which [`Hierarchy::evaluate`] turns into world transforms per frame.
//! 6. [`skin`]: skins, their clusters and weights, and the matrices that move each control
//!    point with its bones at any time.
//! 7. [`Scene`]: the meshes placed in the world, materials, textures, the animation, skins and
//!    the file's units and axes. Most users start here.
//!
//! Every file is treated as hostile. Each count is checked against [`Limits`] before memory is
//! allocated for it, offsets use checked arithmetic, and nothing panics. A zlib array cannot
//! inflate beyond the size its header states, and that size is checked first. Callers that need
//! certainty against bugs should still run the reader inside `catch_unwind`.
//!
//! Not read: ASCII FBX, big-endian files and versions below 7000 (each is refused with an
//! error that says so), blend shapes, geometry caches, cameras, lights, NURBS. Dual quaternion
//! skins are blended linearly, with a warning.
//!
//! The format knowledge follows ufbx (Samuli Raivio, MIT or Unlicense,
//! <https://github.com/ufbx/ufbx>) and fbxcel (YOSHIOKA Takuma, MIT or Apache-2.0,
//! <https://github.com/lo48576/fbxcel>). The transform composition and the handling of the
//! inherit types are ported from ufbx's `ufbxi_get_transform` and `ufbxi_update_node`, the
//! animation curves and layer blending from ufbx's curve reading and evaluation (see [`anim`]),
//! the skin clusters and their blending from ufbx's cluster reading and
//! `ufbx_get_skin_vertex_matrix` (see [`skin`]), all taken under the Unlicense. No code is taken from fbxcel.

pub mod anim;
pub mod document;
pub mod geometry;
pub mod scene;
pub mod skin;
pub mod transform;
pub mod tree;
pub mod write;

use std::fmt;

pub use anim::{Animation, Curve, Interpolation, Key, Layer, Take};
pub use document::{Connection, ConnectionKind, Document, Object, Properties, Property};
pub use geometry::{Attribute, Geometry, Polygon};
pub use scene::{Axes, Axis, MaterialInfo, MeshInstance, Scene, TextureInfo};
pub use skin::{Cluster, Influence, Skin, SkinningMethod};
pub use transform::{Hierarchy, Inherit, Mat4, NodeWorld, RotationOrder, TransformProps};
pub use tree::{Array, Node, Value};

/// What went wrong reading a file, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// The same error with `context` in front.
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self(format!("{context}: {}", self.0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Budgets for reading one file. A file that would exceed one is refused with an error that
/// names it, before the memory for it is allocated.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Largest file.
    pub max_file_bytes: u64,
    /// All arrays together after inflating, and so also the largest single array.
    pub max_array_bytes: u64,
    /// Records in the node tree.
    pub max_nodes: usize,
    /// Values in all records together (each costs a few dozen bytes in memory, but only two
    /// bytes in the file).
    pub max_values: usize,
    /// Deepest nesting of records, and of models under models.
    pub max_depth: usize,
    /// Objects and connections.
    pub max_objects: usize,
    pub max_connections: usize,
    /// Control points of one geometry, and its polygon corners.
    pub max_vertices: usize,
    pub max_polygon_vertices: usize,
    /// Animation curves, and their keys together.
    pub max_curves: usize,
    pub max_keys: usize,
    /// Weights of one skin, all its clusters together.
    pub max_skin_weights: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 << 20,
            max_array_bytes: 1 << 30,
            max_nodes: 2 << 20,
            max_values: 8 << 20,
            max_depth: 64,
            max_objects: 1 << 20,
            max_connections: 4 << 20,
            max_vertices: 16 << 20,
            max_polygon_vertices: 32 << 20,
            max_curves: 1 << 20,
            max_keys: 16 << 20,
            max_skin_weights: 32 << 20,
        }
    }
}

/// The 22 bytes every binary FBX file starts with: the magic text, a zero and 0x1a. The byte
/// after them is 0 for little endian, then comes the version as a 32-bit number.
pub const MAGIC: &[u8] = b"Kaydara FBX Binary  \x00\x1a";

/// Whether `bytes` start like a binary FBX file.
pub fn is_binary_fbx(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Whether `bytes` look like an ASCII FBX file (a comment header or a `FBXHeaderExtension`
/// record near the start).
pub fn is_ascii_fbx(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(bytes.get(..bytes.len().min(1024)).unwrap_or(&[]));
    let t = head.trim_start_matches('\u{feff}').trim_start();
    t.starts_with("; FBX") || head.contains("FBXHeaderExtension:")
}

#[cfg(test)]
mod tests;
