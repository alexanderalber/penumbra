# pendulum-wave

A pendulum wave, picture and sound. 100 pendulums hang in a 10 x 10 grid, in 10 rings around
the centre. Ring k swings 80 - k times in 64 s, so the centre ring is fastest. At 0 s every
pendulum passes the bottom at once. Then the rings drift apart into waves and patterns, and at
32 s they all meet again. After 64 s the loop starts over.

Each pendulum plucks a note as it passes the bottom, so each ring plays its own line of notes.
A kick drum follows the centre ring, every 0.4 s (150 BPM). The sound plays on the system's
default output; `device` in `/speakers` picks another.

## How it is built

**Sound.** `/ring0` to `/ring9` are `chan.lfo` sines. Each swings at its ring's rate,
(80 - k) / 64 Hz, so its value passes 0 exactly when that ring's pendulums pass the bottom.
`/rings` merges them, ring 9 first. `/pluck` (an `audio.instrument`, preset `pluck`, key F,
octave 3, `trigger` both) plays one note per lane at every pass of 0. Lane 0 is ring 9, so the
outer ring plays F3 and the centre ring D5: F3 G3 A3 C4 D4 F4 G4 A4 C5 D5. Each note starts at
the exact sample of the crossing, inside the frame.

`/centre` picks ring 0 for `/kick`, a second instrument with preset `kick`. `/every_other` is
an lfo at 0.625 Hz. It passes 0 on every other kick, and `/accent` adds a softer kick there.
`/mix` adds the three; `/speakers` plays the mix. The mix peaks around -5 dBFS.

**Picture.** `/out` is a `tex.shader` (`pendulums.wgsl`) at 1080 x 1102. It computes every
pendulum's angle from the show time, so any frame is exact and the picture stays in step with
the sound. A bob glows orange for 0.12 s after it passes the bottom. Below the grid, the shader
plots the sync R(t): 1 when all pendulums are at the bottom together, near 0 when they are
spread out. The circle on the left has one dot per ring, at the angle of its phase. The text
comes from the `tex.text` nodes, laid over each other in `/labels`.

The ground is white for the first 8 s and turns navy by 12 s. Over the last 4 s of the loop it
turns white again.

## Try

- `look` in `/out`: `light` or `dark` keeps one ground for the whole loop.
- `key` or `scale` in `/pluck`: every ring changes with it. `spread` 2 plays every other note
  of the scale and spans a wider range.
- `preset` in `/pluck`: `mallet` sounds like a marimba, `bell` rings much longer.
- `gain1` and `gain2` in `/mix` set the kick and the accent; 0 mutes them.
- Bind something to `swing` or `glow` in `/out`, for example a level from the
  audio-reactive example.
