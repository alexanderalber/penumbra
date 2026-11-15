## What it is for

`tex.liquid` shows a liquid from the side, in the picture's plane. Liquid pours in from emitters
or starts as a shape, falls, splashes, pools at the bottom and runs over the edges when they are
open. Take it for water poured over a projection, text that melts into a pool, or a level that
fills up. The same node also simulates sand and snow, alone or together with the liquid.

`tex.fluid` is the other fluid: smoke or ink that fills the whole picture and swirls. Take
`tex.liquid` when there is a surface, so the liquid has a top, falls and comes to rest.

## A first network

1. Add a `tex.liquid` and a `tex.liquid_emitter`.
2. Wire the emitter's `out` into `emitter0` of the liquid.
3. Play the show. A stream falls from near the top left and forms a pool at the bottom.
4. To start with a shape instead: wire a `tex.text` (white text on black) into `fill` and set
   `reset` on and off. The letters are made of water and collapse into a pool.
5. To pour onto something: wire a white shape on black into `obstacles`, for example a
   `tex.text`. The liquid lands on the letters, fills their hollows and runs off their sides.

## Units

All numbers are in the picture's units. Positions are shares of the picture from its top left,
so 0.5, 0.5 is the middle. Lengths are picture heights. Speeds are picture heights per second,
with y down. `gravity` 3.27 is the earth's gravity when the picture shows 3 m of height. For a
picture of a glass 30 cm high, 32.7 makes the liquid move like water in that glass.

## Inputs

- **emitter0, emitter1, ...**: tables from `tex.liquid_emitter`, or any table with its columns
  (see there). 64 emitters in all at most. Emitters add liquid while there are free particles.
- **fill**: liquid lies wherever this picture is brighter than half, when the node starts or
  `reset` is on. `fill_material` says whether it is liquid, sand or snow. A picture wired later
  fills once. A change to the picture shows at the next reset. No liquid starts inside an
  obstacle.
- **obstacles**: solid wherever this picture is brighter than half, as in `tex.fluid`. The
  liquid lands on obstacles, pools on them and flows around them. The picture may change every
  frame: a camera mask, text, a shape that moves. Liquid that an obstacle appears over is pushed
  out to its nearest edge, at 0.1 heights per second plus the obstacle's own speed.
