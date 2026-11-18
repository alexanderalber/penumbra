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
20000 points". `max` is the most at once; while that many live, births wait.

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
particles die.

## Time

One step per frame, as long as the frame. While the show stands still the particles stand
still. Stepped frame by frame (`cook.step`), the same steps give the same particles. `reset`
starts over, as does `node.reset` and the `reset` of a component the particles sit in.
`warm_up` simulates that many seconds at once at the start, without the loop's forces.

## What each particle carries

`id` (a number no other particle of the run had; numbers start again at 0 after a reset, and
after about four billion births),
`age`, `life`, `velocity`, `force`, `position`, and every attribute of its source point:
`color`, `scale`, `normal`, `uv`. Particles stay in the order they were born, oldest first.
A `mass` or `drag` attribute, from the source or from a node in the loop, takes the place of the
param for that particle.
