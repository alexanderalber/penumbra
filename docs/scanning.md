# Scanning: projecting onto objects

A camera films the projectors' structured light patterns; from those pictures penumbra learns
which camera pixel each projector pixel lands on. That is enough to find objects standing in
front of a wall, to give each projector a mask and a blend weight for them, and to draw content
once, in the camera's picture, so it lands on every object the same way in every projector.

Everything here is a command (see [Control](control.md)); the results are plain files in the
project, read by `tex.image`. Nothing of it runs per frame: during the show what remains is a
`tex.remap` lookup and a multiply per projector.

Paths in the commands are relative to the project directory.

## 1. Scan: `scan.patterns` and `scan.decode`

Per projector: a `tex.scan_pattern` into the projector, stepped through its `pattern` 0, 1, 2,
... (`scan.patterns` lists them, 46 at 1920 x 1080), the camera taking one picture of each into
a folder. The room dark, camera exposure fixed and not clipping the white pattern, nothing
moving. Then

    penumbra-cli ctl scan.decode '{"frames": "scan/a"}'

writes into `scan/a/decoded/`:

| file | what |
|---|---|
| `cam_to_proj.exr` | camera size; per camera pixel the projector position (red, green, 0 to 1), -1 where none |
| `proj_to_cam.exr` | projector size; per projector pixel the camera position, blue 1 where measured, 0.5 where filled in from a neighbour, -1 where none |
| `scene.png` | the scene as the projector sees it |
| `quality.png` | per camera pixel what was decoded (grey by confidence; black: not reached; magenta: clipped; blue: ambiguous; red: outliers) |

`proj_to_cam.exr` through `tex.image` into `tex.remap`'s `map`, with the camera's picture as
`in`: the camera's view in the projector's pixels.

## 2. Objects: `scan.objects`

Scan every projector twice: once of the empty scene (the wall alone, the **reference**), once
with the objects in place. Camera and projectors must not move in between.

    penumbra-cli ctl scan.objects '{"projectors": [
        {"reference": "scan/wall_a/decoded", "scan": "scan/a/decoded"},
        {"reference": "scan/wall_b/decoded", "scan": "scan/b/decoded"}]}'

Because the camera sits beside each projector's lens, a projector pixel landing on something
nearer than the wall shows up somewhere else in the camera picture: the parallax. On the wall it
moves by a few camera pixels of noise, on an object by tens to hundreds. Where it moves more
than `threshold` (camera pixels, 25) is the objects' silhouette, cleaned up and with pieces
smaller than `min_area` (projector pixels, 1/256 of the projector) dropped. Where the parallax
jumps by more than `split` camera pixels per projector pixel (2), one object ends and another
at another depth begins: the silhouette is cut there into parts, which then grow back over the
cut. Pixels near the silhouette's edge (closer than `rim`, 25 projector pixels) that were only
filled in by the decode (no measured pixel within `unmeasured`, 4 projector pixels) are dropped:
that light would land on the wall behind, or on something too dark to decode.

Parts of different projectors that cover the same camera pixels, or that continue each other
past a projector's frame edge, are one object. Objects are numbered by their height in the
camera picture, top first, so object 1 is the same thing in every projector.

Written into `out` (default `objects/`), `N` the projector's place in the list:

| file | what |
|---|---|
| `labels_N.png` | 8-bit, projector size: 0 background, `k` object `k` |
| `parts_N.png` | the same to look at: objects in colour, cuts darker, dropped pixels dark red |
| `objects_camera.png` | 8-bit, camera size: the objects in the camera's picture, every labelled projector pixel (shrunk by `erode`, 2 projector pixels, since silhouette edges are half wall) carried over and closed |
| `objects_camera_view.png` | the same to look at |

The reply lists per object its pixels in each projector (an object seen by one projector only
is fine) and in the camera.

Limits: black or very dark surfaces do not decode, so they are neither found nor lit. Faces of
one object at an angle to each other usually stay one part.

## 3. Blend: `scan.blend`

Where two projectors light the same spot it is twice as bright, and any misregistration
doubles. `scan.blend` gives each projector a weight: where another projector also lands on the
same camera pixel (inside its mask), the weight falls towards the projector's own frame edge,
`d_self / (d_self + d_other)` with `d` the distance to the frame edge capped at `ramp`
(projector pixels, 200); elsewhere 1; blurred by `blur` (3), 0 outside the mask. The weights
of all projectors at one camera pixel add up to 1; the reply says how closely.

    penumbra-cli ctl scan.blend '{"scans": ["scan/a/decoded", "scan/b/decoded"],
        "masks": ["objects/labels_0.png", "objects/labels_1.png"]}'

`masks` is optional (one file or `null` per scan); without it the whole frame counts. Also
written per projector is `proj_to_cam_smooth_N.exr`: the camera is usually coarser than the
projector, so only some projector pixels have a measured camera position and content remapped
through the raw map has jagged edges. The smoothed map averages the measured positions with a
Gaussian (`sigma`, 6 projector pixels) within each object (or the decoded area without masks).

Written into `out` (default `objects/`):

| file | what |
|---|---|
| `weight_N.png` | 16-bit, projector size; stored sRGB-encoded so that `tex.image` gives back the weight itself |
| `proj_to_cam_smooth_N.exr` | the projector's `proj_to_cam` map, smoothed |

## 4. The network

Content is made once, in the camera's picture, and each projector takes its share:

1. `tex.shapes` with the camera's size as `width` and `height`; a `tex.image` of
   `objects/objects_camera.png` beside it, then
   `shapes.trace {"path": "/shapes", "from": "/objects_image", "labels": true}`: one shape per
   object, in camera space.
2. `tex.shape_content` from the shapes' `id` output: fill, pulse, chase or ripple per object.
   Because there is one set of shapes, ripples and chases run across all projectors as one.
3. Per projector: `tex.remap` with the content as `in` and a `tex.image` of
   `objects/proj_to_cam_smooth_N.exr` as `map`, `extend` zero.
4. `tex.composite` with `op` multiply: the remap as layer1, a `tex.image` of
   `objects/weight_N.png` as layer2.
5. Into that projector's `out.projector`.
