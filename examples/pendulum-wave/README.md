# pendulum-wave

A pendulum wave seen from above, picture and sound. 121 pendulums stand in an 11 x 11 grid.
Each swings out from the centre and back. A pendulum's ring is its distance from the centre,
rounded to whole grid steps, so there are 8 rings. Ring k swings 56 - k times in 48 s, so the
centre is fastest. At 0 s every pendulum passes its rest point at once. Then the rings drift
apart into spirals and ripples, and at 24 s they all meet again. After 48 s the loop starts
over.

Each pendulum strikes a note as it passes its rest point, so each ring plays its own line of
notes. A hi-hat follows the centre pendulum and a kick drum falls on every other hi-hat. The
sound plays on the system's default output; `device` in `/speakers` picks another.

Inspired by a pendulum-wave clip by [The Lab (@the.lab67)](https://www.tiktok.com/@the.lab67), who
makes physics and maths simulations.

## How it is built

**Sound.** `/ring0` to `/ring7` are `chan.lfo` sines. Each swings at its ring's rate,
(56 - k) / 48 Hz, so its value passes 0 exactly when that ring's pendulums pass their rest
point. `/rings` merges them, ring 7 first. `/notes` (an `audio.instrument`, preset `mallet`,
key A, octave 3, scale `minor_pentatonic`, `trigger` both) plays one note per lane at every
pass of 0. Lane 0 is ring 7, so the corners play A3 and the centre D5: A3 C4 D4 E4 G4 A4 C5
D5. Each note starts at the exact sample of the crossing, inside the frame.

`/centre` picks ring 0 for `/hat`, a second instrument with preset `hat`: a hit every 0.43 s
(140 BPM). `/every_other` is an lfo at half that rate. It passes 0 on every other hat, and
`/kick` plays there. `/mix` adds the three; `/speakers` plays the mix with its default room
reverb.

**Picture.** `/out` is a `tex.shader` (`pendulums.wgsl`) at 1080 x 1080. It computes every
pendulum's position from the show time, so any frame is exact and the picture stays in step
with the sound. Each pendulum has a faint track; its bob flashes as it passes the middle of the
track. A faint circle through each ring lights up at the same moment. The colours come from
the `colors` palette: the centre at its end, the corners near its start.

## Try

- `direction` in `/out`: `across` swings every pendulum left and right, which turns the
  ripples into stripes.
- `colors` in `/out`: any palette or colormap, for example `viridis`, `fire` or `sea`.
- `key` or `scale` in `/notes`: every ring changes with it. `spread` 2 plays every other note
  of the scale and spans a wider range.
- `preset` in `/notes`: `pluck` sounds like a string, `bell` rings much longer.
- `gain1` and `gain2` in `/mix` set the kick and the hat; 0 mutes them.
- Bind something to `swing` or `glow` in `/out`, for example a level from the
  audio-reactive example.
