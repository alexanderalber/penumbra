## What it is for

A `physics.bodies` is one group of bodies for a `physics.rigid_bodies`. A table places the
bodies, a model shows them. The params say who moves them (`mode`), what they collide as
(`shape`) and what they are made of. Two more tables steer the bodies while they move: `forces`
pushes each body, `targets` pulls each body to a place.

The page of [physics.rigid_bodies](../reference/nodes.md#physicsrigid_bodies) starts with a pile
in five nodes.

## The table of a group

Any table can place the bodies. Every column is optional, but the first row must name at least one:

| column | meaning | when the column is missing |
|---|---|---|
| `tx`, `ty`, `tz` | where the body starts, in metres; z is up | 0 |
| `rx`, `ry`, `rz` | how it is turned at the start, in degrees about x, then y, then z | 0 |
| `sx`, `sy`, `sz` | its size, times the model's | 1 |
| `vx`, `vy`, `vz` | its speed at the start, in metres per second (`mode` `dynamic` only) | 0 |
| `wx`, `wy`, `wz` | its spin at the start, in degrees per second about x, y and z (`mode` `dynamic` only) | 0 |

A row with a cell that is not a finite number is left out, and `physics.bodies` says so in a
warning. The solver takes 4000 bodies at most, counted over all groups. Bodies beyond that are
left out, and the solver names the group they belong to.

Bodies that overlap at the start are pushed apart in the first frames, and fast. Leave a gap
between them.

## Bodies that follow a table

`mode` on `physics.bodies` says who moves the bodies of a group:

- **`dynamic`:** the solver. The table is the start. From then on the bodies fall and collide.
- **`driven`:** the table. On every frame each body goes where its row is now. On its way it
  pushes dynamic bodies aside, as fast as it moves itself. Nothing pushes it back, and gravity
  does not pull it.
- **`fixed`:** nobody. The bodies stand where the table puts them, and others land on them. A
  change to the table puts them at their new place at once, without a push.

A driven group is a hand in the pile: a paddle moved by an LFO bats balls away, a person in front
of a camera shoves boxes aside. A fixed group is scenery to rearrange: shelves, pegs, a funnel.

**Rows and bodies.** The first row is the first body, the second row the second. A new row at the
end adds a body there. A row that goes takes the last body away. Keep each thing in its row from
frame to frame. When two rows swap, the two bodies travel to each other's places.

**The columns.** `tx ty tz`, `rx ry rz` and `sx sy sz` count. A size that changes resizes the body
at once. A body that grows pushes its neighbours out and does not throw them. `vx vy vz` and
`wx wy wz` do nothing: the speed of a driven body is the way its row moved since the last frame.
`density` does nothing either.

**Jumps.** A row that moves a long way in one frame would throw everything in between. `jump` on
the solver sets the limit, 0.5 m at first. A body whose row moves farther is put there at once and
pushes nothing. Raise `jump` for a fast paddle. Lower it for a tracker that loses a blob and finds
it elsewhere.

**Fast and thin.** The solver looks where the bodies are once per step. A driven body that moves
farther than it is thick in one frame can pass a small body without touching it. Raise `substeps`,
or make the driven body thicker.

**Time.** While `speed` is 0, a driven body still goes where its row is, and pushes nothing. With
`speed` below 1 a driven body hits harder: it covers the same way in less of the bodies' time.

## A driven group from channels

`table.from_chan` turns channels into the table of a group. Channels with one value each give one
row: the channels `tx`, `ty` and `tz` place one body. A channel that is missing counts as 0.

A paddle that an LFO swings through the pile:

1. `chan.lfo` with `name` set to `tx`, `rate` 0.1 and `amplitude` 4: the paddle goes 4 m to each
   side, once in ten seconds.
2. `chan.constant` with `name0` set to `tz` and `value0` 0.4: the height of the paddle's middle.
3. `chan.merge`: the LFO into `in0`, the constant into `in1`.
4. `table.from_chan`: the merge into `in`. Its table has the columns `tx` and `tz` and one row.
5. `physics.bodies` with `mode` set to `driven`: the table into `table`, a `geo.primitive` box
   of 0.3 x 3 x 0.8 m into `model`.
6. That group into the next free `bodies` input of the solver.

## A tracker's blobs as a driven group

`tex.blob_track` gives places in the picture, from 0 to 1. Its `chans` output keeps each blob in
a slot: `id1`, `x1`, `y1` for the first slot, `id2`, `x2`, `y2` for the second. A group needs
`tx ty tz` in metres and one row per blob. Channel nodes turn one into the other:

1. `chan.select` with `channels` set to `x* y*`, fed by the tracker's `chans`.
2. `chan.math` with `op` `subtract` and `b` 0.5: the middle of the picture becomes 0.
3. `chan.constant` with `rows` 2: `name0` `x`, `value0` 4, `name1` `y`, `value1` -3. This is
   the room the picture covers, 4 m wide and 3 m deep. y is negative since it runs downwards in
   a picture.
4. `chan.math` with `op` `multiply`: step 2 into `a`, the constant into `b`. Every `x` channel
   meets `x`, every `y` channel meets `y`.
5. `chan.rename` with `names` set to `x*=tx* y*=ty*`.
6. `chan.constant` with `name0` `tz` and `value0` 0.4: the height of the hands.
7. `chan.merge`: step 5 into `in0`, the tracker's `chans` into `in1`, step 6 into `in2`.
8. `table.from_chan` with `layout` set to `numbered` and `keep` set to `id`. The number at the
   end of a name picks the row, so each slot is one row. `tz` has no number and fills every row.
   `keep` leaves out the slots whose `id` is 0, the ones without a blob.
9. `physics.bodies` with `mode` set to `driven`: the table into `table`, a model for the hand
   into `model`, such as a `geo.primitive` sphere.
10. That group into the next free `bodies` input of the solver.

A blob that appears adds a body. It is put there without sweeping through the scene. When a blob
leaves, its row goes and the rows after it move up: each of their bodies takes the place of the
next blob. Such a body jumps there without a push when the two blobs are farther apart than
`jump`. Lower `jump` to 0.2 m if people come close to each other.

## A push per body

Wire a table into `forces`. Row 1 pushes the first body of the group's table, row 2 the second,
on every frame:

| column | meaning |
|---|---|
| `fx`, `fy`, `fz` | the speed the body gains per second along x, y and z, in metres per second squared |
| `mx`, `my`, `mz` | the spin it gains per second about x, y and z, in degrees per second squared |

A change to this table never puts the bodies back to their start. Change it on every frame if
you like: make it from channels with `table.from_chan`.

The rows need not match the bodies. Bodies beyond the last row are not pushed. Rows beyond the
last body do nothing. A row with a cell that is not a number pushes nothing, and the node says so
in a warning.

A heavy body gains speed as fast as a light one, as with a `physics.force`. Only the bodies of a
`dynamic` group are pushed.

**Each body lifted by its own channel.** Channels named `fz1`, `fz2`, `fz3`, each between 0 and
30, into a `table.from_chan` with `layout` set to `numbered`. Its table has the column `fz` and
three rows. Wire it into `forces`: each channel lifts its body when it passes 9.81.

## Bodies pulled into a figure

Wire a table into `targets`, with the columns `tx ty tz` and one row per body. A spring pulls
each body to the place in its row. The bodies fly there, collide on their way and form the figure
the places draw: a ring, a grid, a word.

- **`pull`** switches the spring. At 1 the bodies are held at their targets. At 0 they are let go
  and fall. Bind it to a channel.
- **`stiffness`** is how hard the spring pulls. 40 brings a body home in about a second, 400 in a
  quarter of one.
- **`pull_damping`** is how much the spring brakes. At 1 a body arrives without swinging past.
  Below 1 it swings about its target first.

A pulled body feels less of gravity, by the share `pull` says. At `pull` 1 the figure stands
exactly where the targets are, however soft the spring.

A spring does not hold a body rigidly. Another body, a driven paddle or a force field pushes it
off its place, and it comes back. This is the soft sibling of `mode` `driven`, where nothing
pushes a body off its row.

A change to the targets never puts the bodies back to their start. They go to their new places
from where they are. Bodies without a row are not pulled. A row with a cell that is not a number
pulls nothing. Only the bodies of a `dynamic` group are pulled. The spring pulls a body to its
place only: how the body is turned is left to it.

**A figure that forms and falls apart.**

1. `table.text` with `format` `csv`: `tx,ty,tz` and one row per body, the places of the figure.
   Leave at least a body's width between two places.
2. That table into `targets` of the `physics.bodies`.
3. `chan.lfo` with `shape` `square`, `amplitude` 0.5, `offset` 0.5 and `rate` 0.1. Bind `pull`
   to it: five seconds the figure, five seconds the heap.

## What a body collides as

`shape` on `physics.bodies` decides what the solver makes of the group's model:

- `hull`: the model with its dents filled in. A cup collides as a solid lump, and nothing falls
  into it.
- `box`: the box around the model. Boxes stack well and cost little.
- `sphere`: a ball as wide as the model's longest side. With a scale that differs per axis it stays
  a ball, as wide as the largest of the three.

A flat model, such as a plane, has no hull. It collides as the box around it, and `physics.bodies`
says so in a warning.

A ball on the floor rolls on for ever, since nothing slows a rolling ball down. Set `damping` and
`spin_damping` on the solver, to 0.3 or so, and the balls come to rest.

## What a body is made of

Each group has its own `friction`, `bounce` and `density`. The solver has `friction` and `bounce`
for the floor and the obstacles.

- **`friction`:** where two things touch, the mean of their two values counts. Ice (0) on a floor
  of 0.5 slides with 0.25.
- **`bounce`:** the larger of the two values counts. A ball with `bounce` 0.8 bounces off a floor
  with `bounce` 0, and off every other body.
- **`density`:** how heavy a body is for its size. Only the ratio between groups shows. A body of
  density 10 pushes a body of density 1 aside. A body twice as large weighs eight times as much.
