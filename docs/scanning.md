# Scanning: projecting onto objects

A camera films the projectors' structured light patterns; from those pictures penumbra learns
which camera pixel each projector pixel lands on. That is enough to find objects standing in
front of a wall, to give each projector a mask and a blend weight for them, and to draw content
once, in the camera's picture, so it lands on every object the same way in every projector.

A mostly flat object (a relief panel) needs no reference scan: `scan.relief` turns one scan
per projector into height maps and light masks. Nor does a scene of flat faces with objects
among them (boxes, a floor, a wall, a pumpkin on a crate): `scan.scene` finds every face and
every object in one scan per projector, and rectifies any face of known aspect; `scan.regions`
groups its faces into one region per crate front, side or lid and switches the floor off.

Everything here is a command (see [Control](control.md)); the results are plain files in the
project, read by `tex.image_file`. Nothing of it runs per frame: during the show what remains is a
`tex.remap` lookup and a multiply per projector.

Paths in the commands are relative to the project directory. The last section walks through
the whole chain once, from an empty project to a relief lit by two projectors, along the
example projection-onto-objects.

## The scan button: `tex.scan`

The quickest way: one `tex.scan` node per projector. Its params name the `projector`
(`out.projector`), the `camera` (`tex.video_in`), the `folder` scans go to (`scan`, the
projector's scan is the folder named after its path inside, `scan/proj_a`) and, optionally, the
`mapper` (`tex.mapper`) feeding the projector. Under *more*: `settle`, `level`, `second_level`
(0, off: a second pass at that level, see "Two levels" below) and `fill`, as for
`scan.capture` below.

The **scan** button in its param panel scans the projectors of every `tex.scan` naming the same
camera, one after another; then, if every one of them names its mapper, lines them up
(`scan.align`, one undo step), and writes blend weights and smoothed maps into the scan folders
(`scan.blend`). The show keeps running meanwhile: the projectors being scanned show the
patterns, the others black, and everything else keeps cooking. While it runs, the param panel
shows the step and a progress bar (the node a bar along its bottom edge) and **cancel** stops it
before the next picture; the files of earlier scans stay. How it ended stays in the param
panel; a failure shows on the node too, and goes to the log.

The node's outputs are the scan, held until the next one replaces it:

| output | what |
|---|---|
| `proj_to_cam` | the projector-size map into the camera's picture, for `tex.remap`'s `map`; smoothed within the decoded area (`smooth`, on) |
| `weight` | the blend weight (1 everywhere for a projector alone, or before a blend); multiply the projector's picture by it |
| `scene` | the scene as the projector sees it, for drawing or tracing masks |

They are read from the files in the scan folder, so a project opened later has them without
scanning again, and they follow the files: a rerun of `scan.blend` with masks and
`"into_scans": true` (see below) gives the nodes those weights. Nothing of it runs per frame.

The same as a command: `scan.run` (every `tex.scan`, or `{"nodes": [...]}`; `"wait": true`
returns the report when done), `scan.status` and `scan.cancel`:

    penumbra-cli ctl scan.run '{"nodes": ["/scan_a", "/scan_b"], "wait": true}'

The steps after the scan (relief, objects) stay commands: their results (heights, masks) are
files for `tex.image_file`, written by the sections below from the same scan folders.

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
| `frames/00.png` ... | the camera's pictures, with `frames: true` only (`frames_1/` ... for further `levels`) |

The reply has per projector the camera pixels decoded, the projector pixels measured, the bits
the camera resolves (`resolved_bits` of `bits`), the median margin of every bit's pattern
against its inverse, the latency, the frames that had to be taken again, the times, the
warnings and the notes. A camera that misses only the finest one or two bits gets a note, not a
warning: the map is then a projector pixel or a few coarse, usual when the camera sees the scene
coarser than the projector; more bits missing is a warning. `level` (1) lowers the lit stripes if the camera clips even with its exposure down;
`fill` and `options` are those of `scan.decode`.

Which bits a camera pixel resolves is judged around it (`options.window`, 7: a square of 15
camera pixels; 0 judges over the whole picture as before): a face the camera or the projector
sees at a grazing angle squeezes the stripes, so the finest bits the rest of the picture
resolves are noise there, and light the walls around throw back flattens a coarse one. Such
bits are still read but no longer make the pixel ambiguous. A neighbourhood that resolves more
than two bits fewer than the whole picture is judged by the picture's bits, so a shadow lit only
by light bouncing off the object casting it (which carries that object's coarse bits) stays
ambiguous. On a crate lid seen at a grazing angle this doubled the decoded share, at the same
accuracy. A dark face also falls under `min_contrast` (0.02 of full scale): at a dim `level`,
`"options": {"min_contrast": 0.01}` decodes it too.

**Two levels.** No single brightness suits a scene with white and dark surfaces: at full level
the camera clips on the white ones (magenta in `quality.png`), dimmed it loses the dark and
grazing ones. `"levels": [1, 0.4]` (instead of `level`, up to four) scans each projector once
per level and merges the decodes per camera pixel, the surer one winning; a clipped pixel is
not decoded, so the dim pass fills it. It costs a full scan per level. The scene colour is the
first level's. The reply lists per level the camera pixels it decoded, those taken from it and
those only it decoded (`levels`), and how far the levels' decodes lie apart where they overlap
(`apart_px`, projector pixels). On a scene of crates and a pumpkin, a full and a half-level scan
merged decoded 3 % more camera pixels than the full one alone; most of that where it clipped.

**Wrong decodes from bounced light.** In a niche of light surfaces, a bright pass decodes light
the walls throw on as if the projector lit those surfaces directly. The decoder fits the decode's
own epipolar geometry and drops pixels further than `options.epipolar` (4 projector pixels; 0
turns it off) from it; the reply's `off_geometry` counts them. Maps decoded before this check
keep their wrong pixels: decode the kept frames again.

Kept frames of several levels merge again with `scan.decode` and a list:
`"frames": ["scan/proj_a/frames", "scan/proj_a/frames_1"]`.

Without a camera node (photos from another camera), the same files come from the pictures in a
folder: a `tex.scan_pattern` into the projector, stepped through its `pattern` 0, 1, 2, ...
(`scan.patterns` lists them), one picture of each, then

    penumbra-cli ctl scan.decode '{"frames": "scan/a"}'

writes them into `scan/a/decoded/`. That folder is a scan like the ones `scan.capture` writes.

`proj_to_cam.exr` through `tex.image_file` into `tex.remap`'s `map`, with the camera's picture as
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

1. `tex.image_file` of `relief/height_N.exr`.
2. Colours by height: `tex.lookup` with the height as `in` and a `tex.ramp` (horizontal, the
   colours as `keys`, e.g. blue, greens, browns, white for hypsometric tints) as `lut`.
3. Contour lines: `tex.contour` of the same height map, `interval` 0.05 (twenty steps over the
   range), `mode` alpha, `color` dark; `tex.composite` (over) lays them onto the colours. The
   lines keep their width on steep and flat parts, and a valley clipped at 0 gets no outline.
   A measured map is noisy (scan residuals, tile edges) and the lines wobble and break into
   small islands; `smooth` blurs the height inside the node before the lines are found, while
   the colours stay sharp. The example uses 4 on its 960 x 540 map; a full-resolution scan
   wants about 10, and thick lines need it most: without smoothing their edges follow the
   noise and plateau pixels punch holes into them.
4. `tex.composite` multiply with `tex.image_file` of `relief/mask_N.png` (or run `scan.blend` with
   `"masks": ["relief/mask_0.png", ...]` and multiply by its `weight_N.png` instead, where the
   projectors overlap).
5. Into the projector's `out.projector`.

Because the heights are per projector and in shared units, the same ramp gives the same colour
at the same height in every projector.

## 5. Faces and objects: `scan.scene`

A scene built of flat faces, such as boxes or crates on a floor in front of a wall, with
objects among them that are not flat, needs no reference scan either:

    penumbra-cli ctl scan.scene '{"scans": ["scan/proj_a", "scan/proj_b"]}'

A flat face maps its projector pixels to camera pixels by a homography of its own, so every
face of the scene is one fit, and whatever is not flat fits none. Per projector the fits are
found one after the other (RANSAC: hypotheses from four nearby measured pixels, the best
refined over all pixels it explains within `tolerance`, camera pixels, 2), each fit's pixels
are taken out and the search goes on among the rest, until the best fit left holds no face of
`min_plane` (a share of the measured pixels, 0.002). A fit's pixels that do not touch are faces
of their own: the tops of two crates standing side by side at one height are one fit and two
faces (`group` in the reply tells which faces came from one fit). When that search ends, a
second, rough one goes on among the rest with 2.5 times the tolerance: some faces decode
noisier than others (a top seen at a grazing angle, a glossy lid, a surface lit by the light
the walls next to it throw back), and their median error comes out at 1 to 2.5 pixels instead
of 0.6.

A patch of a curved surface can be flat within the tolerance. Such a patch is dropped when a
smooth bowl explains its error better than the decode's noise does (`bend` in the reply: the
bowl's share of the error, its RMS and its `bow_px`, how far it bends over 100 projector
pixels along the patch's longest extent): when the bowl's RMS is above 0.3 camera pixels and
either its share is above 0.5 or it bows more than 5 pixels over 100, or when the bowl's RMS
alone is above 0.3 times the pass's tolerance. The bow counts only along the patch: across a
strip a few pixels wide the bowl follows the decode's noise, so a crate's lid seen at a grazing
angle (which falls apart into long strips and may sag a few pixels under what stands on it),
the posts and the rims of a crate stay faces, while a patch of a pumpkin is about as long as it
is wide. A patch is also dropped when the surface round it goes on bending the way its bowl
does: a flat face ends at a crease, a step or a rim, the pot or the pumpkin it was cut from
carries on. Narrow strips of cylinders and cones can still pass for faces. Where two faces
meet, each pixel goes to the one that fits it better.

What fits no face is an object, except surface detail: measured pixels near a face that lie
within 6 tolerances of its plane are the face's rims, ribs or noise (a Euro crate's walls are
full of them) and stay out of the objects. The rest is closed, its holes filled (never with a
face), pieces smaller than `min_object` (projector pixels, 1/256 of the projector) dropped.
Where the projectors disagree, a face wins: an object of one projector that lies mostly on
another projector's faces in the camera picture is dropped (`objects_on_faces` in the reply
per projector). Faces and objects are numbered alike in
every projector, top of the camera picture first: `planes_N.png` and `labels_N.png` per
projector (8-bit; `labels_N.png` as `scan.objects` writes it, for `scan.blend`'s masks and
`shapes.trace`), `planes_camera.png` and `objects_camera.png` in the camera's picture,
`scene_N.png` and `scene_view.png` to look at (faces in flat colours, objects white). Two
projectors that do not overlap on a face (a wall, the left half lit by one, the right by the
other) give it two numbers.

