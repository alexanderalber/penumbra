# Points

The `pts` nodes work on points on the GPU: particles, point clouds, trails, a plexus of lines,
a flock. They are penumbra's counterpart to TouchDesigner's POPs. The example
[particles](../examples/particles/README.md) shows the main recipe in one network: open it from
**Examples** in the folder menu.

## What a `pts` value is

**Points with attributes.** A `pts` value is a list of points. Each point carries named
attributes: `position` always, and `normal`, `color`, `scale`, `uv`, `velocity`, `age` and others
where a node gives them. Every attribute is a buffer on the GPU with one value per point. A node
that changes one attribute passes the others on untouched, so a long chain stays cheap.

**Names.** Attribute names are words. TouchDesigner's names work wherever you type an
attribute name and mean the same attribute:

| penumbra | TouchDesigner | | penumbra | TouchDesigner |
|---|---|---|---|---|
| `position` | `P` | | `age` | `PartAge` |
| `normal` | `N` | | `life` | `PartLifeSpan` |
| `color` | `Color` | | `id` | `PartId` |
| `scale` | `PointScale` | | `mass` | `PartMass` |
| `uv` | `Tex` | | `drag` | `PartDrag` |
| `velocity` | `PartVel` | | `weight` | `Weight` |
| `force` | `PartForce` | | `width` | `LineWidth` |

A missing attribute reads as its default: position 0, colour white, scale 1, weight 1.

Names starting with `pts_` belong to penumbra's working attributes, such as the pull of wind
that `pts.particles` reads. Most nodes drop them. A file, a table, channels or a program that
names one gets an error: choose another name.

**A count that may live on the GPU.** Nodes that make or drop points on the GPU
(`pts.particles`, `pts.delete`, `pts.proximity`) keep the number of points there. The CPU then
knows only how many there may be. Every node downstream handles that by itself. The viewer
shows the count a frame late, as "15024 of at most 15130 points".

**Lines.** Points can be joined into lines: a trail behind each particle, a generated line or
circle, the rows and columns of a grid, or pairs of points from `pts.proximity`.
`geo.from_pts` in `lines` mode draws them.

**A box.** Some nodes know the box around their points; the viewer and thumbnails frame by it.
Where no node knows it (particles, points moved by an expression), `geo.from_pts` reads the box
back from the GPU, a frame or two late.

## Points on screen

Four nodes give a picture:

1. `pts.generate`: `shape` `sphere`, `count` 5000.
2. `geo.from_pts`, wired from it: a small disc that faces the camera at every point.
3. `tex.render`, wired from `geo.from_pts` into `geo`. Set `background` to black.
4. `view.camera` into `camera` of `tex.render`, `position` [0, -3, 0.5].

The viewer shows a ball of white dots. Click `geo.from_pts` instead: the viewer shows its points
in 3D, without a camera.

`geo.from_pts` draws every point with `tex.render`, as any geometry: a material through
`geo.assign_material` colours the dots, lights and shadows act on them, `geo.merge` puts them
beside other geometry. Its `mode` chooses what stands at each point:

- **`sprites`**: a flat disc or square that faces the camera, `size` metres wide times the
  point's `scale`. `softness` 1 fades each sprite from its centre: a soft glowing dot.
- **`copies`**: a copy of the geometry on the `model` input at every point (instancing).
- **`lines`**: a ribbon along the points' lines, `width` metres wide times the point's `width`.

`blend` `add` adds the sprites' or lines' colour to what lies behind, so dense places glow. Added
sprites cover nothing: the render stays see-through where only they are. Put the render over a
background with `tex.composite`.

## If you know TouchDesigner

