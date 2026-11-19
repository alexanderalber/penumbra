# Particles

`pts.particles` makes points that are born, move, age and die: sparks, a fountain, dust that
swirls round an object. It is penumbra's counterpart to TouchDesigner's Particle POP. Forces
come from nodes in its loop: `pts.force` (attract, repel, a vortex, gravity, wind), `pts.noise`
on `force`, anything that changes `velocity`. `pts.field` gives the points a `weight` from a
shape, which masks a force. `pts.feedback` closes a loop of any points, without births and
deaths.

## The smallest network

1. `pts.generate`: the points particles are born on. A small `circle` with `fill` `inside`
   gives a fountain, a `sphere` a burst in every direction.
2. `pts.particles`, its `source` wired from the generator. Set `rate` (births a second), `life`
   (seconds), `velocity` and `spread`. `gravity` [0, 0, -9.81] makes them fall: z is up.
3. `geo.from_pts`, then a material (`mat.constant` through `geo.assign_material`), a light and
   a camera into `tex.render`, as for any geometry.

The viewer on `pts.particles` shows how many particles live, a frame late: "15024 of at most
15130 points". "At most" is the count read back a frame or two ago plus the births since: the
work over the particles covers that many. `max` is the most particles at once. While that many
live, the births of a step that do not fit are dropped, not saved for later.

### Emit from a model or a text

`pts.from_geo` with `mode` `surface` scatters points evenly over a mesh, each with the surface's
normal there: wire `geo.text` or `geo.file` into it and it into the particles' `source`. Set
`along_normal` on the particles to throw them off the surface. The points are where
`tex.render` would draw the mesh, its placement applied. `count` is how many places there are
to be born on; `rate` how many are born a second. Several emitters go into one `source`
through `pts.merge`.

### Soft, glowing particles

`geo.from_pts` draws each particle as a sprite. `softness` 1 fades it from its centre to its
rim, a soft dot; `blend` `add` adds its colour to what lies behind, so dense places glow
brighter. That is the usual look for sparks and fire: a `mat.constant` of a dim colour (the
sprites add up), `softness` 1, `blend` `add`. Added sprites write no depth and cast no shadow.
They cover nothing either: where only they are, the render stays see-through, and
`tex.composite` over a background adds their glow to it.
A disc costs what a square costs: a million sprites draw in about 2 ms at 1920 x 1080 with 4x
antialiasing on an RTX 3090, soft and added ones in about 1.2 ms.

## A force in the loop

Wire the particles into the forces, and the last force into a `pts.null`. Then wire the null
back into the particles' `previous` input:

```
pts.generate -> pts.particles -> pts.noise -> pts.force -> pts.null --+
                     ^ previous                                       |
                     +------------------------------------------------+
```

- **`pts.noise`** with `attribute` `force` and `flavour` `curl` stirs the particles like smoke.
- **`pts.force`** adds to `force`. `kind` `vortex` turns them around `axis`; `attract` pulls them
  to `center`. `falloff` and `radius` make it weaker with distance.
- **The null** is the end of the loop: TouchDesigner calls it the target. Draw from it, so the
  picture shows the forces of this frame.

Each step `pts.particles` starts from what the null held a step ago. It gives `force` out as 0,
and the loop adds the forces of this step. A particle gains `force / mass` in speed each second,
loses some to `drag`, and moves by its speed. The loop must keep the particles as they are: a
`pts.delete` in it is refused with a warning. To end particles from the loop, write a number
above 0 into the attribute `kill` (`pts.math`, result `kill`, expression `position.z < 0.0`).

**A mask:** a `pts.field` before the `pts.force` writes `weight`, 1 inside its shape and 0
outside. The force multiplies by `weight`, so it acts only there.

**Wind** pulls each particle towards the wind's speed. However light the particle, it never
passes that speed: `pts.particles` steps the wind's part of the speed so that it cannot
overshoot. A negative strength pushes nothing.

### Two traps

- **What the loop writes stays.** Every attribute but `force` comes back into the next step as
  the loop left it. A `pts.field` with `combine` add, or a `pts.math` that adds to an
  attribute, adds again at every step: the value grows frame after frame. Set the value
  (`combine` set, an expression that does not read the attribute), or reset it at the start of
  the loop.
