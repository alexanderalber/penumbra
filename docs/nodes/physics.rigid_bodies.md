## A pile of boxes in five nodes

1. `table.text` with `format` set to `csv`: the first row names the columns, each further row is
   one body. `tx,ty,tz` and three rows `0,0,2`, `0.2,0,3.5`, `-0.1,0.1,5` drop three bodies from
   2 m, 3.5 m and 5 m.
2. `geo.primitive`, shape `box`: what a body looks like.
3. `physics.bodies`: the table into `table`, the model into `model`. This is one group of bodies.
4. `physics.rigid_bodies`: the group into `bodies0`.
5. `tex.render`: the solver's `out` into `geo`.

The bodies fall onto the floor at z = 0 and come to rest. The floor is part of the solver and is
not drawn. Put a flat `geo.primitive` there if it should show.

Step 2 can be left out. A group without a model is drawn as boxes of 1 m.

## Several groups in one world

Every group needs its own `physics.bodies`. Wire each into the next free `bodies` input of the
solver: a new input appears when the last one is taken. Bodies of all groups collide with each
other, so balls roll off boxes and boxes pile up on balls.

A group has one model, one collision shape and one material. Balls and boxes are two groups.
So are wooden boxes and rubber boxes.

The solver's `out` draws every body as the model of its group, with the material that model
carries (`geo.assign_material` before `physics.bodies`). One wire into `tex.render` shows all
groups.

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

## What starts a group over

A change to a group's table puts the bodies of that group back to their start. So does a change to
the form of its model, and to `shape`. The other groups go on from where they are. A group that
starts over can land inside bodies that have moved into its place. The solver pushes them apart.

This holds for a group whose `mode` is `dynamic`. A `driven` or `fixed` group follows its table
instead, see below. A switch of `mode` puts the bodies of the group where the table has them.

`friction`, `bounce` and `density` of a group change while its bodies move. So does the material
of its model.

`reset` on the solver and the reset button of the solver put every group back to its start.

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

## The outputs

- **`out`** is geometry: every body where it is now. Wire it into `tex.render`, or into
  `geo.merge` together with the scenery.
- **`table`** has one row per body: `tx ty tz rx ry rz sx sy sz`, then `group`. `group` is the
  number of the input the body came in by, 0 for `bodies0`. The rows follow the inputs in order,
  and inside a group the rows of its table.

With one group, `table` goes straight into `geo.instance`. With several groups `geo.instance`
would draw one model for all of them: use `out` instead.

## Obstacles

Wire any geometry into `obstacles`: a model from `geo.file`, primitives placed by `geo.transform`,
several joined by `geo.merge`. Bodies collide with its triangles, so they fall into a bowl and roll
down a ramp. An obstacle is a thin shell with nothing inside. A fast or small body can pass through
it: raise `substeps`, or make the bodies larger or slower.

Obstacles may change while the bodies move, and bodies that rested on one fall on when it goes.

**Obstacles that move.** An obstacle that is moved or turned pushes the bodies in its way and
carries along those that lie on it. Put a `geo.transform` before `obstacles` and bind its
`translate` or `rotate` to a channel: a wall shoves a pile across the floor, a turning bar bats
balls away, a platform lifts what stands on it.

**What still jumps.** An obstacle whose size changes (`scale`, `uniform`) or whose mesh changes is
built anew where it is now. It pushes nothing, and it can leave bodies stuck inside itself. So
does an obstacle that moves farther than `jump` in one frame, 0.5 m at first. `jump` measures
the way of the obstacle's own origin: a long bar turning fast about its middle never jumps, and
its ends can pass small bodies without touching them. Raise `substeps` then.

For moving things with a simple form, a `driven` group is the cheaper way: a box or a ball costs
less than a mesh.

Copies made by `geo.tex_instance` are no obstacles, since their places exist on the GPU only.

## Time

The bodies move one step per show frame, as long as something shows the node's result. `speed`
slows time down or holds it at 0. A frame that takes longer than a twentieth of a second counts as
a twentieth, so the bodies slow down instead of jumping.

The solver runs on one core of the CPU. A few hundred boxes cost little. Hulls of detailed models
and obstacles of many triangles cost more, and thousands of bodies in one pile can take longer
than a frame.
