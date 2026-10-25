# synthwave

The courier glider of Hikyaku flying over a city of wireframe blocks between a floor and a ceiling grid, through
a nebula tinted warm and then cold. Everything but the glider is made in the network: no
pictures, no meshes from files.

- `/grid_plane` is a plane split into 80 by 80 `cells`; drawn by `/grid_lines` (mat.wireframe)
  it is a grid. `/floor` lays it flat and moves it one cell per beat, bound to `/scroll` (a
  saw); `/ceiling` is the same floor 4 metres higher.
- `/blocks` (geo.tex_instance) puts a copy of `/block` at every texel of `/block_places` and
  sizes it by `/block_sizes`. Both run `blocks.wgsl`, one handing on positions, the other
  sizes: each block comes towards the camera at the grid's speed and starts again far ahead
  with a new size. The lane down the middle keeps them low. `/facades` puts a
  slightly smaller `/walls` box (mat.occluder, `/wall_mat`) inside every block: invisible, so
  the sky shows through, but it hides the grid and the blocks behind.
- `/render` draws floor, ceiling and blocks through `/camera`; `/fade` (`fade.wgsl`) reads the
  distance from the render's `data` output and lets the lines melt away towards the horizon.
- The sky: `/clouds` (tex.noise) bent into wisps by `/swirl` through `/wisps`, stars from
  `/star_noise` through `/stars`. Slow noise (`/tint`) picks colours from `/warm_keys` and
  `/cold_keys` (tex.lookup); `/palette` changes from one to the other every minute
  (`/season`, softened by `/season_fade`), and `/sky` (composite `color`) puts the palette's
  hue on the nebula's brightness.
- The glider is the courier the player flies in Hikyaku on alber.me: `courier.obj` (`/courier`)
  holds hull, cabin, ducts, nozzles and fins, `fan.obj` (`/fan`) one lift fan's blades.
  `/fan_turn` spins them by `/spin` (a saw), `/fans` (geo.instance) puts four copies into the
  ducts by the rows of `/fan_places`, two of them mirrored so they turn the other way. `/ship`
  rolls it by `/tilt`, steers it by `/weave` and lets it bob by `/bob`; it sits in the scene and
  looks like the blocks: white lines (`/ship_lines`) and a copy with `/wall_mat` (`/ship_cover`)
  that hides what lies behind it. `courier.py` writes both files (run it after changing it).
- `/out` lays the render over the sky (composite `over`).

Try:

- `/block_places` and `/block_sizes` `fill` (how full the city is) and `lane`; set both the
  same.
- `/fade` `start` and `end`: how far the lines reach.
- `/scroll` `rate` and the blocks' `speed` together for another tempo (rate is beats per
  second, speed half of it in metres).
- `/warm_keys` and `/cold_keys` `keys` for other palettes; `/season` `rate` for how often
  they change.
- `/wall_mat` swapped for a mat.constant: solid blocks in its colour.
- `/spin` `rate` for faster fans, `/ship` `translate` and `uniform` to move and size it.
- Bind `/scroll` or `/glow` `brightness` to a sound channel (see the audio-reactive example).