- **obstacle_velocity**: how the obstacles move, so they push the liquid along instead of only
  squeezing it aside. Red is x in widths per second, green y in heights per second, down
  positive: the same as the `velocity` output and `tex.fluid`'s. Each place of an obstacle takes
  the velocity this picture has there. Unwired, obstacles stand still. The `velocity` output of
  `physics.rigid_bodies` goes in as it is (see [Rigid bodies](#rigid-bodies)).
- **force**: motion that pushes the liquid, sand and snow, as in `tex.fluid`.
  `tex.optical_flow`'s output goes in as it is.
- **splats**: pushes at points from a table, as in `tex.fluid`. `tex.blob_track`'s table goes in
  as it is.

## Pushes

`force` and `splats` take the same pictures and tables as in `tex.fluid`, so one camera or
tracker can drive both nodes.

**The force picture** holds motion: red is x in widths, green y in heights, per frame of the
input, down positive. Per second, `force` times the motion's speed is added to the liquid's
speed. A camera's optical flow of a hand moving one picture height per second, at the default
`force` 4, pushes with 4 heights per second squared: a little more than gravity. A picture that
stays the same counts one frame per show frame. So a red of 0.01 in a 16:9 picture at 60 fps
pushes with 0.01 x 1.78 x 60 x 4 = 4.3 heights per second squared to the right. A noise with
`offset` 0 and a small `amplitude` makes gusty wind.

**Splats** are rows with columns `x`, `y` (0 to 1 from the top left), and optionally `vx`, `vy`
(picture sizes per second) and `radius` (picture heights; `splat_radius` without the column).
At most 64 rows. With `splat_motion` `push`, each splat adds `force` times its velocity per
second to the liquid within its radius. With `set`, the liquid there takes on the splat's
velocity: a paddle, or a still hand that holds the liquid back. Colour columns do nothing here:
only emitters and `fill` add liquid.

**Limits.** The force picture and the splats together push with at most 50 heights per second
squared; the node cuts a stronger push and warns. A stronger push makes the liquid faster, and
the node takes more substeps for it: up to about 2.6 times as many at the push's limit. The node
measures the force picture where the liquid is, and that measure arrives a frame or two late.
The push also acts on spray and on liquid in the air.

## Obstacles

The node turns the `obstacles` picture into distances to the nearest solid edge, on a lattice
twice as fine as the grid. It does that again when `edges` or any input of the node changes:
the mask, its velocity, but also an emitter whose table moves, a force picture that changes or
splats that move.

**Thin shapes.** A line half a grid cell thick still holds a gentle pour. Liquid that hits a
line that thin fast may pass it; thicken such lines with `tex.dilate`.

**Moving obstacles.** A mask that moves without `obstacle_velocity` pushes the liquid only by
taking its place. A slow paddle moves a pool almost as well that way. A fast paddle leaves
bubbles behind it and makes smaller waves than with its velocity. The `obstacles` picture of a
frame counts as where the obstacles are at the end of that frame, as every picture made for the
frame shows them. With `obstacle_velocity` the node moves them there over the frame's substeps.

**Fast obstacles.** The node takes more substeps for an obstacle faster than about 5 heights per
second at the default grid. It measures the fastest obstacle in `obstacle_velocity`, and that
measure arrives a frame or two late. `obstacle_velocity` counts up to 30 heights per second; a
faster obstacle moves the liquid at 30.

**friction** sets how much the liquid sticks to obstacles and to the picture's edges. 0 slides
freely. At 1 a thin film on a slope of 24 degrees slides about a sixth as far as without
friction. Deep liquid still runs down over the film.

## Rigid bodies

Bodies from `physics.rigid_bodies` push the liquid as moving obstacles. The solver draws them as
two pictures through a camera: `mask` and `velocity`.

1. Set `plane` on the solver to `xz`, so the bodies stay on a wall facing the camera.
2. Add a `view.camera` at y = -10 that looks along +y, with `projection` orthographic. Its
   `size` is the height in metres the picture shows. Wire it into `camera` of the solver, and
   into the `tex.render` that draws the bodies.
3. Set the solver's `width` and `height` to the liquid's.
4. Wire the solver's `mask` into `obstacles` and its `velocity` into `obstacle_velocity`.
5. Set the liquid's `gravity` to 9.81 over the camera's `size`: 3.27 for 3 m.

**Static obstacles too.** Put the solver's `mask` and the static mask together in a
`tex.composite` with `op` maximum, and wire that into `obstacles`. The bodies' `velocity` goes
into `obstacle_velocity` alone: it is 0 outside the bodies, so the static obstacles stand still.
A `tex.render` of the static geometry in white through the same camera makes a static mask that
lines up with what the bodies collide with. Example: [liquid-bodies](../../examples/liquid-bodies/).

**One way only.** The bodies push the liquid, but the liquid does not push the bodies. A body
falls through a pool as through air, keeps its speed and hits the floor below. The liquid
between body and floor then spurts out sideways. The solver's `damping` slows the bodies, in the
air as well. A `physics.field` of type wind that pushes up below the water's surface stands in
for buoyancy: stronger than gravity, the bodies float. The liquid-bodies example does both.

**Without `velocity`.** `tex.optical_flow` on the mask (times 60 at 60 fps) matches a body that
slides, but not one that turns, one slower than about 0.05 heights per second, or one at rest
after landing. Use the solver's `velocity`.

## Sand and snow

Each emitter has a `material`: liquid, sand or snow. The `fill` picture takes `fill_material`.
One node holds all three at once, so water can pour onto a sand heap.

**Sand** pours, slides and forms heaps. `sand_angle` sets how steep a heap stands: 35 degrees by
default, 20 to 38 work. A heap poured at 35 stands at 34 to 36 degrees and then stays still. A
column of sand that collapses at once runs out flatter, at about 20 degrees, as real sand does.
Values above 38 give no steeper heap. In `out`, sand shows in `sand_color` with a lighter or
darker shade per grain.

**Snow** clumps. Dropped, a block breaks into lumps and stays a lumpy pile. Poured, it builds
up where it lands, also on slopes. `snow_wetness` goes from 0 (powder that crumbles and slides)
over 0.5 (fresh snow) to 1 (wet snow that packs and sticks). In `out`, snow shows in
`snow_color`, bluer and darker deep in a pile, with a soft lumpy edge.

**Friction on obstacles.** Sand and snow hold on to obstacles and the picture's edges by their
own friction: a sand heap stays on a shelf with `friction` at 0. A higher `friction` adds to it.
Water next to sand slides as freely as before.

**Limits.**
- Sand does not sink in water. Grains that a pour washes into water hang in it as specks.
- Water does not wet sand: a heap dams water, and water flows over it.
- Snow does not melt, and sand and snow do not mix into one material.

## Particles, grid and cost

Four particles fill one cell of the grid. `grid` sets the cells across the picture, so it sets
the smallest drop and the thinnest stream. `particles` sets how much liquid can exist: at the
default grid, 65536 particles fill about a fifth of a 16:9 picture.

When all particles are in use, the emitters stop adding and the node warns. Open `edges` free
the particles that leave the picture, so a pour into open edges runs for hours at a constant
count.

A frame takes several substeps, more on a finer grid. Measured on an RTX 3090 at a 60 fps show,
with all particles in use:

| `grid` (16:9 picture) | `particles` | substeps | GPU per frame |
|---|---|---|---|
| 256 | 16384 | 14 | 0.6 ms |
| 384 (default) | 65536 | 21 | 1.9 ms |
| 512 | 131072 | 28 | 5.2 ms |
| 384 (default), sand | 65536 | 24 | 2.1 ms |
| 384 (default), snow | 65536 | 44 | 3.4 ms |
| 384 (default), a noise as `force` | 65536 | 21 | 2.0 ms |
| 384 (default), 8 splats | 65536 | 24 | 2.5 ms |

Drawing `out` takes 0.35 ms of that at the default grid and 1.3 ms at grid 512, where the
particles are drawn at the picture's full size. The other drawn outputs add less than 0.1 ms.
Obstacles cost about 0.35 ms more at the default grid in each frame where an input changes. With
a still mask and still emitters they cost nothing after the first frame.

For the same amount of liquid, twice the cells across need four times the particles and twice
the substeps: about eight times the cost. A show at 30 fps takes twice the substeps per frame.
A thick liquid (`viscosity` above 0.001) or a fast pour adds substeps. Sand adds a few, snow
doubles them: snow is stiff when it is packed. The node keeps the larger count from the first
frame with sand or snow until it starts over. A frame runs at most 128 substeps; beyond that the liquid moves slower
than the show, and the node warns.

## Time and state

The liquid is kept from frame to frame and follows the show clock: a paused show stands still.
A late frame is one longer step of at most 1/30 s, so a stall does not make the liquid jump.
`speed` slows the liquid down or speeds it up. `reset`, `node.reset` or R start over from the
`fill` picture. A new `grid`, or a picture of another shape, starts over too. The liquid is not
kept in the state snapshots: after a restart it starts from `fill`.

## Drawing

`out` draws every particle as a small spot and blurs the spots into one surface with a clean
edge. Where the liquid thins out, in streams and spray, the spots grow, so a stream stays whole
instead of breaking into dots. Thin liquid shows in `color`, deep liquid darker, and a lighter
band runs under the surface. Solids count as liquid for that band, so a pool on a shelf has no
band along the shelf.

**Foam** starts where the liquid hits something hard and in fast, thin spray. It lasts
`foam_time` (0.4 s by default), and `foam_strength` sets how white it shows in `out`. A whole
block of water that lands at once foams all over; lower `foam_strength` if that is too much.

**Sand and snow** go into the same surface, so water on a heap has one edge around both. Each
pixel takes the colours of the materials there, mixed by their share. Foam is only on the
liquid.

The look stays the same at 30, 60 or 144 fps and at any `speed`.

## Outputs

- **out**: the liquid in `color` with its foam, sand in `sand_color`, snow in `snow_color`,
  premultiplied. Transparent around it, with an edge
  about a pixel soft, so it goes straight into `tex.composite`.
- **velocity**: the liquid's velocity per cell, as in `tex.fluid`: red x in widths, green y in
  heights, per second. Blue is the speed in heights per second, alpha how full the cell is. The
  velocity goes on a few cells beyond the liquid and fades there, so a look coloured by speed
  has no step at the edge.
- **density**: how full each cell is, 1 at rest.
- **foam**: the foam as white over transparent, 0 to 1. Colour it or lay it over `out`.
- **grain**: a random grey per particle, averaged, on the liquid's alpha. The pattern travels
  with the liquid. Blurred and with more contrast it makes a crust for lava or mud. On sand the
  grey changes from grain to grain.
- **surface**: for shading in a `tex.shader`. Red is the distance into the liquid in pixels
  (negative outside), green the depth from 0 at the surface to 1 deep inside, blue and alpha the
  outward normal (x, y down). Solids count as liquid.

`foam`, `grain`, `surface`, `velocity` and `density` cost nothing while nothing reads them.

## What it cannot do yet

- A push back on rigid bodies: bodies from `physics.rigid_bodies` push the liquid, but they do
  not float, sink slower or get carried along.
- A second liquid of another colour, or sand that sinks in water.
