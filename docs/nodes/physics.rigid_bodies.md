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
`forces` and `targets` of a group never start it over.

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
the way of the obstacle's own origin: a long bar turning fast about its middle never jumps, and
its ends can pass small bodies without touching them. Raise `substeps` then.

For moving things with a simple form, a `driven` group is the cheaper way: a box or a ball costs
less than a mesh.

Copies made by `geo.tex_instance` are no obstacles, since their places exist on the GPU only.

## Forces

Gravity pulls every body the same way. A `physics.force` adds a field of its own: a pull to a
point, a wind, a whirl, or a kick on a trigger. Wire it into a `force` input of the solver. Each
further field takes the next `force` input, 64 fields in all at most.

A field moves the dynamic bodies of every group. `strength` is the speed a body gains per second,
so a large or heavy body moves as readily as a small or light one. Driven and fixed bodies and
obstacles do not feel a field.

A point that pulls the pile to where an LFO puts it:

1. `physics.force` with `type` `point` and `strength` 12, into `force0` of the solver.
2. `chan.lfo` with `rate` 0.1 and `amplitude` 3. Bind the x of the field's `position` to it.
3. `damping` 0.5 on the solver, so the bodies settle around the point.

Set `strength` to -12 and the point throws the bodies apart.

The page of [physics.force](../reference/nodes.md#physicsforce) has the types, the kick and the
table that takes the place of the node.

## Time

The bodies move one step per show frame, as long as something shows the node's result. `speed`
slows time down or holds it at 0. A frame that takes longer than a twentieth of a second counts as
a twentieth, so the bodies slow down instead of jumping.

The solver runs on one core of the CPU. A few hundred boxes cost little. Hulls of detailed models
and obstacles of many triangles cost more, and thousands of bodies in one pile can take longer
than a frame.
