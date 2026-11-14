## A pile of boxes in six nodes

1. `table.text` with `format` set to `csv`: the first row names the columns, each further row is
   one body. `tx,ty,tz` and three rows `0,0,2`, `0.2,0,3.5`, `-0.1,0.1,5` drop three bodies from
   2 m, 3.5 m and 5 m.
2. `geo.primitive`, shape `box`: what a body looks like.
3. `physics.bodies`: the table into `table`, the model into `model`. This is one group of bodies.
4. `physics.rigid_bodies`: the group into `bodies0`.
5. `tex.render`: the solver's `out` into `geo`.
6. `view.camera` into `camera` of the `tex.render`. Without a camera the render shows nothing
   and says so.

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

The shipped example `rigid-bodies` has balls and boxes as two groups, a driven paddle as a
third, a ramp as an obstacle and a `physics.field` to switch on.

## The groups

A `physics.bodies` is one group. Its page has the columns of the table, the modes (`dynamic`,
`driven`, `fixed`), the collision shapes, the materials, a push per body and the spring that
pulls bodies into a figure: [physics.bodies](../reference/nodes.md#physicsbodies).

## What starts a group over

A change to a group's table puts the bodies of that group back to their start. So does a change to
the form of its model, and to `shape`. The other groups go on from where they are. A group that
starts over can land inside bodies that have moved into its place. The solver pushes them apart.

This holds for a group whose `mode` is `dynamic`. A `driven` or `fixed` group follows its table
instead. A switch of `mode` puts the bodies of the group where the table has them. The tables on
`accelerations` and `targets` of a group never start it over.

`friction`, `bounce` and `density` of a group change while its bodies move. So does the material
of its model.

`reset` on the solver and the reset button of the solver put every group back to its start.

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
the way of the obstacle's own origin. A turn counts on its own: an obstacle that turns by more
than 90 degrees in one frame jumps too, at any `jump`. A long bar that turns fast, but by less
than that, pushes with its full speed, and its ends can pass small bodies without touching them.
Raise `substeps` then. With `jump` at 0 no obstacle and no driven body pushes, whether it moves
or turns.

An obstacle may be flat: a `geo.transform` with a `scale` of 0 along one axis gives a sheet, and
the bodies land on it where it is drawn.

For moving things with a simple form, a `driven` group is the cheaper way: a box or a ball costs
less than a mesh.

Copies made by `geo.tex_instance` are no obstacles, since their places exist on the GPU only.

## Fields

Gravity pulls every body the same way. A `physics.field` adds a field of its own: a pull to a
point, a wind, a whirl, or a kick on a trigger. Wire it into a `field` input of the solver. Each
further field takes the next `field` input, 64 fields in all at most.

A field moves the dynamic bodies of every group. `strength` is the speed a body gains per second,
so a large or heavy body moves as readily as a small or light one. Driven and fixed bodies and
obstacles do not feel a field.

A point that pulls the pile to where an LFO puts it:

1. `physics.field` with `type` `point` and `strength` 12, into `field0` of the solver.
2. `chan.lfo` with `rate` 0.1 and `amplitude` 3. Bind the x of the field's `position` to it.
3. `damping` 0.5 on the solver, so the bodies settle around the point.

Set `strength` to -12 and the point throws the bodies apart.

The page of [physics.field](../reference/nodes.md#physicsfield) has the types, the kick and the
table that takes the place of the node.

## Bodies in one plane (2D)

`plane` holds every dynamic body in one plane: `xy` is level like a table top, `xz` and `yz` are
upright like a wall. A body in the plane cannot leave it and turns only about the plane's normal.
Gravity, fields, `accelerations`, `targets`, start speeds and pushes from other bodies move it
within the plane only. Driven and fixed bodies, obstacles and the floor are not held.

**Depth.** Each body keeps the depth it starts at: its z for `xy`, its y for `xz`, its x for
`yz`. Bodies at different depths are layers that pass each other or touch at their rounded or
turned sides. Seen from the front they then seem to overlap. For a clean 2D picture, give all
bodies the same depth, the driven and fixed ones too.

A switch of `plane` acts at once and starts nothing over. Switched on, each body stays at its
depth of that moment and loses its speed and spin out of the plane. Switched off, the bodies are
free again.

## Balls that bounce off tracked blobs on a wall

Post-its on a wall, filmed by a camera, become boxes. Projected balls bounce off them.

1. The post-its: a driven group, built as in "A tracker's blobs as a driven group" on the page of
   [physics.bodies](../reference/nodes.md#physicsbodies), with `shape` `box`. Its blobs land at
   `tx ty` and at the `tz` of step 6.
2. The balls: a dynamic group with `shape` `sphere`, `bounce` 1 and `friction` 0. Its table
   gives every ball the same `tz` as the post-its, and a start speed in `vx vy`.
3. The frame: a group with `mode` `fixed` and four long boxes around the picture's room, at the
   same `tz`. For the room of 4 m by 3 m of that recipe, the table `tx,ty,tz,sx,sy` with the
   rows `0,1.6,0.4,4.4,0.2`, `0,-1.6,0.4,4.4,0.2`, `2.1,0,0.4,0.2,3.4` and
   `-2.1,0,0.4,0.2,3.4`.
4. The solver: `plane` `xy`, `gravity` 0 0 0, `floor` off, `bounce` 1, `damping` 0.

With these settings a ball keeps its speed, slow balls included. Ball against ball, the balls
lose a little: about 2 % of their energy in a minute.

**What follows a post-it.** Its place does. Its angle does not: `tex.blob_track` measures none,
so every post-it collides as a box square to the picture's edges, however it is turned. Its
size does not come through `chans` either, which has only `area`. Give the post-its their size
by hand: set `rows` of the `chan.constant` of step 6 to 3 and add `sx` and `sy`, the size of a
post-it in metres. For a size that follows, the tracker's `blobs` table has `width` and
`height` of the box around each blob, as shares of the picture.

## Time

The bodies move one step per show frame, as long as something shows the node's result. `speed`
slows time down or holds it at 0. A frame that takes longer than a twentieth of a second counts as
a twentieth, so the bodies slow down instead of jumping.

While time stands (`speed` at 0, or a show clock on hold), driven bodies and obstacles still go
where they are put, and push nothing. A `kick` whose `trigger` rises in that time is lost. It is
not saved up for later.

The solver runs on one core of the CPU. A few hundred boxes cost little. Hulls of detailed models
and obstacles of many triangles cost more, and thousands of bodies in one pile can take longer
than a frame.

## Limits

- **Bodies:** 4000 in all groups together. The solver names the group whose last bodies it left
  out. They come in, each at its row, as soon as another group makes room.
- **Sizes:** a body is at most 1000 times and at least a thousandth of its model's size. Nothing
  collides as smaller than 0.05 mm across or as larger than 2000 km.
- **Places:** at most 1 000 000 m from the origin along each axis. A number beyond counts as
  that.
- **Speeds:** no body moves faster than 400 m/s, or turns by more than an eighth of a turn in
  one solver step.

A number beyond these limits is brought within them. If a number in a body grows past what the
solver can hold all the same, the solver drops that body: it stays where it was and collides with
nothing. The solver's warning counts such bodies. `reset` brings them back.
