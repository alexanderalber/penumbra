## How it pours

The emitter is a short line of nozzles across its direction, `width` picture heights wide. It
pours rows of particles along `direction` at `speed`. The amount follows: `width` times `speed`
is the area of liquid poured per second. A stream 0.04 wide at 0.5 picture heights per second
pours 0.02 square picture heights per second, about 3700 particles a second at the default
grid of `tex.liquid`.

Turning `on` off stops the pour at once. Bind `on` to a channel to switch it, or bind
`position` to move the stream.

## Emitters from a table

Any table can be an `emitter` input of `tex.liquid`, one emitter per row. Its first row names the
columns. `x` and `y` are required, every other column is optional:

| column | meaning | when the column is missing |
|---|---|---|
| `x`, `y` | the stream's middle, as shares of the picture from the top left | required |
| `width` | how wide the stream is, in picture heights | 0.04 |
| `vx`, `vy` | which way and how fast it pours, picture heights per second, y down | 0 and 0.5 |
| `on` | 1 pours, 0 stops; 0.5 and above counts as on | 1 |
| `material` | `liquid`, or its number 0 | `liquid` |

The node's params become `vx` and `vy` as `direction` scaled to `speed`. A table names the
velocity directly, as a tracker's table does.

A row with a cell that is not a finite number, or an unknown material, is left out. A
`table.text` with commas between its cells needs `format` set to `csv`.
