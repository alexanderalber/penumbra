# liquid-bodies

Boxes and balls from `physics.rigid_bodies` drop into a pool of `tex.liquid`, splash, float and
are lifted out again. A turning bar stirs the water at the right. The bodies push the water as
they move and turn.

Look at `/out`. The bodies hang above the pool for the first 3 seconds, then fall. Every 16
seconds they are lifted out and dropped again.

- `/boxes` and `/balls` are two groups of `/solver`, a `physics.rigid_bodies` with `plane` `xz`:
  the bodies stay on the wall that `/camera` looks at. Each table of places goes into `table`
  and `targets` of its group. `pull` on both groups is bound to `/form`, a square `chan.lfo`:
  8 seconds held above the pool, 8 seconds free.
- `/stirrer` is a group with `mode` `driven`: one bar that goes where its table says.
  `/turn`, a saw `chan.lfo` named `ry`, turns it at 72 degrees a second.
- `/camera` is orthographic and looks along +y. It sees 3 m of height, so the liquid's default
  `gravity` of 3.27 heights per second squared is the earth's.
- `/solver` draws the bodies through `/camera` as two pictures at the liquid's size: `mask`
  and `velocity`. `velocity` goes into `obstacle_velocity` of `/liquid`.
- `/left` and `/right` place `/wall` twice, and `/walls` merges them. `/walls` goes into
  `obstacles` of `/solver`, so the bodies stay in the basin. `/walls_mask` draws the same walls
  white through `/camera`. `/solid` puts `/walls_mask` and the bodies' `mask` together and goes
  into `obstacles` of `/liquid`.
- The liquid does not push the bodies back. `/buoyancy`, a `physics.field` of type `wind`,
  stands in for that: it pushes up with 14 m/s² below the water's surface, more than gravity,
  so the bodies float. Its ball of 20 m reaches up to about 1 m across the basin, the pool's
  surface. `damping` 1.5 on `/solver` slows the bodies as water would, and in the air as well.
- `/bodies` renders the solver's `out` through `/camera`. `/out` lays `/liquid` over it, the sky
  and the walls in grey.

Try: `strength` on `/buoyancy` (8: the bodies sink slowly; 0: they hit the floor at full speed,
and the water they squeeze out spurts up to the top of the picture), `rate` on `/turn`, `damping` on `/solver`. Unwire
`obstacle_velocity` of `/liquid`: the bodies then only take the water's place.

It costs about 4.4 ms per frame on an RTX 3090 (power limit 75 %), measured headless at 60 fps
with a release build. The liquid alone, with the walls and no bodies, takes about 3.2 ms. The
solver's two pictures take about 0.6 ms. The rest is the liquid taking in a new obstacle picture
on every frame.

Not in this example: the water pushing the bodies (the buoyancy above is a stand-in, the same
everywhere below the surface, wherever the water is), bodies that turn out of the wall, and
sand. The pages of [tex.liquid](../../docs/nodes/tex.liquid.md#rigid-bodies) and
[physics.rigid_bodies](../../docs/nodes/physics.rigid_bodies.md) explain the pictures.
