# projection-onto-objects

A relief of a mountain range, 3D-printed in tiles and leaning against a wall, lit by two
projectors stacked one above the other. A camera scanned it once; from that scan come a height
map and a blend weight per projector. On the relief lie land colours by height, contour lines,
a sun going round, a tide rising and falling in the valleys and a snow line moving up and down.

Without the projectors, look at `/view`: the picture as it would land on the relief, put
together from the camera's photos of it. Look at `/out_a` and `/out_b` for what each projector
shows.

The measured maps ship with the example, at half the projectors' resolution (960 x 540):

- `relief/height_0.png`, `height_1.png`: the height of the relief per projector pixel, 0 to 1,
  from `scan.relief` (16-bit PNG instead of its float EXR).
- `relief/weight_0.png`, `weight_1.png`: how much light each projector gives where both reach,
  from `scan.blend` with the relief's masks. Outside the relief they are 0.
- `camera/cam_to_proj_0.png`, `cam_to_proj_1.png`: per camera pixel the projector pixel it saw
  (`cam_to_proj.exr` of each scan), and `camera/photo_0.jpg`, `photo_1.jpg`: the relief lit
  white by each projector, as the camera saw it (as grey, the camera's tint taken out). Only
  `/view` needs them.

The network, per projector (`_a` is the lower projector, `_b` the upper):

- `/height_a` is the height map. `/land_a` (tex.lookup) gives it colours from `/land_keys`:
  greens in the valleys, browns higher up, grey rock at the top. Because both projectors'
  heights are in the same units, one ramp gives the same colour at the same height in both.
- `/slope_a` measures how steeply the relief rises, left to right and top to bottom, on the
  height softened by `/smooth_a`. `/shade_a` (tex.channel_mix) turns that into light from one side: its weights are bound to
  `/sun_x` and `/sun_y`, two lfos a quarter wave apart, so the sun goes round once every 20
  seconds. Facing the sun stays as it is, facing away gets darker. `/lit_a` multiplies the
  colours by it.
- `/waves_a` is the height moved up and down a little by moving noise (`/swell`). `/sea_a`
  (tex.threshold, inverted) is white where it lies below `/tide`, which rises and falls once in
  40 seconds; `/water_a` colours that blue through `/water_keys`. `/surf_a` (tex.contour, one line at the `offset` bound to `/tide`)
  draws the shore.
- `/snowfield_a` is white above `/snowline`, on the height moved by still noise (`/drift`), so
  the snow line is ragged; `/snow_a` colours it.
- `/contour_a`: a dark line every twentieth of the height range.
- `/map_a` lays snow, water and lines over the shaded land. `/out_a` multiplies it by
  `/weight_a`, which also leaves the wall around the relief dark. `/proj_a` is the projector;
  it has no `display` yet, so it opens no window until you choose one.
- The preview: `/seen_a` (tex.remap) moves `/out_a` into the camera's picture through
  `/cam_to_proj_a`, `/lit_room_a` multiplies it by the photo. `/room` adds both projectors
  (an average, which `/view` makes twice as bright), `/upright` turns the camera's picture upright
  (the camera stood on its side). It matches a photo of the lit relief closely; the dark spots
  are where the camera saw no projector pixel during the scan.

`/scan_a` and `/scan_b` are the scan nodes that measure all this on your own object. The
steps, from an empty project to the running picture, are in
[Scanning](../../docs/scanning.md#example-a-relief-from-scan-to-picture).

Try:

- `/tide` `offset` and `amplitude`: how high the water reaches.
- `/snowline` `offset`: where the snow lies; its `rate` for how fast it moves.
- `/sun_x` and `/sun_y` `rate` (keep them equal) for a faster day; `/slope_a` and `/slope_b`
  `strength` for deeper shadows.
- `/land_keys` `keys` for other colours, `/contour_a` `interval` for more or fewer lines.
