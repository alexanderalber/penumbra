# Scanning: projecting onto objects

A camera films the projectors' structured light patterns; from those pictures penumbra learns
which camera pixel each projector pixel lands on. That is enough to find objects standing in
front of a wall, to give each projector a mask and a blend weight for them, and to draw content
once, in the camera's picture, so it lands on every object the same way in every projector.

A mostly flat object (a relief panel) needs no reference scan: `scan.relief` turns one scan
per projector into height maps and light masks.

Everything here is a command (see [Control](control.md)); the results are plain files in the
project, read by `tex.image`. Nothing of it runs per frame: during the show what remains is a
`tex.remap` lookup and a multiply per projector.

Paths in the commands are relative to the project directory.

## 1. Scan: `scan.capture`

The room dark, the camera's exposure fixed (no auto exposure, no DRO / auto HDR) and not
clipping white, nothing moving, the camera running as a `tex.video_in` node. Then

    penumbra-cli ctl scan.capture '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b"]}'

scans one projector after the other (without `projectors`: every active `out.projector`). The
projector being scanned shows the patterns, every other active projector black (also those not
listed, whose light would fall on the scene); what they show is replaced on
the output board, the network is not changed, and when the command ends (however it ends) they
show their pictures again. The patterns are drawn in the projector's own pixels, past its
colour correction (a projector dimmed by `projector.match` still shows full contrast). Each
projector takes 2 + 2 (log2 width + log2 height) pictures, 46 at 1920 x 1080, about a quarter
of a second each.

A picture is taken once the pattern was presented, the camera's latency (measured below, plus a
frame) and `settle` (s, 0.05) passed, the camera's picture changed from the one taken before
(stripes finer than the camera resolves change nothing it sees: those once twice the latency
passed), and two camera frames in a row agree
(`verify`, on; at most `retries`, 3, frames may disagree before the picture is taken anyway,
with a warning). The latency is measured on
the way, from black to white and white to black, and reported as `latency_ms` (`rise`, `fall`):
the time from the output window presenting a change to the first camera frame showing it. The
coarsest stripes are checked: where the projector reaches, pattern and inverse must differ
clearly, otherwise the pair is taken again once.

The results go into `out` (default `scan/`), one folder per projector named by its path
(`/proj_a` is `scan/proj_a/`):

| file | what |
|---|---|
| `cam_to_proj.exr` | camera size; per camera pixel the projector position (red, green, 0 to 1), -1 where none |
| `proj_to_cam.exr` | projector size; per projector pixel the camera position, blue 1 where measured, 0.5 where filled in from a neighbour, -1 where none |
| `scene.png` | the scene as the projector sees it |
| `quality.png` | per camera pixel what was decoded (grey by confidence; black: not reached; magenta: clipped; blue: ambiguous; red: outliers) |
| `frames/00.png` ... | the camera's pictures, with `frames: true` only |

The reply has per projector the camera pixels decoded, the projector pixels measured, the bits
the camera resolves (`resolved_bits` of `bits`), the median margin of every bit's pattern
against its inverse, the latency, the frames that had to be taken again, the times and the
warnings. `level` (1) lowers the lit stripes if the camera clips even with its exposure down;
`fill` and `options` are those of `scan.decode`.

Without a camera node (photos from another camera), the same files come from the pictures in a
folder: a `tex.scan_pattern` into the projector, stepped through its `pattern` 0, 1, 2, ...
(`scan.patterns` lists them), one picture of each, then

    penumbra-cli ctl scan.decode '{"frames": "scan/a"}'

writes them into `scan/a/decoded/`. That folder is a scan like the ones `scan.capture` writes.

`proj_to_cam.exr` through `tex.image` into `tex.remap`'s `map`, with the camera's picture as
`in`: the camera's view in the projector's pixels.

## 2. Line projectors up: `scan.align`

Projectors standing side by side (or one above the other) on one surface, each feeding from a
`tex.mapper` of the same content:

    penumbra-cli ctl scan.align '{"scans": ["scan/proj_a", "scan/proj_b"], "mappers": ["/map_a", "/map_b"]}'

fits per scan a homography from camera pixels to projector positions (robustly, outliers
dropped; `error_px` says how well it fits), which gives each projector's frame in the camera's
picture. The content is the rectangle, in the camera's axes, from the left edge of the leftmost
frame to the right edge of the rightmost, between the lowest top edge and the highest bottom
edge (for projectors one above the other, `layout` column, the same turned: top to bottom,
between the innermost left and right edges). Each mapper gets its projector's share of it as
`region` (its frame's left to right edge, the outer ones to the content's edge), the corners of that share as `corner_tl` ...
`corner_bl` and `fit` stretch, all as one undo step. The content's `aspect` (camera pixels)
is in the reply: content of that aspect fills it without distortion. Where the frames overlap,
the mappers' soft edges blend.

The camera should look at the surface roughly square-on: the content is a rectangle in its
picture. On a surface that is not flat the homography fits only roughly (a warning gives the
median error); if the frames share no rectangle, the error lists each frame in the camera and
its fit error. A camera turned by a quarter, upside down or mirrored is fine (`camera_turned` says
which); projectors turned against each other are not. `apply: false`, or no `mappers`, only
reports. A mesh warp on a mapper stays and moves the aligned picture (a warning says so).

## 3. Objects: `scan.objects`

Scan every projector twice: once of the empty scene (the wall alone, the **reference**), once
with the objects in place (`scan.capture` with `"out": "wall"`, then with the default `scan`).
Camera and projectors must not move in between.

    penumbra-cli ctl scan.objects '{"projectors": [
        {"reference": "wall/proj_a", "scan": "scan/proj_a"},
        {"reference": "wall/proj_b", "scan": "scan/proj_b"}]}'

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