- **An inherited `weight` masks every force.** The particles take every attribute of their
  source point. Points from `pts.from_tex` carry `weight` (the picture's alpha), so a force
  with the default `mask` acts on each particle as strongly as its birth texel was opaque, and
  not at all where it was transparent. Clear `mask` on the force, or write `weight` in the
  loop.

## Colour over life

Every particle has `age` (seconds since its birth) and `life` (the seconds it lives). `age /
life` runs from 0 at birth to 1 at death. A `pts.math` after the particles, `result` `color`:

```
vec4f(mix(vec3f(1.0, 0.95, 0.6), vec3f(0.95, 0.08, 0.02), age / life), 1.0)
```

turns them from pale yellow to red. The same works for `scale`: `1.0 - age / life` shrinks them
as they age.

**Random per particle:** `rand_key(id, 0u)` gives each particle its own number in 0..1 that
stays with it. `rand(0u)` is keyed by the place in the list, and the place changes when older
particles die. `pts.random` and `pts.delete` by random follow the `id` by themselves.

## Trails

`pts.trail` puts a line behind every particle through the places it had over the last frames.
TouchDesigner's Trail POP with a Line MAT does the same.

```
pts.particles -> pts.trail -> pts.math -> geo.from_pts (mode lines)
```

1. **`pts.trail`** after the particles (or after the null at the end of their loop). `length`
   is how many places a trail keeps, the particle's own place included. `every` 2 keeps every
   second frame: the trail reaches twice as far back. Trails find their particle by `id`, so a
   trail stays with its particle while others die.
2. **`pts.math`** with `result` `width` or `color` shapes the trail. Each trail point has
   `trail`: 0 at the particle, 1 at the oldest place. `1.0 - trail` thins the trail to its tail;
   `vec4f(mix(vec3f(1.0, 0.9, 0.5), vec3f(0.6, 0.1, 0.02), trail), 1.0)` fades it from yellow to
   dark red.
3. **`geo.from_pts`** with `mode` `lines` draws each trail as a ribbon that faces the camera.
   `width` is in metres; the points' `width` attribute multiplies it.

A new particle starts with a trail of its birth place, which grows from there. A trail ends with
its particle. The trail carries `position`, `id`, `trail` and the attributes in `attributes`
(`color width scale` by default): each place keeps the value the particle had then. Trails cost
`length` points per particle: 100 000 particles with trails of 32 are 3.2 million points.

## Lines between near points (plexus)

`pts.proximity` joins each point to its nearest neighbours with lines: the plexus look,
TouchDesigner's Proximity POP.

```
points -> pts.proximity -> pts.math -> geo.from_pts (mode lines)
     \-> geo.from_pts (sprites) ------------------------------> geo.merge
```

1. **`pts.proximity`** after any points: particles, a generator with `pts.noise` that drifts,
   points from a picture. `distance` is the longest line in metres, `max_lines` how many lines
   each point starts. A pair is one line.
2. **`pts.math`** fades the long lines. Each line end has `span` (the line's length in metres)
   and `span01` (the length over `distance`, from 0 to 1). `result` `color`,
   `vec4f(color.rgb * (1.0 - span01), 1.0)` darkens a line as it grows; `result` `width`,
   `1.0 - span01` thins it. On a black background a dark line disappears.
3. **`geo.from_pts`** with `mode` `lines` draws the lines, a second one with `sprites` the
   points; `geo.merge` puts both into one render.

**Steady lines:** `choose` `nearest` joins the nearest points; a line switches only when two
distances change order. `choose` `random` gives each pair a number that stays with it, so lines
come and go only as points come within `distance` or leave it.

**Cost:** about 3.6 ms a frame for a million moving points with 1.8 million lines, plus about
2.5 ns a line to draw (RTX 3090). `max` (a million lines by default) caps the lines; the node
warns when it cuts some.

## A flock (boids)

`pts.neighbor` gives each point what its neighbours within `distance` add up to: `neighbors`
(how many), `cohesion` (the way to their middle), `separation` (a push away from those nearer
than `separation`) and `near_velocity` (their mean speed, from `average`). These are the three
rules of a flock. In the particle loop, a `pts.math` with `result` `force` steers by them:

```
pts.particles -> pts.neighbor -> pts.math (force) -> pts.null --+
      ^ previous                                                |
      +---------------------------------------------------------+
```

```
6.0 * separation + 1.5 * (near_velocity - velocity) + 1.2 * cohesion - 0.35 * position
```

The first three terms keep the birds apart, fly them alike and keep them together;
`- 0.35 * position` pulls the flock back to the middle. Particles with a long `life` and no
`gravity` make a flock that lives on. `average` `color` blurs colours over neighbours instead.
For rules of your own, `pts.shader` with `// @neighbors` walks the neighbours in a program
(see [Shaders](shaders.md)).

## Time

One step per frame, as long as the frame. While the show stands still the particles stand
still. Stepped frame by frame (`cook.step`), the same steps give the same particles. `reset`
starts over, as does `node.reset` and the `reset` of a component the particles sit in.
`warm_up` simulates that many seconds ahead at the start, without the loop's forces. While the
show runs this takes about 4 ms of each frame, and no particles show until it is done; stepped,
it all happens in the first frame. Stepped frames also wait for every program the loop's nodes
compile, so a script needs no `wait` before `cook.step`.
`pts.trail` follows the same clock: a place per frame, none while time stands, `reset` and
`node.reset` start the trails over.

## What each particle carries

`id` (a number no other particle of the run had; numbers start again at 0 after a reset, and
after about four billion births),
`age`, `life`, `velocity`, `force`, `position`, and every attribute of its source point:
`color`, `scale`, `normal`, `uv`. Particles stay in the order they were born, oldest first.
A `mass` or `drag` attribute, from the source or from a node in the loop, takes the place of the
param for that particle.
