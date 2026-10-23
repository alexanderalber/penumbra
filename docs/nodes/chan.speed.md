## What it adds up

Each input channel is a rate per second, and the output channel of the same name is its sum over
time: a speed in, a position out. A constant 0.25 rises by a quarter per second; a channel that
swings around 0 wanders back and forth.

Every cook adds the input's current value times the time since the last cook, so frames in which
nothing asked for the node still count, with the value it had when it cooked again. A channel that
holds many samples (a trail, audio) is summed sample by sample at its own rate instead.

## Limits

- `limit: loop` wraps from `max` back to `min` and the other way round: an angle (`min` 0, `max`
  360) or a scroll position that never runs away.
- `limit: clamp` stops at `min` and `max`.
- `limit: none` keeps adding without end.

The sum starts at 0, brought into the range by `limit` (with `clamp` and 0 outside the range, at
its nearest end). `reset` holds it there while on; a seek back in time, `node.reset` or R start it over as well.

## Example

Spin something at a speed the music sets: a `chan.audio_analysis` level through `chan.math` (scale
it to degrees per second) into `chan.speed` with `limit: loop`, `min` 0, `max` 360, and bind a
rotation param to the output.
