## What it is for

A switch decides which picture goes on. Wire chains into `in0`, `in1` and on (each one wired adds a free input below it), say the
scenes of a show, the looks of a projection or a test pattern beside the real content, and
`index` picks the one that comes out. Turn `index` by hand, or bind it to whatever should change
the picture during the show: a key, an OSC or MIDI control, a timer.

## Which input

`index` counts the wired inputs from 0, in order, and skips the gaps: with `in0`, `in1` and `in4`
wired, 2 is `in4`. An index past the last input shows the last one.

## Cut or crossfade

With `blend` off, the picture cuts from one input to the next, and a fractional index rounds down
(1.7 shows input 1). With `blend` on, a fractional index mixes the two inputs either side: 1.25
is three quarters of input 1 and a quarter of input 2. Move `index` smoothly from 1 to 2 and the
picture crossfades. The mix has the size of the lower input.

For two fixed pictures and a mix amount, `tex.cross` is the simpler node. For a show that runs
on a timeline, with cuts and fades at set times, use `tex.sequence`.

## The inputs not shown

With `inactive: cook` (the default), every chain keeps running whether it is shown or not.
Switching is instant and every input is up to date, but all of them cost GPU time.

With `inactive: skip`, only the shown input's chain runs (both, while blending); the others stand
still and cost nothing. That makes room for many heavy scenes. The price: switching to a skipped
chain cooks it in that frame, so a slow chain makes one frame late and a video may need a moment
to catch up. A feedback loop, cache or time machine in a skipped chain carries on from where it
stopped, not from where it would be by now.

## Example

Scenes on the number keys: wire four scenes into `in0` to `in3`, add a `chan.keyboard_in` (its
`keys` are `1 2 3 4` by default) and bind `index` to its `last` channel. Key 1 shows `in0`, key 2
`in1`, and so on. With `inactive: skip`, only the scene on screen costs anything.