**Support and height.** The face right below the objects in the camera picture is the support
(`support` names another): per column of an object's lower edge the first face below it within
a twelfth of the picture's height (under a pumpkin the camera sees a band nothing measures, its
underside and its shadow), the face most columns name. The objects' height is their residual against the support's
homography, as `scan.relief` measures it against its panel: `height_N.exr`, `mask_N.png` and
`height_camera.exr`, in the first projector's units. Its sign comes from the faces the camera
sees below the support (a crate's front, the floor in front of it), which lie under the
support's plane while the objects stand on it; `sign` pins it as for `scan.relief`.

**Rectifying a face.** A face that is a rectangle of known aspect (a crate's top is 60 x 40, its
front 60 x its height) can get its own coordinates, for content in true proportions such as
tiles: `rectify` lists the faces and their aspect, `[{"plane": 7, "aspect": [60, 31]}]`;
without it the support is rectified as 60 x 40. The face's border in the camera picture,
without the part along an object, is fitted by four straight lines, so an edge hidden under an
object is completed from its visible parts; the lines' crossings are the corners, the far left
one first. A face whose edge is hidden by another face in front (not by an object) gets that
face's edge instead; then give its four corners in camera pixels with `corners`. Per rectified
face K: `uv_K_N.exr` per projector (red and green the face's u along the outline's first edge
and v along its last, 0 to 1) and `uv_K_camera.exr`. Both cover the whole outline, and their
blue tells what each pixel shows: 1 the face, 0.5 an object standing on it, 0.25 another face,
-1 outside the outline. Pixels inside the outline that nothing measured (a dark or glossy part
of the face, a band in an object's shadow) and the face's rims and ribs count as the face, so
content fills the face and not just the pixels that decoded; mask by blue above 0.9 to keep it
off the objects. `scene_view.png` draws the outline and a grid of true squares over it, to
check by eye. Into a `tex.shader`, `u *
aspect[0]` and `v * aspect[1]` are the face's own units.

A face that is not quite flat (a crate's lid sagging under the pumpkin on it, a warped board)
is mapped with a smooth correction on top of its plane: per projector, a quadratic fitted to
every measured pixel inside the outline within a few pixels of the plane, kept when it fits
clearly better and stays small over the whole face (the reply's `projector_fit`: pixels, the
median error with the plane alone and corrected, the correction's size). At a grazing angle a
camera pixel spans centimetres of the face, so without it tiles drawn per projector land a
centimetre or more off and the two projectors' tiles drift apart.

**A face given by its corners.** Such a lid also fails the face search itself: no plane holds it
within the tolerance, it falls apart into patches that each look curved, and its pixels end up
in the object standing on it. Give its four corners without a `plane`,
`[{"aspect": [60, 40], "corners": [[653, 551], [1148, 617], [1090, 742], [505, 663]]}]`, and
the face is claimed: in each projector the plane most of the measured pixels inside the corners
lie on (scored by how much of the quad its pixels cover, so a dense patch of the object in
front does not win), grown with its bend, takes every pixel inside that fits it, from the faces
and objects they were given to; the other projectors search where the first found it. Where the
lid sinks further right round the object's foot than its bend follows, those pixels miss the
plane on the side away from the object (the object stands on the near side, nothing in front of
the face can lie beyond it), so the object's pixels that lie up to 15 tolerances beyond the
face join it too (only the object's: the rim along the lid's edge lies beyond it as well); the
object then ends at its own outline instead of a band of lid below it. It gets a number of its own like any face, and
`"support": "rectified"` makes it the support.

**Objects where projectors disagree.** An object pixel of one projector is taken out of the
objects where another projector measures that camera pixel but sees no object there: a face one
projector sees at a grazing angle, or the seams between faces, would otherwise join the object.
`carved_px` in the reply counts the pixels taken out. A sagging lid that every projector sees
as part of the object still needs its corners. In `objects_camera.png` stray projector pixels
are left out (fewer than six of the object's pixels within 4 camera pixels): where an object
hides something, its edge pixels carry positions filled in between it and the background and
would otherwise add a ragged fringe.

**Straight borders.** The faces' borders as measured are as ragged as the decode along them.
Where a border is a line, `planes_camera.png` and `planes_N.png` get the line instead: each
border between two labels (cut where three meet) is fitted once for both sides, split into
straight runs with corners where it bends. Between two faces that meet at a crease, also across
a narrow seam the decode left dark, the line is where their planes cross, as every projector
seeing both faces sends them, when the measured border lies on it; on the scans of crates at
hand this holds rarely (the planes, fitted to thousands of pixels, still place their common edge
a few pixels off the measured one, and the goodness test then takes the fitted line). A run is
straight when it is at least `straight_length` long (pixels, 16), nine tenths of the measured
border lie within `straight_tolerance` of its line (pixels, 2) and it does not bow away from the
line by half that: a curve (a pumpkin, a pot, a patch of a relief) keeps its measured raster, so
do borders along objects (`objects_camera.png` and `labels_N.png` are never changed) and along
the picture's frame. On the crate scans about half of the faces' border length came out
straight. `planes_camera_measured.png` keeps the faces as measured; `"straight": false` writes
the measured ones into `planes_camera.png` too. The reply's `borders` (and per projector) says
how many borders there were, the share of their length that became straight (`straight_share`)
and on planes' meeting lines (`planes_share`), the straight runs' goodness (the 90th percentile
distance of the measured border from its line and the bow, `p90_px` and `bow_px`) and the pixels
that changed; `scene_view.png` draws the straight runs (fitted cyan, meeting lines yellow).

Everything the reply holds is also written to `scene.json`.

**Trying it without a scan.** `scan.synthetic` writes the scans of a made-up room, ray cast,
in the same files `scan.decode` writes: by default a landscape of five Euro crates with a
ribbed pumpkin on one of them, two full HD projectors left and right of a full HD camera, and
decode noise like a real scan's (plane fits off by about 0.7 camera pixels, median). Each scan
folder also gets `photo.png`, the camera's picture of the room lit white by that projector, for
previews in camera space. Then `scan.scene` on its folders gives what a real scan of such a
landscape would, so the content after it can be built before the real scan exists:

    penumbra-cli ctl scan.synthetic '{"out": "synthetic"}'
    penumbra-cli ctl scan.scene '{"scans": ["synthetic/a", "synthetic/b"], "out": "scene"}'

### 5.1 Regions: `scan.regions`

The face search splits what a person sees as one surface: a Euro crate's front falls apart into
its panels and the rib strips between them (the ribs stand a centimetre or two proud, a plane of
their own), a slightly bulged panel into two fits with a ragged border between them, and every
face's edge is as ragged as the decode there. Content that colours "each crate front" wants
regions instead: one per part of the scene, with clean edges, and the floor left dark.

Which faces belong together the geometry cannot tell: two crates standing side by side or
stacked have coplanar fronts, separated by a seam much like a rib groove. So the grouping is
given, by pixels of the camera picture, and `scan.regions` does the rest:

    penumbra-cli ctl scan.regions '{"scene": "scene", "edit": [{"op": "merge",
        "at": [[300, 600], [280, 520], [160, 600]], "name": "left crate"}]}'

**The grouping** is a list of regions, each a set of seed pixels in the camera picture, an
optional name and an optional `enabled`. The face under a seed (or the nearest one within
`reach`, 12 camera pixels) belongs to the region; a face goes to the first region that names it.
Faces under no seed are regions of their own. Seeds rather than face numbers, because face
numbers change with every rescan while a pixel keeps showing the same crate as long as the
camera and the crates stay. The grouping lives in a file, `regions.json` in the scene folder
(`file` names another), so it survives a rescan and `scan.scene` running again:

    {"regions": [
      {"name": "left crate", "seeds": [[300, 600], [280, 520], [160, 600]]},
      {"name": "post between the crates", "seeds": [[1163, 526]], "enabled": false}
    ]}

`regions` in the params replaces the file's list; `edit` changes it, one step after the other,
and the file is written back. The steps, all by camera pixel:

| step | what |
|---|---|
| `{"op": "merge", "at": [[x, y], ...], "name"?}` | the faces under the points, and every region they are already in, become one region |
| `{"op": "split", "at": [x, y]}` | the face under the point leaves its region and is a region of its own again |
| `{"op": "disable", "at": [x, y]}` | the region under the point is off (nothing in the label pictures) |
| `{"op": "enable", "at": [x, y]}` | it is on, also when it lies on the floor |
| `{"op": "auto", "at": [x, y]}` | back to the default: on, unless it lies on the floor |
| `{"op": "name", "at": [x, y], "name": "..."}` | names it |
| `{"op": "clear"}` | forgets the whole grouping |

An editor that lets someone click faces needs nothing but this command: a click is a camera
pixel, the reply lists every region with its faces, seeds, size and kind, and every face with
`at`, a pixel well inside it.

**In the viewer.** Load `regions_camera.png` from the scene folder into a `tex.image_file` (it can
be named before it exists) and select it: the viewer shows the camera picture of the scan with
every face in the colour of its region (neighbouring regions never share one), dark lines
between regions, faint ones between a region's faces, regions that are off striped grey with
"off" on them, the names on the regions, and the suggestions as dashed lines with a `+` in the
middle. Opening it reads the last reply when nothing changed since, else runs `scan.regions`
once.

| on the picture | what |
|---|---|
| click a face | selects it (its edge turns white); a click beside every face forgets the selection |
| Shift or Ctrl + click | adds the face to the selection or takes it away |
| M | merge: the selected faces' regions become one |
| S | split: the selected faces leave their regions, each a region of its own again |
| D | off, or on again when everything selected is off |
| A | back to the default (on, unless on the floor) |
| click a `+` | confirms the suggestion: merges the strip into the panel beside it |
| Esc | forgets the selection |
| Ctrl+Z, Ctrl+Y | undo, redo of the region changes |
| wheel, right or middle drag | zoom, pan |

The header has the same as buttons, and a name field for the one region the selection is in
(Enter names it). Every change is one `scan.regions` call with the clicked faces' pixels (a
second or two; the status line says `working...`, changes made meanwhile follow in one call),
so `regions.json`, the label pictures and the reply are rewritten and every `tex.image_file` showing
them reloads. The options the command ran with last (`seam`, `smooth`, ...) are kept. Undo and
redo send the grouping as it was (`regions`); they work while the pointer is over the picture
and only for the changes made since the editor opened, since the grouping is a file beside the
scan, not part of the project's history.

**The floor** is switched off by default. Against the support (the face the objects stand on),
every other face's plane shows how it lies: faces parallel to the support meet it only at the
horizon, faces that are not meet it in a line that runs between or across them (a crate's front
meets its lid along their shared edge). The floor is the lowest large face in the camera picture
below the support that lies parallel to it; faces on its plane are `floor`, other faces parallel
to it `level` (the support, other tops), the rest `upright`. A region is the kind most of its
pixels are. Without a support in `scene.json` nothing is classed and the floor stays on; `enable`
switches a floor region on, `disable` switches anything off.

**Cleaning**, per region in the camera picture: the seams between its faces up to about twice
`seam` wide (camera pixels, 10) are closed with pixels nothing measured, holes filled the same
way, the edge smoothed (`smooth`, a Gaussian of 2.5 pixels). Then, with `straight` (on by
default), four straight lines are fitted to the region's border (along an object standing in
front the border is left out, notches such as a crate's handle recess are closed first); where
the quadrilateral fits (at most 4 % of the region outside it, at most 4 % of it on other faces,
three quarters of it filled), it replaces the region, and the reply lists its corners. Regions
that are no quadrilateral (a front half hidden behind another crate, one cut by the picture's
frame) get straight borders where a line fits them, as `scan.scene` does for faces (between two
regions on the line their planes meet in where it fits), and keep their cleaned raster elsewhere. Objects always stay out, and where two regions claim a pixel
the one whose own face lies nearest gets it. Each projector's regions are the camera regions it
lights: per projector pixel the region of the camera pixel it lands on (pixels the decode only
filled in count where they continue the region of the nearest measured pixel), gaps of a few
pixels closed.

**Suggestions:** thin faces (rib strips, rims; at most 20 camera pixels across) that touch a
larger face across a narrow seam and lie on its plane within 3 camera pixels are listed in the
reply as `suggestions` (both faces, a pixel inside each). They are never applied: a person
confirms them with a `merge`.

Written into `out` (default: the scene folder):

| file | what |
|---|---|
| `regions_camera.png` | 8-bit, camera size: 0 none or off, enabled regions numbered top of the picture first. In place of `planes_camera.png` |
| `regions_N.png` | the same per projector, projector size. In place of `planes_N.png` |
| `regions_view.png` | to look at: regions in colour with dark borders, faces that are off striped grey, objects white, the grouping's seeds as dots |
| `regions_reply.json` | the reply (with `borders` as in `scan.scene`): per region its number, name, faces, seeds, kind, size, straight corners and per projector the plane through its faces (`projector_to_camera`, a homography, and the worst median error of a face against it); the regions that are off and why; every face with its region, kind, height (support 1, floor 0) and a pixel inside it; the suggestions; the grouping as used |
| `regions.json` | the grouping (in the scene folder or `file`), written when `regions` or `edit` changed it |

Content made for `planes_camera.png` takes `regions_camera.png` as it is: the same 8-bit
labels, one per crate front instead of one per strip. The region numbers change when the
grouping does (they are counted top of the picture first), so content that picks single regions
should pick them by name or seed from the reply, not by number.

## 6. Blend: `scan.blend`

Where two projectors light the same spot it is twice as bright, and any misregistration
doubles. `scan.blend` gives each projector a weight: where another projector also lands on the
same camera pixel (inside its mask), the weight falls towards the projector's own frame edge,
`d_self / (d_self + d_other)` with `d` the distance to the frame edge capped at `ramp`
(projector pixels, 200); elsewhere 1; blurred by `blur` (3), 0 outside the mask. The weights
of all projectors at one camera pixel add up to 1; the reply says how closely.

    penumbra-cli ctl scan.blend '{"scans": ["scan/proj_a", "scan/proj_b"],
        "masks": ["objects/labels_0.png", "objects/labels_1.png"]}'

