## What it is for

`tex.liquid` shows a liquid from the side, in the picture's plane. Liquid pours in from emitters
or starts as a shape, falls, splashes, pools at the bottom and runs over the edges when they are
open. Take it for water poured over a projection, text that melts into a pool, or a level that
fills up.

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
  `reset` is on. A picture wired later fills once. A change to the picture shows at the next
  reset. No liquid starts inside an obstacle.
- **obstacles**: solid wherever this picture is brighter than half, as in `tex.fluid`. The
  liquid lands on obstacles, pools on them and flows around them. The picture may change every
  frame: a camera mask, text, a shape that moves. Liquid that an obstacle appears over is pushed
  out to its nearest edge, at 0.1 heights per second plus the obstacle's own speed.
- **obstacle_velocity**: how the obstacles move, so they push the liquid along instead of only
  squeezing it aside. Red is x in widths per second, green y in heights per second, down
  positive: the same as the `velocity` output and `tex.fluid`'s. Each place of an obstacle takes
  the velocity this picture has there. Unwired, obstacles stand still.

## Obstacles

The node turns the `obstacles` picture into distances to the nearest solid edge, on a lattice
twice as fine as the grid. It does that again when `edges` or any input of the node changes:
the mask, its velocity, but also an emitter whose table moves.

**Thin shapes.** A line half a grid cell thick still holds a gentle pour. Liquid that hits a
line that thin fast may pass it; thicken such lines with `tex.dilate`.

**Moving obstacles.** A mask that moves without `obstacle_velocity` pushes the liquid only by
taking its place. A slow paddle moves a pool almost as well that way. A fast paddle leaves
bubbles behind it and makes smaller waves than with its velocity. The liquid moves at most about
5 heights per second at the default grid and 60 fps, so a faster obstacle is not followed in
full.

**friction** sets how much the liquid sticks to obstacles and to the picture's edges. 0 slides
freely. At 1 a thin film on a slope of 24 degrees slides about a sixth as far as without
friction. Deep liquid still runs down over the film.

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
| 256 | 16384 | 14 | 0.4 ms |
| 384 (default) | 65536 | 21 | 1.7 ms |
| 512 | 131072 | 28 | 4.0 ms |

Obstacles cost about 0.35 ms more at the default grid in each frame where an input changes. With
a still mask and still emitters they cost nothing after the first frame.

For the same amount of liquid, twice the cells across need four times the particles and twice
the substeps: about eight times the cost. A show at 30 fps takes twice the substeps per frame.
A thick liquid (`viscosity` above 0.001) or a fast pour adds substeps. A frame runs at most 128 substeps; beyond that the liquid moves slower
than the show, and the node warns.

## Time and state

The liquid is kept from frame to frame and follows the show clock: a paused show stands still.
A late frame is one longer step of at most 1/30 s, so a stall does not make the liquid jump.
`speed` slows the liquid down or speeds it up. `reset`, `node.reset` or R start over from the
`fill` picture. A new `grid`, or a picture of another shape, starts over too. The liquid is not
kept in the state snapshots: after a restart it starts from `fill`.

## Outputs

- **out**: the liquid in `color`, transparent around it.
- **velocity**: the liquid's velocity per cell, as in `tex.fluid`.
- **density**: how full each cell is, 1 at rest.

## What it cannot do yet

- Bodies from `physics.rigid_bodies` as obstacles: wire a picture of them into `obstacles`, but
  their velocity has to come from somewhere else for now.
- One liquid: no sand, snow or a second liquid of another colour.
- Drawing: `out` is a plain outline of where the liquid is. Thin streams look banded or break up
  into dots.
- Forces from a picture or a table, as `tex.fluid` takes them.
