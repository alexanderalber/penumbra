# projector-wall

Three projectors side by side on a bare wall, showing one picture. Each projector shows its share
of the picture. Where two of them overlap, both fade, so the overlap is as bright as the rest of
the wall. On the wall drift slow clouds of colour, and a soft band of light sweeps across all
three projectors and back every 25 seconds.

The measurements come from a real setup: three Epson projectors A, B and C, left to right, and a
camera facing the wall. Without the projectors, look at `/wall`: the picture as it lands on the
wall, seen from where the camera stood. Then look at `/unblended`: the same without the blend
weights. There the two overlaps are twice as bright. Look at `/blend_a`, `/blend_b` and
`/blend_c` for what each projector shows.

The measured maps ship with the example, at half the projectors' resolution (960 x 540), as
16-bit PNGs:

- `blend/proj_to_cam_0.png` to `_2.png` (A, B, C): per projector pixel, the camera pixel it
  lands on (red and green, 0 to 1). This is the scan's smoothed map, `proj_to_cam_smooth.exr`.
- `blend/weight_0.png` to `_2.png`: how much light each projector gives, from `scan.blend`.
  At every spot of the wall the weights add up to 1, measured to within 0.005.
- `flatfield/gain_proj_a.png`, `black_proj_a.png` and the same for B and C: the flat field from
  `projector.flatfield`. The gain evens out brightness and colour across each projector; it costs
  31 % of the light on this wall. The black lift raises black beside the overlaps to the
  brighter black inside them.
- `preview/wall_to_proj_a.exr` to `_c.exr`: per pixel of `/wall`, the projector pixel the camera
  saw there; -1 where the projector does not reach. Only the preview needs them.

## The network

**The content** is drawn once, in the camera's picture: `/clouds` (tex.noise) slides sideways
with `/drift` and is coloured by `/colours` (palette `sea`). `/sweep` is a soft bright bar whose
centre follows `/sweep_x`. `/content` lays the bar over the clouds. The wall fills only a band
across the middle of the camera's picture, so that is where the content counts.

**Per projector** (`_a` is the left projector, `_b` the middle one, `_c` the right one):

1. `/share_a` (tex.remap, `extend` zero) takes the projector's part of `/content` through its
   map `/map_a`.
2. `/blend_a` (tex.blend_weight) multiplies it by the blend weight `/weight_a`.
3. `/proj_a` (out.projector) shows it. Its `gain` and `black` inputs take the flat field
   (`/proj_a_gain`, `/proj_a_black`). Its `matrix` and `curves` (group *colour*) are empty here:
   `projector.match` writes them. `/proj_a` has no `display` yet, so it opens no window until
   you choose one.

**The preview:** `/seen_a` (tex.remap) carries `/blend_a` back into the camera's picture through
`/wall_to_a`. `/wall` adds the three. `/raw_a` and `/unblended` do the same with `/share_a`,
before the blend weight. The preview leaves out the flat field and the colour match, since
`out.projector` applies them only on the way to the projector.

`/scan_a`, `/scan_b` and `/scan_c` are the scan nodes that measure all this on your own wall.

## Your own wall

The steps, in the order they are done at the wall. The details of each are in
[the Helpers window](../../docs/helpers.md#blend-and-match) and [Scanning](../../docs/scanning.md).

1. **Keep a copy.** **Save copy as** in the status bar's folder menu, then open that copy.
2. **Projectors.** Set each `out.projector`'s `display` to one of the ids the Helpers window
   lists under **Outputs**: `/proj_a` the left projector, `/proj_b` the middle, `/proj_c` the
   right. Line them up with the **Align** test pattern so that neighbours overlap. Here they
   overlap by about two fifths of a picture.
3. **Camera.** Add a `tex.video_in` named `cam` that sees the whole wall. Turn its automatic
   exposure and white balance off. In the Helpers window, **Camera exposure**, **Check** says
   whether the camera reads each projector well; aim for `ok`.
4. **Scan.** Select `/scan_a` and press **scan** in its param panel. It scans the three
   projectors one after another and writes blend weights into the scan folders (`scan/proj_a`,
   ...). Then connect each scan node to its projector's chain: its `proj_to_cam` output to
   `/share_a`'s `map`, its `weight` output to `/blend_a`'s `weight`, and the same for B and C.
   Delete `/map_a` and `/weight_a` and their siblings.
5. **Colour match.** Helpers window, **Colour match**, **Match**. It writes `matrix` and
   `curves` into each `out.projector`, so all three show the same whites and greys.
6. **Flat field.** Helpers window, **Flat field**, **even** all (on a bare wall), **Measure**.
   It writes new gain and black pictures into `flatfield/`; `/proj_a_gain` and the other five
   nodes reload them.
7. **Preview.** Point `/wall_to_a`'s `file` at `scan/proj_a/cam_to_proj.exr`, and the same for
   B and C. `/wall` then shows the whole camera picture.

Then put your own content into `/content`'s place. Anything drawn in the camera's picture works,
for example an effect from the [content library](../../library/README.md).

Try:

- `/sweep_x` `rate` for a faster sweep, `/sweep` `size` for a wider band.
- `/colours` `palette`: `ember`, `neon`, `magma`, or hex colours.
- `/blend_b` `mode` detail: thin lines and edges then come from one projector only. It needs the
  scan's `detail.png` at its `detail` input.
- `/proj_b` `correct` off: the projector without its colour match and flat field, to compare on
  the wall.
