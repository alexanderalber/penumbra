# rigid-bodies

`physics.rigid_bodies` moves bodies that fall, collide and pile up. Here 25 balls and 50 boxes
stand as rings in the air. Every 16 seconds they fall onto a ramp for 8 seconds, then fly back
into the rings. A yellow paddle shoves the heap up the ramp.

Look at `/out`. The rings stand for the first 3 seconds, then the bodies fall.

- `/balls` and `/boxes` are the two groups, each a `physics.bodies`. `/ball_places` and
  `/box_places` have one row per body. `/ball_look` and `/box_look` are the models with their
  materials. Both groups go into `/solver`, so balls and boxes collide with each other.
- Each table of places is wired into `table` and into `targets` of its group. The bodies start
  on the rings, and a spring pulls each body back to its place there. `pull` on the group
  switches the spring: 1 holds the rings, 0 lets the bodies fall.
- `/form` is a square `chan.lfo` between 0 and 1. `pull` of both groups is bound to it.
- `/paddle` is a group with `mode` `driven`. Its one body goes where its table says and pushes
  what is in its way. `/sweep` (an LFO named `tx`) and `/height` (a constant named `tz`) are
  merged, and `/paddle_row`, a `table.from_chan`, turns them into a table of one row.
- `/ground` is the floor and the ramp. It goes into `obstacles` of the solver and, with a
  material, into `/scene`.
- `/attractor` is a `physics.field` that pulls every body towards a point above the ramp. Its
  `strength` is 0, so it does nothing until you raise it.
- `/out` renders `/scene` through `/camera`, lit by `/sun` and `/sky`.

The solver's `damping` and `spin_damping` are 0.5. Without them the balls roll away for ever.

Try: `strength` on `/attractor` (15 gathers the fallen bodies into one clump, -15 blows them
apart). `rate` on `/form` (how often the rings form). `stiffness` on `/balls` or `/boxes` (400
snaps the bodies into place, 4 lets them drift there). `bounce` on `/balls`. `amplitude` and
`rate` on `/sweep` (a wider or faster paddle). `reset` on `/solver` puts every body back to its
start.

Not in this example: obstacles that move, a kick on a trigger, a push per body, hulls of
models, fixed groups and a tracker that moves bodies. The pages of
[physics.rigid_bodies](../../docs/nodes/physics.rigid_bodies.md),
[physics.bodies](../../docs/nodes/physics.bodies.md) and
[physics.field](../../docs/nodes/physics.field.md) describe them.
