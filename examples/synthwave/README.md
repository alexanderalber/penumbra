# synthwave

A flight over a city of wireframe blocks between a floor and a ceiling grid, through a nebula
tinted warm and then cold. Everything is made in the network: no pictures, no meshes from
files.

- `/grid_plane` is a plane split into 80 by 80 `cells`; drawn by `/grid_lines` (mat.wireframe)
  it is a grid. `/floor` lays it flat and moves it one cell per beat, bound to `/scroll` (a
  saw); `/ceiling` is the same floor 4 metres higher.
- `/blocks` (geo.tex_instance) puts a copy of `/block` at every texel of `/block_places` and
  sizes it by `/block_sizes`. Both run `blocks.wgsl`, one handing on positions, the other
  sizes: each block comes towards the camera at the grid's speed and starts again far ahead
  with a new size. The lane down the middle keeps them low. `faces: hidden` on `/block_lines`
  hides the lines behind a block, the block itself stays see-through.
- `/render` draws floor, ceiling and blocks through `/camera`; `/fade` (`fade.wgsl`) reads the
  distance from the render's `data` output and lets the lines melt away towards the horizon.
- The sky: `/clouds` (tex.noise) bent into wisps by `/swirl` through `/wisps`, stars from
  `/star_noise` through `/stars`. Slow noise (`/tint`) picks colours from `/warm_keys` and
  `/cold_keys` (tex.lookup); `/palette` changes from one to the other every minute
  (`/season`, softened by `/season_fade`), and `/sky` (composite `color`) puts the palette's
  hue on the nebula's brightness.
- `/body` is an ellipse (tex.shapes). `/wobble` squashes, stretches, tilts and steers it with
  four channels (`/squash`, `/stretch`, `/tilt`, `/weave`); it hides the lines behind it
  (`/lines`, composite `outside`), darkens the sky (`/shadow`) and gets a white `/outline`.
- `/out` lays the lines and the outline over the sky.

Try:

- `/block_places` and `/block_sizes` `fill` (how full the city is) and `lane`; set both the
  same.
- `/fade` `start` and `end`: how far the lines reach.
- `/scroll` `rate` and the blocks' `speed` together for another tempo (rate is beats per
  second, speed half of it in metres).
- `/warm_keys` and `/cold_keys` `keys` for other palettes; `/season` `rate` for how often
  they change.
- Bind `/scroll` or `/glow` `brightness` to a sound channel (see the audio-reactive example).
