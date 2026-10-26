## Which type for what

| to ... | `type` | params |
|---|---|---|
| soften a jump, fade in fast and out slowly | `first_order` | `rise`, `fall` |
| soften a jump with a smoother start and stop, never overshooting | `critical` | `rise`, `order` |
| calm a wobbly sensor and keep the shape of its movement | `bessel` | `rise`, `order` |
| remove fast wiggle as cleanly as possible; a small overshoot is fine | `butterworth` | `rise`, `order` |
| average the last moment, newest values counting most | `gaussian` | `window` |
| average the last moment evenly; cancels a wobble of exactly that period | `box` | `window` |
| cap how fast a value may change | `slew` | `up`, `down` |
| steady a tracked position at rest and follow it closely in motion | `one_euro` | `rise`, `beta` |

Params the chosen type does not use are greyed out in the panel.

## Same speed across types

`rise` means the same for `first_order`, `critical`, `bessel` and `butterworth`: after a step, the
output has covered 63 % of it after `rise` seconds. Switching between these types keeps the felt
speed; the curve around that point changes. A higher `order` starts later and then catches up
faster. Overshoot after a step: `critical` none, `bessel` below 1 %, `butterworth` 4 % at order 2,
8 % at order 3, 11 % at order 4.

Only `first_order` has a separate `fall`. The other linear types use `rise` both ways.

## Averages

`box` and `gaussian` look at the last `window` seconds. `box` lags by half the window. `gaussian`
weighs recent values more (a half bell curve, sigma a third of the window) and lags about a quarter
of it. Channels with many samples per second (audio, a long trail) can need more history than the
node keeps; the node then shortens the window and says so in a warning.

## one_euro

`one_euro` (Casiez, Roussel and Vogel, 2012) changes its smoothing with the speed of the input.
At rest it smooths like `first_order` with time constant `rise`. The faster the input moves, the
less it smooths: `beta` sets how much. Start with `beta` 0, set `rise` until the jitter at rest is
gone, then raise `beta` until fast moves no longer lag. `beta` depends on the input's units: a
position from 0 to 1 needs a larger `beta` than one in pixels.

## Timing

A channel with one value per frame steps by the time since the last cook. Frames in which nothing
asked for the node still count. A channel with many samples is filtered sample by sample at its own
rate. The first value of a channel passes unchanged. Switching `type` or `order` carries on from
the last output. A seek back in time, `node.reset` or R start the node over.