`masks` is optional (one file or `null` per scan); without it the whole frame counts.
`"into_scans": true` writes `weight.png`, `detail.png` and `proj_to_cam_smooth.exr` into each scan's own
folder instead (where a `tex.scan` reads them). Also
written per projector is `proj_to_cam_smooth_N.exr`: the camera is usually coarser than the
projector, so only some projector pixels have a measured camera position and content remapped
through the raw map has jagged edges. The smoothed map averages the measured positions with a
Gaussian (`sigma`, 6 projector pixels) within each object (or the decoded area without masks).

Written into `out` (default `objects/`):

| file | what |
|---|---|
| `weight_N.png` | 16-bit, projector size; stored sRGB-encoded so that `tex.image_file` gives back the weight itself |
| `detail_N.png` | the same format: the detail weight, 1 where this projector draws the small features, 0 where another does (see below) |
| `proj_to_cam_smooth_N.exr` | the projector's `proj_to_cam` map, smoothed |

### Small features from one projector

Two projectors never agree to the pixel: the scan is off by a pixel or two here and there, a
projector's lens creeps. A cross-fade hides that for large areas, but a thin line, an outline
or text shows up twice in the overlap, slightly apart. `detail_N.png` says, per projector
pixel, which projector draws the small features: of the projectors that light that camera
pixel, the one whose pixel covers the least of the camera's picture there (the sharpest, from
the nearer or longer-throw projector, or the one looking straighter at the surface); a
projector within 16 pixels of its frame edge only where every other one is too; when the
projectors are alike, the lower number. The detail weights at a camera pixel add up to 1 like
the weights, but switch rather than fade, softened by `blur`.

