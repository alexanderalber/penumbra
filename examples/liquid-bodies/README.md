# liquid-bodies

Boxes and balls from `physics.rigid_bodies` drop into a pool of `tex.liquid`, splash and sink to
the floor. Every 16 seconds they are lifted out and dropped again. A turning bar stirs the water
at the right. The bodies push the water as they move and turn.

Look at `/out`. The bodies hang above the pool for the first 3 seconds, then fall.

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
- `/bodies` renders the solver's `out` through `/camera`. `/out` lays `/liquid` over it, the sky
  and the walls in grey.

**The water does not push the bodies back.** A body falls through the pool as through air and
lands on the floor. `damping` on `/solver` is 8: every body loses that share of its speed per
second, in the air as well as in the water. A falling body then reaches at most 1.2 m/s. Faster
bodies hit the floor hard, and the water squeezed out under them spurts up to the top of the
picture. The pool is 1.35 m deep, and the bodies hang just above it.

Try: `damping` on `/solver` (1.5 lets the bodies fall fast), `rate` on `/turn`, `rate` on
`/form`. Unwire `obstacle_velocity` of `/liquid`: the bodies then only take the water's place.

It costs about 5.4 ms per frame on an RTX 3090 (power limit 93 %), measured headless at 60 fps
with a release build. The liquid alone, with the walls and no bodies, takes about 4.2 ms. The
solver's two pictures take about 0.7 ms. The rest is the liquid taking in a new obstacle picture
on every frame.

Not in this example: the water pushing the bodies (they neither float nor slow down in it), bodies
that turn out of the wall, and sand. The pages of
[tex.liquid](../../docs/nodes/tex.liquid.md#rigid-bodies) and
[physics.rigid_bodies](../../docs/nodes/physics.rigid_bodies.md) explain the pictures.
