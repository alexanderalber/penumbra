# synthwave

A glider flies over a city of wireframe blocks, between a floor grid and a ceiling grid. Behind
it lies a nebula, tinted warm and then cold. The glider is the courier from Hikyaku, a browser
game on alber.me. Everything else is made in the network: no pictures, no meshes from files.

- **Grid:** `/grid_plane` is a plane split into 80 by 80 `cells`. `/grid_lines`
  (mat.wireframe) draws it as a grid. `/floor` lays it flat and moves it one cell per beat,
  bound to `/scroll` (a saw). `/ceiling` is the same floor 4 metres higher.
- **City:** `/blocks` (geo.tex_instance) puts a copy of `/block` at every texel of
  `/block_places` and sizes it by `/block_sizes`. Both nodes run `blocks.wgsl`. Each block comes
  towards the camera at the grid's speed and starts again far ahead with a new size.
- **Lane:** the blocks stay low in a lane that drifts slowly from side to side. Each block takes
  the lane's place at the moment it appears far ahead. The lane is therefore a winding path that
  comes towards the camera.
- **Solid blocks:** `/facades` puts a slightly smaller `/walls` box inside every block, with
  `/wall_mat` (mat.occluder). The box is invisible: the sky shows through the block. The grid
  and the blocks behind it are hidden.
- **Glider:** `courier.obj` (`/courier`) holds hull, cabin, ducts, nozzles and fins. `fan.obj`
  (`/fan`) holds the blades of one lift fan. `/fan_turn` spins the blades by `/spin`, and
  `/fans` (geo.instance) puts four copies into the ducts by the rows of `/fan_places`. Two
  copies are mirrored, so they turn the other way. `courier.py` writes both files; run it after
  changing it.
- **Steering:** `/steer` runs `blocks.wgsl` at 1 by 1 pixels and hands on the lane where the
  glider flies: its place, its turn and its lean. `/course` reads that pixel as channels.
  `/ship` moves, turns and rolls the glider by them, and `/bob` lifts it up and down.
  `/ship_cover` hides what lies behind the glider, like the blocks.
- **Render:** `/render` draws floor, ceiling, blocks and glider through `/camera`. `/fade`
  (`fade.wgsl`) reads the distance from the render's `data` output and lets the lines fade out
  towards the horizon.
- **Sky:** `/clouds` (tex.noise) is bent into wisps by `/swirl` through `/wisps`. Stars come
  from `/star_noise` through `/stars`. Slow noise (`/tint`) picks colours from `/warm_keys` and
  `/cold_keys` (tex.lookup). `/palette` changes from one to the other every minute (`/season`,
  softened by `/season_fade`). `/sky` (composite `color`) puts the palette's hue on the
  nebula's brightness.
- `/out` lays the render over the sky (composite `over`).

Try:

- `fill` (how full the city is), `lane` (half its width), `drift` (how far it wanders, in
  metres) and `drift_rate` on `/block_places`, `/block_sizes` and `/steer`. Set all three the
  same.
- `bank` on `/steer`: how far the glider leans into a turn.
- `/fade` `start` and `end`: how far the lines reach.
- `/scroll` `rate` and the blocks' `speed` together for another tempo. `rate` is beats per
  second; `speed` is half of it, in metres per second.
- `/warm_keys` and `/cold_keys` `keys` for other palettes; `/season` `rate` for how often they
  change.
- A mat.constant in place of `/wall_mat`: solid blocks in its colour.
- `/spin` `rate` for faster fans. `/ship` `uniform` for the glider's size. When you move the
  glider along `translate` y, set `at` on `/steer` to the same value.
- Bind `/scroll` or `/glow` `brightness` to a sound channel (see the audio-reactive example).
