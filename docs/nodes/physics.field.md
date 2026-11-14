## What it is for

A `physics.field` speeds up the bodies of a `physics.rigid_bodies`: it pulls them to a point,
blows them along as a wind or drives them round as a whirl. Wire it into a `field` input of the
solver. A second field goes into the next `field` input, which appears when the last one is
taken.

A field moves the dynamic bodies of every group. Driven and fixed bodies do not feel it, and
neither do obstacles.

Every param is a number, so every param can follow a channel: sound makes the wind, an LFO moves
the point, a key gives the kick.

## The three types

- **`point`:** pulls the bodies towards `position`. A negative `strength` pushes them away.
- **`wind`:** blows the bodies along `direction`, everywhere or within `radius` of `position`.
- **`vortex`:** drives the bodies round an axis. The axis runs through `position` along
  `direction`. Seen from where `direction` points to, they go counterclockwise. Nothing pulls
  them inwards, so they drift outwards as they speed up. A `point` field at the same place holds
  them together.

## How strong

`strength` is the speed a body gains per second, in metres per second squared. Gravity is 9.81.
A wind of 10 straight up lifts the bodies slowly. A wind of 5 straight up halves their weight.

Every body gains speed at the same rate: a large box as fast as a small one, a group of density
10 as fast as a group of density 1. A real wind would hardly move the heavy one. This is on
purpose, since a show mostly wants the whole pile to move.

A body on the floor stays until the field beats the grip. With a `friction` of 0.5 on both, that
takes a `strength` of about 5.

While a field pushes a body, the solver cannot let that body rest. A large pile in a steady wind
costs more than the same pile without one. Set `strength` to 0 when the field is not needed.

## Where it reaches

`radius` 0 means everywhere. With a radius, bodies farther from `position` feel nothing. For a
vortex the distance from its axis counts. `falloff` `linear` weakens the field evenly from its
middle to the radius.

Within 0.2 m of a point, and of the axis of a vortex, the field fades to nothing. Bodies that
have arrived there do not shake.

## A kick on a trigger

`kick` is a change of speed in metres per second. The field gives it once, at the moment
`trigger` rises past 0.5, to every body it reaches. It has the field's shape: a `point` with a
negative `kick` blasts the bodies apart, a `wind` gives a gust, a `vortex` a twirl.

The kick comes besides `strength`. For a kick alone set `strength` to 0.

While time stands for the bodies (`speed` at 0 on the solver, or a show clock on hold), a
trigger that rises gives no kick. The kick is not saved up for later.

## Recipes

**Sound blows the pile apart.** A `physics.field` with `type` `point`, `strength` 0, `kick` -6
and `position` in the middle of the pile, a little under the floor so the bodies fly upwards
too. Bind `trigger` to the `kick` channel of a `chan.beat_analysis`.

**An attractor that wanders.** A `physics.field` with `type` `point` and `strength` 12. Bind the
x of `position` to a `chan.lfo` with `amplitude` 3 and `rate` 0.1. The bodies follow the point
from side to side. `damping` 0.5 on the solver keeps them from circling it for ever.

**A whirl.** Two fields at the same `position`: a `vortex` with `strength` 8 and a `point` with
`strength` 6. The point holds what the vortex drives round.

## A table of fields

A `field` input takes any table with these columns in its first row. Each further row is one
field, 64 fields in all at most. This is how a tracker places attractors: its channels, turned
into metres and into a table by `table.from_chan`, with a `strength` channel beside them.

| column | meaning | when the column is missing |
|---|---|---|
| `type` | `point`, `wind` or `vortex`, or 0, 1 or 2 | `point` |
| `strength` | metres per second squared | 0 |
| `tx`, `ty`, `tz` | `position`, in metres | 0 |
| `dx`, `dy`, `dz` | `direction` | up (0 0 1) |
| `radius` | metres, 0 for everywhere | 0 |
| `falloff` | `none` or `linear`, or 0 or 1 | `none` |
| `kick` | metres per second | 0 |
| `trigger` | the kick comes when it rises past 0.5 | 0 |

The table needs a `strength` or a `kick` column. A table without both is taken for a mistake, and
the solver says so. A row with a cell that is not a number is left out, and the solver counts
such rows in a warning.

A kick belongs to its `field` input and its row there. A field that comes or goes on another
input changes nothing. Inside one table, a row that takes the place of another can kick in its
stead when rows come and go.
