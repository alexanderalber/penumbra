## What it is for

`chan.from_tex` turns a picture into a few numbers that can drive params. How bright is the
camera picture? How much of the mask is lit? How many grains are left in a sand simulation? Is
the simulation dead, or has it filled the screen?

`measure` says what each channel tells about the pixels:

| measure | the channel is | for |
|---|---|---|
| `mean` | the average value | the mean colour, a brightness to follow |
| `sum` | all values added up | counting: in a picture of 0 and 1, the number of pixels that are 1 |
| `min`, `max` | the smallest or largest value | whether anything is left, the brightest spot's level |
| `coverage` | the share of pixels above `threshold`, from 0 to 1 | how much of the window is ice, how much of the mask is lit |

`components` picks the channels: `r g b a`, `r g b`, `r`, or `lum` (brightness as the eye
weighs it). With `columns` and `rows` above 1 the picture is divided into areas, and each area is
measured on its own.

- **`rows` makes channels.** Each row gives its own channels, numbered from the top: `r0`, `r1`, ...
- **`columns` makes samples.** Each channel gets one sample per column, left to right. A binding
  reads the last sample, the right-hand column.

So `columns` 3 and `rows` 1 gives one `r` channel of three samples, and a binding sees only the
third. To read three pixels as three channels, use `layout` `texels` (below).

`components` `rg` gives `r` and `g` only.

## Names

`names` gives the channels names of your own, in order: `coverage activity` calls the first
channel `coverage` and the second `activity`. A `-` leaves a channel out. So a 1 x 1 measure
whose red is the coverage and whose blue means nothing becomes `coverage activity -` with
`components` `rgb`. Channels past the names keep the names they would have had: `r`, `g0`, ...
With names, a component's channel output needs only a `chan.select` per output, and no
`chan.rename`.

## Every texel as a channel

With `layout` `texels` each texel of the picture gives channels of its own, with one sample
each. This is for a small state texture: a strip of timers, counters and flags that a
simulation keeps in a 1 x N or N x 1 picture.

- **Order:** texel by texel, row by row from the top left; within a texel `r`, `g`, `b`, `a` as
  `components` picks them. A 2 x 2 picture with `components` `red` gives top left, top right,
  bottom left, bottom right.
- **Names:** `names` names them in that order. A channel past the names is called `chan` with its
  number from 0, as in `chan.constant`: after two names comes `chan2`.
- **Size:** the channels follow the picture. A picture that grows gives more channels at the next
  readback; the names stay in order.
- **Limit:** at most 1024 texels. A larger picture is an error that says its size. Every frame
  something uses the channels, the texels are read back from the GPU, and the cook waits for
  them as it does for a measure of a whole picture (see below). `columns` and `rows` do nothing here. `measure` still applies,
  to one texel at a time: mean, sum, min and max all give the texel's value, and `coverage` gives
  0 or 1.

## Exact counts

Every pixel counts once, and the sums are added up in 32-bit floats. So the `sum` of a picture
of 0 and 1 is an exact count up to 16 million pixels: 1920 x 1080 is about 2 million. A mean is
good to about 7 digits. A `tex.shader` with `format` 32f is measured at its full precision.

A GPU that cannot render 32-bit floats reduces the first 16 x 16 pixels in half floats and adds
up the rest on the CPU. Counts of 0 and 1 stay exact there; other sums keep about 3 digits.

## When the numbers arrive

Usually the channels belong to the frame that just cooked. When the GPU is slow they come a
frame or two later. While stepping or recording, they always belong to their frame. The node
costs only while something uses its channels. At 1920 x 1080 it added about half a
millisecond per frame in a stepped run, most of it waiting for the GPU.

## Driving the loop it measures

A binding from `chan.from_tex` into the loop that the picture comes from is refused as a loop.
Put a `chan.feedback` between: wire `chan.from_tex` into its `previous` input and bind to the
`chan.feedback`. The value then arrives a frame later. See the example.

## Example

A simulation that starts over when it has died:

1. `chan.from_tex` on the simulation's state, `measure` `coverage`, `components` `luminance`,
   `threshold` 0.05: the share of the picture that is alive.
2. `chan.math` with `op` `below` and `b` 0.001: 1 when almost nothing is alive.
3. `chan.feedback` with the `chan.math` wired into `previous`.
4. Bind the loop's `tex.feedback` `reset` to the `chan.feedback`. When the population has died,
   the loop starts over from `initial` a frame later.

For "has filled the screen", add a second `chan.math` with `above` and combine both with
`chan.math` `max`.