`tex.blend_weight` in `detail` mode uses it: the picture is split at `size` pixels (8 by
default), the blurred picture is weighted by `weight_N.png`, the rest (picture minus blur) by
`detail_N.png`. Brightness still crosses over smoothly; lines and edges come from one
projector only. Light cannot be negative, so where a thin dark gap lies right beside something
bright, the other projectors' blurred share fills a faint halo into it. Pick `size` a little
above how far the projectors disagree; much larger only widens that halo.

## 7. The network

Content is made once, in the camera's picture, and each projector takes its share:

1. `tex.shapes` with the camera's size as `width` and `height`; a `tex.image_file` of
   `objects/objects_camera.png` beside it, then
   `shapes.trace {"path": "/shapes", "from": "/objects_image", "labels": true, "live": true}`: one
   shape per object, in camera space. `live` connects the picture to the shapes' `trace` input
   and sets `trace_labels`, so the node traces by itself whenever the picture changes (a new scan)
   or the project opens, and nothing traced is stored in the project file. A script writing
   `project.ron` gets the same by writing that connection and those params:
   `"/shapes": (kind: "tex.shapes", params: {"width": 1920, "height": 1080, "trace_labels": true}, inputs: {"trace": "objects_image"})`.
   Without `live`, the outlines are written into the node's `shapes` text once, to edit by hand.
