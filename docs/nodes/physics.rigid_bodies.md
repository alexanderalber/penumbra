## A pile of boxes in five nodes

1. `table.text` with `format` set to `csv`: the first row names the columns, each further row is
   one body. `tx,ty,tz` and three rows `0,0,2`, `0.2,0,3.5`, `-0.1,0.1,5` drop three bodies from
   2 m, 3.5 m and 5 m.
2. `geo.primitive`, shape `box`: the model of a body.
3. `physics.rigid_bodies`: the table into `bodies`, the model into `shape`.
4. `geo.instance`: the model into `in`, the solver's `out` into `table`.
5. `tex.render`: the instance into `geo`.

The bodies fall onto the floor at z = 0 and come to rest. The floor is part of the solver and is
not drawn. Put a flat `geo.primitive` there if it should show.

## The table of bodies

Any table can place the bodies. Every column is optional, but the first row must name at least one:

| column | meaning | when the column is missing |
|---|---|---|
| `tx`, `ty`, `tz` | where the body starts, in metres; z is up | 0 |
| `rx`, `ry`, `rz` | how it is turned at the start, in degrees about x, then y, then z | 0 |
| `sx`, `sy`, `sz` | its size, times the model's | 1 |
| `vx`, `vy`, `vz` | its speed at the start, in metres per second | 0 |
| `wx`, `wy`, `wz` | its spin at the start, in degrees per second about x, y and z | 0 |

A row with a cell that is not a finite number is left out. The node takes 4000 bodies at most.

The output has one row per body in the same order, with the columns `tx ty tz rx ry rz sx sy sz`.
A change to the table puts every body back to its start. So does a change to the model in `shape`,
`reset`, and the reset button of the node.

Bodies that overlap at the start are pushed apart in the first frames, and fast. Leave a gap
between them.

## What a body collides as

`shape` (the param) decides what the solver makes of the model in the `shape` input:

- `hull`: the model with its dents filled in. A cup collides as a solid lump, and nothing falls
  into it.
- `box`: the box around the model. Boxes stack well and cost little.
- `sphere`: a ball as wide as the model's longest side. With a scale that differs per axis it stays
  a ball, as wide as the largest of the three.

A flat model, such as a plane, has no hull. It collides as the box around it, and the node says so
in a warning.

Every body has the same shape and the same density. A body twice as large weighs eight times as
much.

## Obstacles

Wire any geometry into `obstacles`: a model from `geo.file`, primitives placed by `geo.transform`,
several joined by `geo.merge`. Bodies collide with its triangles, so they fall into a bowl and roll
down a ramp. An obstacle is a thin shell with nothing inside. A fast or small body can pass through
it: raise `substeps`, or make the bodies larger or slower.

Obstacles may change while the bodies move, and bodies that rested on one fall on when it goes. A
moving obstacle jumps from place to place and does not push bodies along. It can leave them stuck
inside itself.

Copies made by `geo.tex_instance` are no obstacles, since their places exist on the GPU only.

## Time

The bodies move one step per show frame, as long as something shows the node's result. `speed`
slows time down or holds it at 0. A frame that takes longer than a twentieth of a second counts as
a twentieth, so the bodies slow down instead of jumping.

The solver runs on one core of the CPU. A few hundred boxes cost little. Hulls of detailed models
and obstacles of many triangles cost more, and thousands of bodies in one pile can take longer
than a frame.
