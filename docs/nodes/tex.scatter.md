## What it is for

`tex.scatter` writes points into a picture. Each texel of the `positions` input is one point.
The point adds 1 into the pixel at its x and y. Many points on one pixel add up. A shader cannot
do this: it writes only its own pixel.

Typical uses: particles or agents that leave a trail, grains that pile up, a count of where
things are.

## Positions

Red is x, green is y. Blue and alpha are free for the simulation's own state.

- **`units` `pixels`:** pixels from the top left corner, y down. A point at (10.7, 3.2) adds into
  pixel (10, 3). A `tex.shader` that reads the picture with `textureLoad(in1, vec2i(pos), 0)`
  reads the same pixel.
- **`units` `share`:** 0 to 1 across and 0 to 1 down, for positions that do not depend on the size.

Make the positions with a `tex.shader` with `format` 32f. Then a position keeps fractions of a
pixel on any picture size. In half floats, positions above 1024 round to half pixels and above
2048 to whole ones.

## Values

Without `values`, each point adds `value` to r, g, b and a (1 by default). With `values`
connected, each point adds the texel of `values` at the same place as its position: a picture
of the same size as `positions`. Values that are not a finite number add 0.

## Dead points and the edge

- A point whose x or y is not a finite number adds nothing.
- `dead` `blue` or `alpha`: a point with that channel below 0 adds nothing. Use it for particles
  that are not born yet or have died.
- `edge` `drop`: a point beyond the picture adds nothing. `wrap`: it comes back in from the
  opposite side, for a simulation on a torus. `clamp`: it adds into the nearest edge pixel.

`splat` above 1 makes each point add into a square of pixels around its pixel. Even sizes put
the extra column and row right and below.

## Exact sums

The picture starts at 0 every frame. In `format` 32f the sums are exact up to 16 777 216 per
pixel: 100 000 points on one pixel give 100 000. `chan.from_tex` with `measure` `sum` then counts
the points exactly. A GPU that cannot add into 32-bit floats writes 16f instead and warns. Counts
are exact there up to 2048 per pixel; the sum of a pixel stops growing at 2048.

## Cost

The node draws one point per texel of `positions`. On an RTX 3090, a million points cost about
a quarter of a millisecond per frame. The picture size hardly matters. `splat` multiplies the
cost by its square.

## Recipe: particles in a texture, deposit into a trail

1. `seed`: a `tex.shader` that writes the first state, one particle per texel (rg: position in
   pixels, b: heading, a: 1 alive or -1 dead). `format` 32f.
2. `agents_loop`: a `tex.feedback` with `seed` as `initial`.
3. `agents`: a `tex.shader`, `format` 32f, in0 `agents_loop`, in1 `trail_loop`. It reads the
   trail around each particle, turns and moves it, and writes the new state.
   `agents_loop`'s `previous` is `agents`.
4. `deposit`: `tex.scatter` with `positions` `agents`, `width` and `height` the trail's size,
   `edge` `wrap`, `dead` `alpha`.
5. `trail`: a `tex.shader`, `format` 32f, in0 `trail_loop`, in1 `deposit`. It blurs and fades
   the trail and adds `deposit` to it. `trail_loop` is a `tex.feedback` with `trail` as
   `previous`.

A display shader on `trail` colours it. The deposit is drawn between two steps of the agents.
So `iterations` on `agents` runs several moves per deposit, not several moves each with its
own deposit.
