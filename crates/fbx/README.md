# fbx-reader

A reader for FBX 7.x files, binary and ASCII, in safe Rust. It reads meshes, materials,
the model hierarchy, animation and skins. Its only dependency is `miniz_oxide`, for the
zlib-compressed arrays.

```rust
use fbx_reader::{Limits, Scene};

let bytes = std::fs::read("character.fbx")?;
let limits = Limits::default();
let scene = Scene::parse(&bytes, &limits)?;

// Every model placed at 1.5 s into the first take.
let worlds = scene.animation.worlds(&scene.hierarchy, 0, 1.5);

for mesh in scene.mesh_instances() {
    let geometry = scene.geometry(mesh.geometry_id, &limits)?;
    if let Some(skin) = scene.skin(mesh.geometry_id, &limits)? {
        let clusters = skin.cluster_matrices(&worlds);
        // skin.point_matrix(point, &clusters): geometry space to the world for one point.
    }
}
```

The crate documentation has a complete example that triangulates, poses and skins each mesh.

## What it reads

- **Files:** FBX 7.x (file versions 7000 to 7700), binary little endian or ASCII.
  Both forms read into the same node tree, so everything above it works for both.
- **Meshes:** control points, polygons with ear-clipping triangulation, the first normal layer,
  the first UV set, material indices per polygon. All mapping and reference modes are resolved.
- **Materials:** diffuse colour and factor, opacity, the diffuse texture with its file names or
  embedded image bytes.
- **Hierarchy:** the full FBX transform (pivots, offsets, pre- and post-rotation, all rotation
  orders, geometric transforms) and the three inherit types.
- **Units and axes:** `UnitScaleFactor` and the file's axes, with a matrix into any other axes.
- **Animation:** takes, layers (additive, override, override passthrough, animated weights,
  quaternion or Euler rotation blending), curves with constant, linear and cubic keys (auto,
  TCB, user and weighted tangents) and all extrapolation modes.
- **Skins:** clusters with their weights and bind matrices, the bind pose as fallback, and per
  point the blended matrix for linear blend skinning.

The node tree and the object layer stay public. Anything the crate does not interpret itself
can still be read from there: cameras, lights, blend shapes, custom properties.

## What it does not read

- FBX 6 and older, and big-endian files. Each is refused with an error that says so.
- Blend shapes, geometry caches, cameras, lights, NURBS, LOD groups, visibility.
- Vertex colours, tangents, UV sets after the first, more than one texture per material, PBR
  material properties.
- Dual quaternion skins: they are blended linearly, and the skin carries a warning.

## Guarantees

- **No panics:** no `unwrap`, `expect`, `panic!` or indexing that can fail. Every error comes
  back as `Result` with a text that names the record or object.
- **No unsafe code.**
- **Budgets:** `Limits` caps file size, decoded array bytes, records, values, nesting depth,
  objects, connections, vertices, polygon corners, curves, keys and skin weights. Each count
  is checked before the memory for it is allocated. A zlib array cannot inflate beyond the size
  its header states. A file that claims 4 GiB of array data costs nothing to refuse.
- **Mutation-tested:** about 19,000 mutated copies of real files (truncations and random byte
  changes, binary and ASCII) and 240,000 fuzz cases through a mesh loader built on the crate,
  0 panics.

## Validation against ufbx

The test files of [ufbx](https://github.com/ufbx/ufbx) were read by both libraries, and the
results compared with ufbx 0.11.5:

| What | Files | Agree | Differences |
|---|---|---|---|
| Meshes, binary | 191 | 188, 2 refused by both | 1: an edge-only mesh, whose zero-area triangles this crate drops |
| Meshes, ASCII 7.x | 281 | 271, 7 refused by both | 3: zero-area triangles; a model chain 10,000 deep (refused here, `max_depth` 64); a parent cycle (cut here with a warning, refused by ufbx) |
| Animated node matrices, binary | 50 | 50 | worst difference 1.7e-15 |
| Animated node matrices, ASCII | 103 | 103 | worst difference 2.4e-7 |
| Skinned vertices, binary | 16 | 11, 3 still in both | 2: blend shapes and dual quaternion skinning, which this crate leaves out |
| Skinned vertices, ASCII | 21 | 12, 6 still in both | 3: blend shapes and dual quaternion skinning |

Animation was compared at 9 times per take, including times before and after the take.
Skinned vertices agree to 2.8e-7 of the scene size where both libraries do the same thing.

## Features

- `write`: the `write` module. It writes a node tree as a binary or ASCII FBX file and builds
  small test scenes. Meant for tests and tools. Off by default.

Matrices are plain `[[f64; 4]; 4]` arrays, row-major for column vectors, translation in the
last column. `mint::RowMatrix4::from(m)` and `glam::DMat4::from_cols_array_2d(&m).transpose()`
take them without a feature of this crate.

## Credits

- [ufbx](https://github.com/ufbx/ufbx) by Samuli Raivio (MIT or Unlicense) is the map of the
  format and the test oracle. The transform composition, the inherit types, curve reading and
  evaluation, layer blending and skin cluster reading are ported from ufbx under the Unlicense.
- [fbxcel](https://github.com/lo48576/fbxcel) by YOSHIOKA Takuma (MIT or Apache-2.0) served as
  a reference for the binary format. No code is taken from it.

The crate is part of [penumbra](https://github.com/alexanderalber/penumbra) and is developed
there.

## Licence

MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
