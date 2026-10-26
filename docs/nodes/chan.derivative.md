## What it counts

Each output channel is how fast the input channel of the same name changes: a position in, a
speed out. A channel that rises by 2 every second gives 2 with `per: second`, and 2/60 with
`per: frame` at 60 frames per second. `chan.integrator` does the opposite.

A channel with one value per frame is compared with its value at the last cook, over the time
between. A channel with many samples is compared sample by sample; `per: frame` then counts per
sample. The first value gives 0.

## Spikes

A jump in the input, such as a scene switch or a tracker losing its blob, gives one large value.
`limit: clamp` keeps the output within `min` and `max`. A `chan.filter` before this node calms a
jittery input, which otherwise turns into a jittery speed.