## 4. Relief: `scan.relief`

For a mostly flat object, a relief panel filling most of each projector's frame, one scan per
projector is enough; no scan of the empty scene is needed:

    penumbra-cli ctl scan.relief '{"scans": ["scan/proj_a", "scan/proj_b"]}'

Per projector a homography from projector to camera pixels is fitted robustly (reweighted,
Cauchy) to the measured pixels: the panel's plane. Whatever stands out of the plane lands
beside where the homography predicts, by its parallax, along the direction between camera and
projector; that residual is the height, in camera pixels of parallax (not millimetres, and not
quite the same scale everywhere: the parallax per millimetre grows towards the near side of the
field). Before that the panel is cut out from the wall or floor around it, which would tilt the
plane: where the residual (filled in from the nearest measured pixel, median filtered) jumps by
more than `jump` (camera pixels per projector pixel, 4), one surface ends; the largest piece is
the panel, and the plane is fitted again on it alone, twice. Pixels without a measurement
(the camera is coarser than the projector, self-shadows) get their height from their
neighbours, in the projector's own pixels. The plane takes up any tilt of the panel and also
the broadest part of the relief itself: a panel that is one big dome comes out flatter.

The light mask is the panel with its holes filled, so unmeasured self-shadows inside stay lit.
Pixels without a measurement are lit only inside the outline of the measured panel pixels:
in notches and gaps of that outline up to about twice `reach` wide (projector pixels, 60).
Nothing past the outline is lit. There the scan knows nothing: a strip along the panel's edge
the camera could not decode looks the same as wall or floor beside the panel. An unmeasured
pixel in a notch is also tested: where the plane and its filled height put its light
in the camera's picture, does the camera see another projector pixel there? Then the panel's
edge stands in front of that spot and the pixel's light lands on the wall behind it, so it stays
dark. Raise `reach` if dark gaps stay along the panel's edge. A strip along the edge that was
not decoded at all stays dark whatever `reach` is; scan again with more light there. Also left out is light the plane predicts outside the camera's picture (never seen); the
mask is then shrunk by `erode` (projector pixels, 4) so nothing spills onto the wall.

**Sign.** The scan does not tell near from far: the same data comes from a mirrored depth with
the camera on the projector's other side. `sign` `"auto"` (the default) is a heuristic: measured
pixels well outside the panel are taken as the wall behind it, so heights grow away from them;
with no wall in view, the heights are taken as skewed upwards (narrow peaks over a broad base).
Check the result by eye. `1` and `-1` fix the sign to the camera's axes instead; the reply's
`sign.as_param` says which of the two a run found, so once it is right it can be pinned while
camera and projectors stay put. The sign applies to the first projector; the others follow it
through the overlap.

**Several projectors.** Each projector's heights are in its own units (its baseline to the
camera differs). They are brought to the first one's over the camera pixels both light inside
their masks, by a line fit with matched standard deviations (`scale` `"symmetric"`, the default;
`"least_squares"` regresses on the other projector), outliers trimmed. The reply gives the
scale, offset, both slopes, the overlap in camera pixels and the correlation. One scale per
projector is an approximation: across a wide field the parallax per millimetre varies, so a
projector whose area lies mostly far from the overlap can still be off by tens of percent.

Written into `out` (default `relief/`), `N` the projector's place in the list:

| file | what |
|---|---|
| `height_N.exr` | projector size, float: the height, 0 to 1 over the 1st to 99.5th percentile of all lit pixels (clamped), in red, green and blue |
| `mask_N.png` | 8-bit, projector size: 255 where the projector lights the relief, 0 elsewhere; also a mask for `scan.blend` |
| `height_camera.exr` | camera size: the same heights as the camera sees them, alpha 0 where no projector lights the relief |
| `relief_view.png` | the same to look at, grey by height, black where unknown (also camera pixels no scan decoded) |

The reply has per projector the fit's error, the panel (pixels, share of the measured ones),
the relief area, its own height range, how the sign was found and the scale to the first; the
heights' raw range (`range`, camera pixels of parallax in the first projector's units) and the
warnings.

**Content on the relief** stays in the network, per projector:

1. `tex.image` of `relief/height_N.exr`.
2. Colours by height: `tex.lookup` with the height as `in` and a `tex.ramp` (horizontal, the
   colours as `keys`, e.g. blue, greens, browns, white for hypsometric tints) as `lut`.
3. Contour lines: `tex.contour` of the same height map, `interval` 0.05 (twenty steps over the
   range), `mode` alpha, `color` dark; `tex.composite` (over) lays them onto the colours. The
   lines keep their width on steep and flat parts, and a valley clipped at 0 gets no outline.
4. `tex.composite` multiply with `tex.image` of `relief/mask_N.png` (or run `scan.blend` with
   `"masks": ["relief/mask_0.png", ...]` and multiply by its `weight_N.png` instead, where the
   projectors overlap).
5. Into the projector's `out.projector`.

Because the heights are per projector and in shared units, the same ramp gives the same colour
at the same height in every projector.

## 5. Blend: `scan.blend`

Where two projectors light the same spot it is twice as bright, and any misregistration
doubles. `scan.blend` gives each projector a weight: where another projector also lands on the
same camera pixel (inside its mask), the weight falls towards the projector's own frame edge,
`d_self / (d_self + d_other)` with `d` the distance to the frame edge capped at `ramp`
(projector pixels, 200); elsewhere 1; blurred by `blur` (3), 0 outside the mask. The weights
of all projectors at one camera pixel add up to 1; the reply says how closely.

    penumbra-cli ctl scan.blend '{"scans": ["scan/proj_a", "scan/proj_b"],
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

## 6. The network

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
