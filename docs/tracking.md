# Tracking

`tex.blobtrack` finds blobs in a picture (people in front of a camera, hands, spots of light)
and follows them: each blob keeps its id while it moves, and through a short gap. Positions
come out as a table and as channels, ready to bind.

## Setting up

1. Feed it a camera: Tab > tex > videoin, then Tab > tex > blobtrack, and connect the two (see
   [Camera](camera.md)). Any `tex` works as input, so a video file is fine for trying things
   out, and tex nodes in between can crop or mask the picture first.
2. **Learn the empty scene.** With `detect` on `background` (the default), a blob is whatever
   differs from a learned picture of the empty room. The first frame is learned by itself,
   which is wrong whenever someone is in the picture at that moment. Clear the picture and
   press **learn background** in the param panel, or **in 5 s** to give yourself time to walk
   out first. From a script: `blobtrack.learn {path, delay?}`.
   Instead of learning, a picture can be connected to the second input, `background`: a
   photo of the empty room (`tex.image`), a frame held by a `tex.cache` (`record` off), or
   anything computed. While it is connected it is the background for every frame; learning
   and `adapt` do nothing.
3. Look at `out` in the viewer and set `threshold`. `show` switches what `out` shows:

| `show` | what |
|---|---|
| `overlay` | the picture, darkened where nothing counts; each blob with its box and centre in its id's colour |
| `mask` | white where a pixel counts, after `open` and `close` |
| `difference` | how much each pixel differs from the background; the threshold is mid grey |
| `background` | the learned background |

`out` is as large as the analysed picture (`size`, under "more"), not as large as the input.

Dark patches in the overlay on a person are parts that look like the background behind them
(dark hair before a dark cupboard). A lower `threshold` or a larger `close` fills them; better
light helps most. A patch where someone *was* when the background was learned shows up as a
blob too: learn again with the picture empty.

## Detecting

- `detect` `background`: a pixel counts when it differs from the background by `threshold` in
  any colour channel (0 to 1, linear light).
- `detect` `bright`: a pixel counts when its luminance is at least `threshold`; no background.
  For spots of light (a torch, a laser pointer on a wall) or a lit person in a dark room.
- `adapt`: the background follows lasting changes (the light outside, a chair moved) within
  about this many seconds. Pixels inside a blob follow ten times slower, so someone standing
  still for a long while fades into the background eventually. 0 keeps the background as
  learned.
- `min_area`, `max_area`: blobs smaller or larger than this share of the picture are ignored.
  `max_area` keeps a sudden change of the whole room (the light switched on) from being one
  huge blob; `adapt` then takes it into the background.
- `max_blobs`: at most this many, the largest.
- Under "more": `size` (width the picture is analysed at; 320 is plenty for people across a
  room, more finds smaller things), `open` (removes specks and lines thinner than twice this, in
  analysed pixels), `close` (fills holes and joins parts closer than twice this).

## Following

A blob in the new frame takes the id of the nearest blob of the frame before, within `reach`
(a share of the picture's width, beyond where the blob's speed would have carried it). A blob
that goes missing keeps its id for `hold` seconds; if it comes back within that time and
within reach, it is the same blob. Ids count up and are never reused while the node lives.

## Outputs

Coordinates are shares of the picture, origin top left: `x` 0 is the left edge, 1 the right;
`y` 0 the top, 1 the bottom. That is how surface coordinates work too.

**`blobs`** is a table with a header row and one row per blob, ordered by id:

| column | what |
|---|---|
| `id` | stays with the blob |
| `x`, `y` | centre of its pixels |
| `left`, `top`, `width`, `height` | its box |
| `area` | its pixels as a share of the picture |
| `age` | seconds since it appeared |
| `vx`, `vy` | speed in picture widths and heights per second, smoothed |
| `r`, `g`, `b` | its mean colour in the picture (linear) |

**`chans`** has `count` (blobs now) and for each slot `n` from 1 to `slots` (under "more",
default 4): `idn`, `xn`, `yn`, `arean`. A blob keeps its slot while it is tracked and a new
blob takes the lowest free slot, so `x1` follows one person even when someone else comes and
goes. An empty slot has id 0 and zeros.

## Timing

The node shrinks the input on the GPU, reads that small picture back and analyses it on the
CPU. It waits up to 4 ms for the readback within the cook, so with a quick GPU the blobs belong
to the camera frame that just arrived. When the readback takes longer, the node cooks again as
soon as it is there; a frame arriving in between is skipped, never queued.

## Not there yet

Classifying spots by colour, masking out the projector's own light, and mapping camera
positions onto the surface (camera calibration) come later. The learned background does not
survive a restart.