2. `tex.shape_content` from the shapes' `id` output: fill, pulse, chase or ripple per object.
   Because there is one set of shapes, ripples and chases run across all projectors as one.
   Or any effect of the [content library](../library/README.md) (edges, water, a lamp going
   round an object, tiles, ...): they take the scan's pictures and draw in the camera's
   picture too.
3. Per projector: `tex.remap` with the content as `in` and a `tex.image_file` of
   `objects/proj_to_cam_smooth_N.exr` as `map`, `extend` zero.
4. `tex.blend_weight`: the remap as `in`, a `tex.image_file` of `objects/weight_N.png` as
   `weight`. For content with thin lines, outlines or text, `mode` detail and a
   `tex.image_file` of `objects/detail_N.png` as `detail` (see "Small features from one
   projector" above). A `tex.composite` multiply with the weight as layer2 does the same as
   `smooth` mode.
5. Into that projector's `out.projector`.

Then, optionally, even out each projector's light and the black around overlaps with
`projector.flatfield` and the same scan folders (see [Even light across a projector, and black in
overlaps](helpers.md#even-light-across-a-projector-and-black-in-overlaps)): it connects a gain map
and a black lift to each `out.projector`.

## Example: a relief from scan to picture

The example **projection-onto-objects** (status bar folder button, **Examples**) is a relief
lit by two projectors stacked one above the other, with the maps of one scan. It shows its
picture without camera or projectors (`/view`). The steps below make the same thing for your
own object and room, in the order they are done at the object. Each is a command (see
[Control](control.md)) or a button.

**0. A project.** Either the example, kept as your own: **Save copy as** in the status bar's
folder menu, into a folder beside the installation, then open that. Or an empty one:

    penumbra-cli ctl project.new '{"dir": "D:/shows/relief"}'

**1. Projectors and camera.** One `out.projector` per projector, its `display` from
`output.list` (the example's `/proj_a` and `/proj_b` have none, so they open no window until
you set it), and a `tex.video_in` for the camera:

    penumbra-cli ctl output.list
    penumbra-cli ctl node.set '{"path": "/proj_a", "params": {"display": "EPSON PJ [SECA514] @ target 168196"}}'
    penumbra-cli ctl node.set '{"path": "/proj_b", "params": {"display": "EPSON PJ [SECA519] @ target 168194"}}'
    penumbra-cli ctl node.create '{"path": "/cam", "kind": "tex.video_in", "params": {"device": "Cam Link"}}'

In an empty project create the projectors first, with `node.create` and `kind`
`out.projector`. Fix the camera's exposure, white balance and focus on the camera itself.

**2. Scan.** One `tex.scan` per projector, naming projector and camera (the example has
`/scan_a` and `/scan_b`, naming `/cam`):

    penumbra-cli ctl node.create '{"path": "/scan_a", "kind": "tex.scan", "params": {"projector": "/proj_a", "camera": "/cam"}}'
    penumbra-cli ctl node.create '{"path": "/scan_b", "kind": "tex.scan", "params": {"projector": "/proj_b", "camera": "/cam"}}'

Room dark, then the **scan** button in either node's param panel, or

    penumbra-cli ctl scan.run '{"wait": true}'

Both projectors are scanned into `scan/proj_a` and `scan/proj_b`, about 20 s each. (With
`tex.mapper`s named in the nodes, the run also lines the projectors up for flat content,
`scan.align`; a relief does not need that.)

**3. Heights and masks.**

    penumbra-cli ctl scan.relief '{"scans": ["scan/proj_a", "scan/proj_b"]}'

writes `relief/height_0.exr`, `height_1.exr`, the masks and `relief/relief_view.png`. Look at
the view: valleys dark, peaks bright. If it is the wrong way round, run it again with
`"sign": -1` (or `1`, the opposite of the `sign.as_param` the reply gave).

**4. Blend weights** where both projectors reach, inside the masks:

    penumbra-cli ctl scan.blend '{"scans": ["scan/proj_a", "scan/proj_b"], "masks": ["relief/mask_0.png", "relief/mask_1.png"], "out": "relief"}'

writes `relief/weight_0.png` and `weight_1.png`. In the example these replace the shipped
weights, and the `tex.image_file` nodes reload them. Point the height nodes at the new heights:

    penumbra-cli ctl node.set '{"path": "/height_a", "params": {"file": "relief/height_0.exr"}}'
    penumbra-cli ctl node.set '{"path": "/height_b", "params": {"file": "relief/height_1.exr"}}'

**5. Colour (optional).** Make the projectors look alike, then even out the colour across each
(on a relief only the colour: its shading would be taken for a hot spot):

    penumbra-cli ctl projector.match '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b"]}'
    penumbra-cli ctl projector.flatfield '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "scans": ["scan/proj_a", "scan/proj_b"], "even": "colour"}'

`projector.flatfield` connects a gain map and a black lift to each `out.projector`.

**6. Content.** In the example it is there already: the projectors show it as soon as their
displays are set. In an empty project the base, for projector A (B the same with `_b`,
`height_1`, `weight_1`):

    penumbra-cli ctl node.batch '{"ops": [
      {"op": "create", "path": "/land_keys", "kind": "tex.ramp", "params": {"width": 256, "height": 1, "keep_size": true,
        "keys": "0 0.02 0.16 0.03\n0.3 0.188 0.402 0.061\n0.55 0.578 0.456 0.102\n0.75 0.305 0.127 0.045\n0.9 0.42 0.4 0.38\n1 0.6 0.6 0.6"}},
      {"op": "create", "path": "/height_a", "kind": "tex.image_file", "params": {"file": "relief/height_0.exr"}},
      {"op": "create", "path": "/weight_a", "kind": "tex.image_file", "params": {"file": "relief/weight_0.png"}},
      {"op": "create", "path": "/land_a", "kind": "tex.lookup"},
      {"op": "create", "path": "/contour_a", "kind": "tex.contour", "params": {"mode": "alpha", "color": [0, 0, 0, 0.6]}},
      {"op": "create", "path": "/map_a", "kind": "tex.composite"},
      {"op": "create", "path": "/out_a", "kind": "tex.composite", "params": {"op": "multiply"}}]}'
    penumbra-cli ctl node.connect '{"from": "/height_a", "to": "/land_a", "input": "in"}'
    penumbra-cli ctl node.connect '{"from": "/land_keys", "to": "/land_a", "input": "lut"}'
    penumbra-cli ctl node.connect '{"from": "/height_a", "to": "/contour_a"}'
    penumbra-cli ctl node.connect '{"from": "/land_a", "to": "/map_a", "input": "layer1"}'
    penumbra-cli ctl node.connect '{"from": "/contour_a", "to": "/map_a", "input": "layer2"}'
    penumbra-cli ctl node.connect '{"from": "/map_a", "to": "/out_a", "input": "layer1"}'
    penumbra-cli ctl node.connect '{"from": "/weight_a", "to": "/out_a", "input": "layer2"}'
    penumbra-cli ctl node.connect '{"from": "/out_a", "to": "/proj_a", "input": "in"}'

The moving layers of the example (sun, tide, snow line; its README says how they are made) are
copied over with copy and paste in the editor, or by opening the example's `project.ron` beside
your own.

**Looking without the room.** The example's `/view` puts both projectors' pictures into the
camera's picture (`tex.remap` through each scan's `cam_to_proj`) and multiplies them by a photo
of the relief lit white by each projector. For your own scan, point `/cam_to_proj_a` and
`/cam_to_proj_b` at `scan/proj_a/cam_to_proj.exr` and `scan/proj_b/cam_to_proj.exr`; the
photos are the first camera picture of a scan with `"frames": true` (`scan.capture`).
`/upright` turns the camera's picture by a quarter (the example's camera stood on its side);
set its `flop`, `flip_x` and `flip_y` for yours. At the object itself, look at the object.
