//! A reader for FBX files, version 7.x, binary (little endian) and ASCII, in safe Rust.
//!
//! It reads meshes with their normals, UVs and materials, the model hierarchy with FBX's full
//! transform rules, animation takes with their layers and curves, and skins for linear blend
//! skinning. Its only dependency is `miniz_oxide`, for the zlib-compressed arrays.
//!
//! # Example
//!
//! Load a file, walk its meshes, pose the first take at half its length and skin the points:
//!
//! ```
//! use fbx_reader::transform::transform_point;
//! use fbx_reader::{Error, Limits, Scene};
//!
//! fn show(bytes: &[u8]) -> Result<(), Error> {
//!     let limits = Limits::default();
//!     let scene = Scene::parse(bytes, &limits)?;
//!     for warning in &scene.warnings {
//!         eprintln!("warning: {warning}");
//!     }
//!
//!     // The first take at half its length; a file without takes keeps its stored pose.
//!     let (take, time) = match scene.animation.takes.first() {
//!         Some(t) => (0, t.start + 0.5 * t.duration()),
//!         None => (0, 0.0),
//!     };
//!     let worlds = scene.animation.worlds(&scene.hierarchy, take, time);
//!
//!     for mesh in scene.mesh_instances() {
//!         let geometry = scene.geometry(mesh.geometry_id, &limits)?;
//!         let skin = scene.skin(mesh.geometry_id, &limits)?;
//!         let clusters = skin.as_ref().map(|s| s.cluster_matrices(&worlds)).unwrap_or_default();
//!         let rigid = worlds.get(mesh.node).map(|w| w.geometry);
//!
//!         let mut triangles = 0;
//!         for polygon in &geometry.polygons {
//!             for corners in geometry.triangulate(polygon) {
//!                 for corner in corners {
//!                     let corner = corner as usize;
//!                     let (Some(&point), Some(p)) =
//!                         (geometry.corners.get(corner), geometry.position(corner))
//!                     else {
//!                         continue;
//!                     };
//!                     // A skinned point follows its bones, any other point its model.
//!                     let to_world = skin
//!                         .as_ref()
//!                         .and_then(|s| s.point_matrix(point as usize, &clusters))
//!                         .or(rigid);
//!                     let _world = to_world.map(|m| transform_point(&m, p));
//!                 }
//!                 triangles += 1;
//!             }
//!         }
//!         println!("{}: {triangles} triangles, skinned: {}", mesh.name, skin.is_some());
//!     }
//!     Ok(())
//! }
//! # // The test scene comes from the `write` feature; without it the example is only compiled.
//! # #[cfg(feature = "write")]
//! # show(&fbx_reader::write::to_bytes(7400, &fbx_reader::write::skinned_scene(), true))?;
//! # Ok::<(), Error>(())
//! ```
//!
//! Positions come out in the file's units and axes. [`Scene::unit_meters`] gives the units,
//! [`Scene::axes`] and [`Axes::conversion_to`] the turn into your own axes.
//!
//! # Layers
//!
//! [`Scene`] is the entry point for most uses. Below it, each layer stays public for reading
//! what the crate does not interpret itself (cameras, lights, blend shapes, custom properties):
//!
//! 1. [`tree`]: the low-level node tree. Every record, value and array as stored, binary and
//!    ASCII read into the same types.
//! 2. [`Document`]: the objects by id, their connections, and their properties with the
//!    defaults from the property templates filled in.
//! 3. [`transform`]: a `Model`'s local transform from its property values, and the world
//!    transforms of the whole hierarchy ([`Hierarchy`]).
//! 4. [`geometry`]: a `Geometry` object's polygons, normals, texture coordinates and material
//!    indices, with FBX's mapping and reference modes resolved.
//! 5. [`anim`]: takes, layers and curves, and the models' transform properties at any time.
//! 6. [`skin`]: skins, their clusters and weights, and the matrices that move each control
//!    point with its bones.
//! 7. [`Scene`]: the meshes placed in the world, materials, textures, the animation, skins and
//!    the file's units and axes.
//!
//! Matrices are plain arrays ([`Mat4`]): row-major for column vectors, translation in the last
//! column.
//!
//! # Hostile files
//!
//! Every file is treated as hostile. Each count is checked against [`Limits`] before memory is
//! allocated for it, offsets use checked arithmetic, and nothing panics. A zlib array cannot
//! inflate beyond the size its header states, and that size is checked first. Callers that need
//! certainty against bugs should still run the reader inside `catch_unwind`.
//!
//! # Not read
//!
//! Big-endian files, versions below 7000 and FBX 6 are refused with an error that says so.
//! Blend shapes, geometry caches, cameras, lights and NURBS are not interpreted. Dual
//! quaternion skins are blended linearly, with a warning.
//!
//! # Features
//!
//! - `write`: the `write` module, which writes a node tree as a binary or ASCII FBX file and
//!   builds small test scenes. Meant for tests and tools. Off by default.
//!
//! # Credits
//!
//! The format knowledge follows ufbx (Samuli Raivio, MIT or Unlicense,
//! <https://github.com/ufbx/ufbx>) and fbxcel (YOSHIOKA Takuma, MIT or Apache-2.0,
//! <https://github.com/lo48576/fbxcel>). The transform composition and the handling of the
//! inherit types are ported from ufbx's `ufbxi_get_transform` and `ufbxi_update_node`, the
//! animation curves and layer blending from ufbx's curve reading and evaluation (see [`anim`]),
//! the skin clusters and their blending from ufbx's cluster reading and
//! `ufbx_get_skin_vertex_matrix` (see [`skin`]), all taken under the Unlicense. No code is
//! taken from fbxcel.

#![warn(missing_docs)]

pub mod anim;
mod ascii;
pub mod document;
pub mod geometry;
pub mod scene;
pub mod skin;
pub mod transform;
pub mod tree;
#[cfg(any(test, feature = "write"))]
pub mod write;

use std::fmt;

pub use anim::{Animation, Curve, Interpolation, Key, Layer, Take};
pub use document::{Connection, ConnectionKind, Document, Object, Properties, Property};
pub use geometry::{Attribute, Geometry, Polygon};
pub use scene::{Axes, Axis, MaterialInfo, MeshInstance, Scene, TextureInfo};
pub use skin::{Cluster, Influence, Skin, SkinningMethod};
pub use transform::{Hierarchy, HierarchyNode, Inherit, Mat4, NodeWorld, RotationOrder, TransformProps};
pub use tree::{Array, Node, Tree, Value};

/// What went wrong reading a file, in words. [`Display`](fmt::Display) gives the whole text,
/// for example `geometry 22 "Cube": PolygonVertexIndex: 9 is past the 8 vertices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    /// An error with `message`, for code that reads parts of a file the crate does not
    /// interpret and wants to report in the same type.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    /// The text of the error.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The same error with `context` in front.
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self { message: format!("{context}: {}", self.message) }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Budgets for reading one file. A file that would exceed one is refused with an error that
/// names it, before the memory for it is allocated.
///
/// Start from [`Limits::default`] and change the fields that matter; new budgets may be added
/// in later versions.
///
/// ```
/// let mut limits = fbx_reader::Limits::default();
/// limits.max_file_bytes = 16 << 20;
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
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
    /// Objects.
    pub max_objects: usize,
    /// Connections between objects.
    pub max_connections: usize,
    /// Control points of one geometry.
    pub max_vertices: usize,
    /// Polygon corners of one geometry.
    pub max_polygon_vertices: usize,
    /// Animation curves.
    pub max_curves: usize,
    /// Keys of all animation curves together.
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
