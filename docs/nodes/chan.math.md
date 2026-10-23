## What it is for

Channels rarely arrive in the range a param wants. An audio level runs from 0 to 1, a rotation
wants degrees; a tracked position runs across the picture, a light wants it the other way round.
`chan.math` is the node in between: it scales, shifts, limits and compares channels, so that one
number fits where it goes.

## Two inputs, or one and a number

`a` is the channel (or channels) to work on. `b` is what to work with:

- **Unconnected `b`:** the param `b` stands in, one number for every channel of `a`. This is the
  usual case: `multiply` with `b` 360 turns 0 to 1 into 0 to 360 degrees, `add` with `b` -0.5
  moves 0 to 1 to -0.5 to 0.5.
- **Connected `b`:** each channel of `a` meets the channel of `b` with the same name, else `b`'s
  first channel. So `x` and `y` of a tracker multiplied by `x` and `y` of a `chan.constant`
  scale each axis on its own, and a single channel in `b` acts on all of `a`.

The output has `a`'s channels, with their names. Every sample is computed on its own, so a
channel with many samples (a spectrum, a trail) comes out with as many.

## Which op for what

| to ... | op | b |
|---|---|---|
| scale (0 to 1 into 0 to 360) | `multiply` | 360 |
| shift (move the centre) | `add` or `subtract` | the shift |
| cap at a highest value | `min` | the cap |
| keep above a lowest value | `max` | the floor |
| count round and round (an angle that grows into 0 to 360) | `modulo` | 360 |
| make steps (whole numbers only) | `floor` | 1 |
| bend a curve (gentle start, steep end) | `power` | 2 |
| switch something on for one piece of a show | `equal` | the piece's number |

For a whole range in one go (0 to 1 into 20 to 80), chain two: `multiply` by 60, then `add` 20.

## Example

A rotation that follows the music: a `chan.audio_analysis` level into `chan.math` with `op`
`multiply` and `b` 90, and bind a `tex.transform`'s `rotate` to the output. Loud passages turn
the picture by up to 90 degrees.