| TouchDesigner | penumbra | |
|---|---|---|
| Point Generator, Grid, Sphere, Box, Circle, Line, Rectangle POP | `pts.generate` | one node, `shape` chooses |
| Particle POP | `pts.particles` | |
| Feedback POP | `pts.feedback` | |
| Noise POP | `pts.noise` | `flavour` curl for curl noise |
| Force Radial POP, Force POP | `pts.force` | `kind`: attract, repel, axis, vortex, direction, wind |
| Field POP | `pts.field` | writes `weight` |
| Math POP, Math Mix, Math Combine, Limit, Attribute, Pattern POP | `pts.math` | one expression you type |
| Transform POP | `pts.transform` | |
| Random POP | `pts.random` | |
| Delete POP | `pts.delete` | |
| Lookup Texture POP | `pts.lookup_tex` | |
| Trail POP | `pts.trail` | |
| Proximity POP | `pts.proximity` | |
| Neighbor POP | `pts.neighbor` | gives sums and means, no indices |
| GLSL POP | `pts.shader` | WGSL, see [Shaders](shaders.md#a-program-over-points-ptsshader) |
| Merge, Switch, Null POP | `pts.merge`, `pts.switch`, `pts.null` | |
| Point File In POP | `pts.file` | `.ply`, `.xyz`, `.csv`, `.txt`, `.pts` |
| TOP to POP | `pts.from_tex` | |
| SOP to POP, Sprinkle POP | `pts.from_geo` | `mode` vertices or surface |
| CHOP to POP, DAT to POP | `pts.from_chan`, `pts.from_table` | |
| POP to TOP | `tex.from_pts` | |
| POP to SOP, Line MAT, Point Sprite MAT, Geometry COMP instancing | `geo.from_pts` | sprites, copies, lines |
| POP to CHOP, Analyze POP | `chan.from_pts` | |
| POP to DAT | `table.from_pts` | |

The add dialog finds each node by its TouchDesigner name: type `trail pop` or `line mat`.

What a TouchDesigner user trips over:

- **The loop is a wire.** A Particle POP names its target. In penumbra the last node of the
  loop is wired into the `previous` input of `pts.particles` (or `pts.feedback`). The wire is
  drawn dashed.
- **Every point is drawn.** There are no point primitives to add first. `geo.from_pts` draws
  whatever points it gets.
- **Expressions instead of operation menus.** `pts.math` takes a WGSL expression such as
  `position.z + sin(time) * 0.1`, where TouchDesigner has menus of operations.
- **Neighbour indices are not given out.** `pts.neighbor` writes counts, sums and means
  (`neighbors`, `cohesion`, `separation`, `near_velocity`). A `pts.shader` with
  `// @neighbors` walks the neighbours itself.
- **z is up**, as everywhere in penumbra. Gravity is [0, 0, -9.81].
- **No counterpart yet:** Ray, Polygonize, Extrude, Skin, the Line tools (Line Thick, Line
  Resample and the like), GLSL Advanced, GLSL Copy, Copy, Cache, Sort, Group. Points reach
  meshes only as sprites, copies and lines.

## Particles

`pts.particles` makes points that are born, move, age and die. They are born on the points of
its `source` input.

1. `pts.generate`: `shape` `circle`, `fill` `inside`, `size` [0.3, 0.3, 0.3]: a small disc to
   be born on.
2. `pts.particles`, its `source` wired from the generator: `rate` 2000 (births a second),
   `life` 1, `velocity` [0, 0, 4], `spread` 0.8, `gravity` [0, 0, -9.81].
3. Draw them: `geo.from_pts` with `size` 0.015, `tex.render`, a `view.camera` at
   [0, -6, 0.5].

A fountain rises and falls. `max` is the most particles at once (100 000 by default): births
that do not fit are dropped. Each particle carries `id`, `age`, `life`, `velocity`, `force` and
every attribute of the point it was born on. Particles stay in the order they were born, oldest
first.

### A force in the loop

Wire the particles into the forces, the last force into a `pts.null`, and the null back into
the `previous` input of `pts.particles`:

```
pts.generate -> pts.particles -> pts.noise -> pts.force -> pts.null --+
                     ^ previous                                       |
                     +------------------------------------------------+
```

Start from the fountain above, set `gravity` to [0, 0, 0] and `drag` to 1:

- **`pts.noise`**: `attribute` `force`, `flavour` `curl`, `amplitude` 4. The particles swirl
  like smoke.
- **`pts.force`**: `kind` `vortex`, `strength` 3. The particles circle the z axis.
- **The null** is the end of the loop: TouchDesigner calls it the target. Draw from it.

Each step starts from what the null held a step ago. `pts.particles` gives `force` out as 0,
and the loop adds this step's forces. A particle gains `force / mass` in speed each second,
loses some to `drag`, and moves by its speed.

The loop must keep the particles in number and order. A `pts.delete` in it is refused with a
warning. To end particles from the loop, write a number above 0 into `kill`: a `pts.math` with
`result` `kill` and `expression` `f32(position.z < 0.0)`.

**A mask:** a `pts.field` before the force writes `weight`, 1 inside its shape and 0 outside.
`pts.force` multiplies by `weight`, so it acts only there.

**Wind** (`pts.force` `kind` `wind`) pulls each particle towards the wind's speed. A particle
never passes that speed, however light it is.

**What the loop writes stays.** Every attribute but `force` comes back into the next step as the
loop left it. A node that adds to an attribute adds again at every step, so the value grows
frame after frame. Set values instead of adding to them.

**An inherited `weight` masks every force.** Points from `pts.from_tex` carry `weight` (the
picture's alpha), and the particles born on them inherit it. Clear `mask` on the force, or set
`weight` in the loop.

### Colour over life

`age / life` runs from 0 at a particle's birth to 1 at its death. A `pts.math` after the null,
`result` `color`:

```
vec4f(mix(vec3f(1.0, 0.95, 0.6), vec3f(0.95, 0.08, 0.02), age / life), 1.0)
```

turns the particles from pale yellow to red. `result` `scale` with `1.0 - age / life` shrinks
them as they age.

**A random number per particle:** `rand_key(id, 0u)` gives each particle its own number between
0 and 1 that stays with it. `rand(0u)` is keyed by the place in the list, and that place changes
as older particles die. `pts.random` and `pts.delete` by random follow `id` by themselves.

### Soft, glowing particles

On `geo.from_pts`: `softness` 1, `blend` `add` and `size` 0.04. Give the particles a dim
colour, since added sprites add up: put the colour expression above in brackets and multiply
it by 0.5. Composite the render over a dark
`tex.constant` with `tex.composite`. Dense places glow.

### Emit from a model or a text

`pts.from_geo` with `mode` `surface` scatters points over a mesh, each with the surface's normal
there. Wire `geo.text` into it and it into the `source` of `pts.particles`. Set `along_normal` on
the particles to 0.2: they leave the letters outwards. The points lie where `tex.render` would
draw the mesh. A `geo.transform` between the text and `pts.from_geo` turns the emitter with the
text: bind its `rotate` z to a `chan.lfo` with `amplitude` 30, and the sparks leave a swaying
word.
Several emitters go into one `source` through `pts.merge`. Each system counts its ids from 0, so
`pts.merge` moves the ids of its second input up by half their range (a third for three inputs,
and so on). Ids that already lie in their input's part stay as they are. So points that come
round a `pts.feedback` loop into the same input keep their ids, and trails and random numbers
keyed by id hold.

## Trails

`pts.trail` puts a line behind every point through the places it had over the last frames:
TouchDesigner's Trail POP with a Line MAT.

```
pts.particles ... pts.null -> pts.trail -> pts.math -> geo.from_pts (mode lines)
```

1. **`pts.trail`** after the null of the loop. `length` 16 keeps 16 places per trail. `every` 2
   keeps every second frame, so the trail reaches twice as far back.
2. **`pts.math`**, `result` `width`, `expression` `1.0 - trail`. Each trail point has `trail`: 0
   at the particle, 1 at the oldest place. The trail thins to its tail.
3. **`geo.from_pts`**, `mode` `lines`, `width` 0.01: a ribbon that faces the camera.

A trail finds its particle by `id` and ends with it. Each place keeps the `color`, `width` and
`scale` the particle had then (`attributes` lists them).

**Memory:** trails are sized for as many particles as there may be. 100 000 particles with
trails of 32 are 3.2 million points. One set of trails takes at most 512 MB, and the node keeps
two; longer trails leave the last particles without one, with a warning.

## Lines between near points (plexus)

`pts.proximity` joins each point to its near neighbours with lines: TouchDesigner's Proximity
POP.

1. `pts.generate`: `shape` `box`, `fill` `inside`, `distribution` `random`, `count` 2000,
   `size` [2, 2, 2].
2. `pts.noise`, `amplitude` 0.1, `speed` 0.3: the points drift.
3. `pts.proximity`: `distance` 0.25 (the longest line, in metres), `max_lines` 3 (lines each
   point starts).
4. `pts.math`, `result` `color`, `expression` `vec4f(color.rgb * (1.0 - span01), 1.0)`. Each
   line end has `span` (the line's length in metres) and `span01` (the length over `distance`).
   Long lines fade to black.
5. `geo.from_pts`, `mode` `lines`, `width` 0.004. A second `geo.from_pts` on the drifting
   points draws them as sprites; `geo.merge` puts both into one render.

`choose` `random` keeps each line as long as its two points stay within `distance`; `nearest`
can switch a line when two distances change order. `max` caps the lines, a million by default.

## A flock (boids)

`pts.neighbor` gives each point what its neighbours within `distance` add up to: `neighbors`
(how many), `cohesion` (the way to their middle), `separation` (a push away from those nearer
than `separation`) and `near_velocity` (their mean speed). These are the three rules of a flock.

```
pts.particles -> pts.neighbor -> pts.math (force) -> pts.null --+
      ^ previous                                                |
      +---------------------------------------------------------+
```

1. `pts.generate`: `shape` `sphere`, `fill` `inside`, `distribution` `random`, `size` [3, 3, 3].
2. `pts.particles`: `rate` 6000, `max` 600, `life` 1000, `velocity` [0.5, 0, 0], `spread` 1.
3. `pts.neighbor`: `distance` 0.5, `separation` 0.15.
4. `pts.math`, `result` `force`:
   `6.0 * separation + 1.5 * (near_velocity - velocity) + 1.2 * cohesion - 0.35 * position`.
5. `pts.null`, wired back into `previous`.

Within seconds the 600 birds fly as one flock. The last term pulls the flock back to the middle.
A `pts.math` after the null, `result` `color`, `vec4f(0.5 + 0.5 * normalize(velocity), 1.0)`,
colours each bird by its heading.

## A picture as points

`pts.from_tex` makes a point per texel of a picture, or per cell when it samples the picture
down: TouchDesigner's TOP to POP.

1. A picture: `tex.file`, `tex.video_in`, or `tex.noise` with `speed` 0.5 to try it.
2. `pts.from_tex`, `mode` `height`, `count` 50000, `height` 0.3: a relief of coloured points,
   lifted by brightness towards the camera.
3. `geo.from_pts` with `size` 0.008, and a `view.camera` at [0, -2.5, 0.8].

`mode` `color` lays the points flat, `position` reads positions stored in a picture, `depth`
turns a depth camera's picture into a point cloud. `pts.lookup_tex` does the other way round:
each point takes a colour or a number from a picture, at its `uv` or its position.
`tex.from_pts` turns an attribute back into a picture, a texel per point.

## Values out and in

- **`chan.from_pts`** gives `count` as a channel, and with `attribute` `position` the first
  points' coordinates (`tx0 ty0 tz0`) and the mean, minimum and maximum over all points
  (`tx_mean`, `tz_max`). Bind a param to them. What comes from the GPU arrives a frame late.
- **`table.from_pts`** shows the first points as rows, to look at what they hold.
  `pts.from_table` reads such a table back into the same points. The exceptions are attributes
  of one number named like a component: `x`, `r`, `u`, `alpha`, `index`, or a name ending in
  `_x` or `_0`.
- **`pts.from_chan`** and **`pts.from_table`** make a point per sample or row. The names say
  where a number goes: `x y z` or `tx ty tz` place a point, `r g b a` colour it, a plain name
  makes an attribute. An audio spectrum becomes a line of points that `pts.math` can lift.
- **`pts.file`** reads a point cloud from `.ply`, `.xyz`, `.csv`, `.txt` or `.pts`.

## Time

Particles, trails and `pts.feedback` take one step per frame, as long as the frame. While the
show stands still they stand still. Stepped frame by frame (`cook.step`), the same steps give
the same particles. `reset` starts them over, as do `node.reset` and the reset of a component
they sit in. `warm_up` on `pts.particles` simulates that many seconds ahead at the start,
without the loop's forces.

## Limits

- **Drawing is the cost.** The programs over points are cheap: noise over a million points
  takes about 0.2 ms a frame. A million sprites take about 2 ms, soft and added ones about
  1.2 ms. Sprites of many pixels each are bound by fill rate: a million discs of about 16
  pixels took 22 ms. All at 1920 x 1080 with 4x antialiasing, on an RTX 3090 at a 280 W power
  limit. The show laptop's RTX 3060 mobile is presumably three to four times slower.
- **Each node costs CPU time.** Every node in a chain takes some 0.05 to 1 ms of CPU time per
  frame whatever the number of points: the [particles](../examples/particles/README.md) example, 25
  nodes, takes 4 to 6 ms a frame on the desktop above, most of it CPU.
- **A crowd slows the neighbour search.** `pts.proximity` and `pts.neighbor` look at no more than
  `max_per_cell` points of each cube around a point; a million points in a small box still
  take tens to hundreds of milliseconds a frame. The nodes lower the cap themselves for large counts, with a
  warning.
- **The first cook compiles.** A new node of a kind takes up to a few seconds to compile its
  program the first time; until then it is pending, and the show runs on.
- **Not built:** collisions with geometry (Ray POP), meshes made from points, sorting, caching
  frames, groups. Lines have no round caps and no least width in pixels.
- **Tested on Windows with Direct3D 12 only.** The GPU programs use atomics, workgroup memory and
  indirect draws, which have not run on Metal (macOS) or Vulkan (Linux) yet.
