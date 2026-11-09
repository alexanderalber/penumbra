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
measured on its own: a sample per column, a channel per row (`r0`, `r1`, ...).

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
