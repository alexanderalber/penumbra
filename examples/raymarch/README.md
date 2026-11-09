# raymarch

Three raymarched looks that move with music, drawn by `tex.shader` nodes. Look at `/out`.

Drag the slider on `/look`: 0 is the tunnel, 1 the morph, 2 the fractal. Between two numbers
`/looks` crossfades. `/look` also appears on the show screen.

The project needs no microphone and no sound device. `/song` is a beat made of sound nodes, and
nothing plays it on a speaker.

## How it is built

**Sound.** `/song` is a component with a placeholder beat: kick, hat and pluck at 128 BPM. The
kick stops for 7.5 s of every 15 s. `/analysis` (a `chan.beat_analysis`) hears `/song` the way it
would hear a microphone. It finds the tempo after about 6 seconds and gives `level`, `kick` and
more channels.

**Shaping.** The chan nodes below `/analysis` make the numbers the shaders want:

- `/level` is the loudness, smoothed so that it falls in 0.45 s.
- `/hit` is each kick with a 0.22 s fade.
- `/slow_level` is the loudness over several seconds.
- `/soft` is `/slow_level` plus 0.45: how softly the morph melts.
- `/flight` adds up the level, so it only grows. `/rate` scales the level first. A channel that
  only grows moves the camera forward without a jump.
- `/orbit` adds up the level into degrees, from -180 to 180.
- `/hue` and `/julia` are slow `chan.lfo` swings.

**Looks.** The dashed green lines are bindings:

| channel | `/tunnel` | `/morph` | `/fractal` |
|---|---|---|---|
| `/level` | `energy` | `energy` | `energy` |
| `/hit` | `flash` | `kick` | `kick` |
| `/hue` | `hue` | `hue` | `hue` |
| `/flight` | `travel` | `phase` | `phase` |
| `/orbit` | | `orbit_angle` | `orbit_angle` |
| `/soft` | | `blend` | |
| `/slow_level` | | | `morph` |
| `/julia` | | | `julia` |

Leave `speed` and `orbit` alone on the shaders: changing them live makes the camera jump. Bind
`travel` or `phase` instead. Keep `power` on `/fractal` a whole number, because a value between
two whole numbers can show a seam on the surface.

**Glow.** `/bright_parts` keeps what is brighter than `black`. `/halo` blurs it, and `/out` adds
the blur back onto the picture.

## Real music

1. Add an `audio.device_in` (Tab > audio > device_in). With `device` empty it reads the
   system's default input.
2. Wire it into `audio` on `/analysis`. This replaces `/song`.
3. Play music with a clear beat. `/analysis` needs a few seconds to find the tempo.

To hear `/song` instead, add an `out.audio` and wire `/song` into it.

Any other sound works in the same place: an `audio.file` plays a sound file, and the
[audio-reactive](../audio-reactive/README.md) example shows more uses of the channels.

## Output and cost

Each shader draws 1920 x 1080. `/screen` is an `out.projector` that is switched off. Tick its
`active` for a preview window. Untick `window` to go fullscreen on a display (`display` picks it).

On an RTX 3090 limited to 280 W, one frame at 1920 x 1080 costs about 2.5 ms for the tunnel, 5 ms
for the morph and 3.3 ms for the fractal. `/looks` cooks only the look on show. During a crossfade
it cooks two.

To make a look cheaper:

- Lower `steps` in the `look` group of that shader. Defaults: 96 tunnel, 72 morph, 88 fractal.
- Lower `width` and `height` of that shader. The cost follows the number of pixels.
- On `/fractal`, lower `detail`. On `/morph` or `/fractal`, set `shadow` to 0.

## Try

- `b` on `/rate`: how far the music pushes the flight. 1.2 is calm, 3 races.
- `fall` on `/hit`: longer flashes.
- `black` on `/bright_parts` and `size` on `/halo`: less or more glow.
- `colours` on a shader: any palette, for example `fire` or `viridis`.
- `sides` on `/tunnel`: 4 is a square shaft.
- `/song`: change `rate` on the lfo nodes and the `gain` inputs of `mix` to try other rhythms.
