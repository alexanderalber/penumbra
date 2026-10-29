# Audio

Two nodes make a show react to sound: `audio.device_in` reads an input device, and
`chan.audio_analysis` turns what it hears into channels that params can be bound to.

## Sound is its own family

Sound travels on cyan wires of its own kind, `audio`. A `chan` wire carries values: its last
sample is the value now. An `audio` wire carries a stream: each frame's block of samples
continues the block before, so every sample counts. The two kinds do not connect.
`chan.audio_analysis` is the bridge from sound to channels that params can be bound to.

- `audio.device_in` reads a microphone or an audio interface.
- `audio.oscillator` makes a test tone. `shape` is sine, saw, square, triangle or noise. The
  params `frequency` and `amplitude` can be bound; a change glides across one frame, so it
  does not click. `rate` is the sample rate, 48000 by default.
- `audio.mix` adds up to eight sounds. Each input has its own gain (`gain0` to `gain7`), and
  `gain` sets the whole. All inputs need the same sample rate; two rates are an error, since
  the mix does not convert between them.
- `audio.instrument` and `audio.ambience` are ready-made sounds in a key and scale, played by
  channels. See [Instruments and ambience](#instruments-and-ambience).
- `chan.read` reads an audio node too: the rate, the block's length, `peak` and `rms` per
  channel, and with `samples: true` the whole block.

The viewer shows sound as the last 50 ms of its waveform and a peak meter per channel in
dBFS. The waveform starts at a rising zero crossing, so a steady tone stands still.

A tone generator makes exactly the samples of one frame. At 48 kHz and 60 fps that is 800
samples. At 44.1 kHz it alternates between 735 and 736, so the sound stays in step with the
picture for hours. After skipped frames, the next block covers them all, up to half a second.
After a longer pause or a jump in time, the sound starts over.

## Playing sound

`out.audio` plays its input on a speaker or headphones. Mono plays on every speaker, stereo on
left and right. `device` is part of an output's name; the `...` menu lists them, and empty is
the system's default output. `level` is the volume in dB, and `mute` silences the node. Both
glide across one frame.

Every output ends in a limiter that keeps the sound below -1 dBFS. It is always on, because a
sudden blast in a room full of people is the worst thing an installation can do. When the
limiter has to work, the node warns how much it takes off. Turn `level` down until the warning
goes. Several `out.audio` nodes on one device are added together, and their sum is limited too.
They must share one sample rate.

Sound plays while the show runs. Pause the show and the output fades out; play it and the sound
comes back. This holds for a microphone routed to `out.audio` too. `audio.mute` mutes every
output at once: a panic button, also in Helpers > Audio.

The delay from a frame to the speaker has two parts. `latency` is what penumbra queues, 50 ms
by default. The device adds its own buffer, about 30 to 50 ms in Windows' shared mode. A smaller
`latency` answers faster. 30 ms holds on a calm machine, but then a late frame is heard as a
gap. `audio.status` shows the delay and counts the gaps. `audio.measure_latency` plays quiet
clicks and finds them again on an input. The input `loopback:Speakers` records what the output
named Speakers plays, without a cable.

## Instruments and ambience

Two nodes make music from channels, with no sound design needed. Both play in a `key` and
`scale`, so any notes together sound fine. The default is C major pentatonic.

**`audio.instrument`** plays one-shot sounds that fade by themselves: `pluck`, `mallet`,
`bell`, `kick` and `hat`. Wire channels into `play`. Each channel is
a lane that starts a note when its value crosses `threshold` (0). `trigger` picks the
direction: `up` for pulses such as an onset or a key, `both` for a swinging value, which plays
at every pass. The note starts at the exact sample of the crossing, interpolated between
frames. A tracked position or an lfo lands within a fraction of a millisecond. A pulse that
only knows its frame starts at the frame's first sample. `decay` sets how long a note rings,
`brightness` how sharp it sounds. `voices` (32) limits the notes at once; one more fades out
the oldest in 5 ms. To try a sound without wiring anything, press `play` in the params, or run
`audio.play {path, lane?}`. One note plays at the next frame, as lane 0 (or `lane`) would. The
note is heard only through an `out.audio` while the show plays. A press while paused is
dropped.

**`audio.ambience`** holds a sound without notes: `drone`, `pad`, `noise` (like wind),
`water` (a stream) or `rain`. Pitch, `brightness`, `swarm`, `density` and `level` follow their
params smoothly over `glide` (0.1 s), so bind them to tracking. Nothing wired: one tone sounds all the time. With
channels wired into `gate`, each channel holds its own tone while its value is above
`threshold`. The tone fades in over `attack` and out over `release`.

**Water.** `water` sounds like running water: a low rumble, a gurgle between 200 and 600 Hz, a
splash around 4 kHz and a hiss above 6 kHz. Each band swells and fades on its own, at random,
from a fifth of a second to a few seconds. Small bubbles pop in the gurgle. `brightness` 0 is
a deep river, 1 a bright trickle with more bubbles.

**Rain.** `rain` is a soft rush with droplets ticking in it. `density` sets how hard it rains:
0 is a drizzle of 3 drops a second over a faint rush, 0.5 about 90 drops a second, 1 a
downpour of 600 over a loud rush. `brightness` 0 sounds distant and dull, 1 close and crisp.
`density` does nothing with the other presets.

**Swarm.** `swarm` works with `drone` and `pad`. At 0 every tone sits on its note. Above 0 each
tone becomes eight voices whose pitches wander slowly at random around the note. 0.2 sounds
like a chorus. At 1 the voices roam up to half an octave either side, and the chord dissolves
into a moving cloud. Turn `swarm` back to 0 and every voice glides onto its note. Bind `swarm`
to movement: people moving make a cloud, and the chord comes back when they stand still. The
loudness stays about the same across `swarm`.

**Pitch.** `note` 0 plays the root in `octave` (4 holds middle C; A4 is 440 Hz), `note` 1
plays `range` octaves higher (2). `note` is rounded to the nearest note of the scale. Lane `i`
adds `i * spread` notes of the scale: with key F, octave 3 and spread 1, ten lanes play F3 G3
A3 C4 D4 F4 G4 A4 C5 D5. Scale `off` gives free pitch for a theremin. `kick`, `hat`,
`noise`, `water` and `rain` ignore pitch.

**Level.** `level` is -12 dB by default. One note then peaks around -13 to -16 dBFS. A pile of
notes bends softly below full scale instead of clipping. Every note has velocity 1.

## Setting up

1. Add an `audio.device_in` (Tab > audio > device_in). With `device` empty it reads the system's
   default input; the empty `device` box names that device in grey (hover the box for the device
   a typed name picked). `audio.status` names it too. The `...` menu beside `device` lists the inputs of this machine; part of a
   name typed in works too, e.g. `Focusrite`. The [Helpers window](helpers.md#audio) shows a
   level meter per channel. The device is opened in shared mode, so other programs can use it too.
2. Connect it to a `chan.audio_analysis`.
3. Bind params to the analysis' channels, as to any channel (see
   [bindings](concepts.md#bindings)): drag a wire from its output onto a param row, or pick the
   channel in the param's link menu.

`audio.device_in` outputs the samples that arrived since its last cook, one channel per device
channel (`ch1`, `ch2`, ...) at the device's sample rate. It keeps listening while the show is
paused. If the device goes away (a pulled cable), the node shows an error and the input is
opened again every two seconds until it is back.

Projects saved before `audio` was a family name the node `chan.audio_in`. They open with
the new name. A wire from it, or from an NDI source's sound, into a chan node other than the
analysis is left out on opening, and so is a binding to it; the log names each. Put a
`chan.audio_analysis` in between and bind to the analysis.

## What the analysis gives

All values are channels of the `out` output, in this order:

| channel | what |
|---|---|
| `level` | loudness, 0 to 1 |
| `low`, `mid`, `high` | loudness of the bands below 150 Hz, between, above 2.5 kHz (params `low`, `high`) |
| `onset` | 1 in the frame a sound starts (a hit, a note), else 0 |
| `kick` | 1 in the frame a sound starts in the low band |
| `beat` | 0 at each beat, rising to 1 at the next: drive anything that should move with the beat |
| `tick` | 1 in the frame a beat starts |
| `bar` | 0 at each bar's first beat, rising to 1 at the next bar |
| `bar_beat` | the beat within the bar: 0, 1, 2, 3 |
| `bpm` | the tempo, 0 until one is found |
| `confidence` | 0 to 1: how clearly the music repeats at that tempo |
| `rms`, `peak` | the raw level, full scale is 1 |
| `beats_per_bar`, `low_hz`, `high_hz` | the params `beats_per_bar`, `low` and `high` as they are (bound ones evaluated), for what draws the analysis |

With `normalize` on (the default), `level`, the bands and the spectrum are measured against
the loudest of the last seconds, so a quiet microphone still spans 0 to 1. Sounds below
`floor` (-60 dB) count as silence and are not raised.

The second output, `spectrum`, is one channel whose samples are the loudness of `bands` (32)
bands from 30 Hz to 16 kHz, low to high, each 0 to 1 over 60 dB.

## The meters in the viewer

Selected, the analysis shows meters in the viewer, made to be read at a glance during a show:

- **Level and bands:** `level`, `low`, `mid` and `high` as bars (in the colours their curves
  have in the plot), each with a white line that holds its peak for a second, then falls.
- **Tempo:** `bpm` in large type, `--` until a tempo is found. The less sure the analysis is,
  the dimmer the number; the bar below it is `confidence` (red when unsure, amber, green).
- **Beat light:** a disc that flashes on each beat, amber on a bar's first, blue on the others,
  in a ring that fills with `beat` from one beat to the next.
- **Bar:** a cell per beat of the bar (the `beats_per_bar` channel), the first outlined in amber; the
  current one (`bar_beat`) is lit and fills with `beat`. The line below fills with `bar`.
- **Hits:** the last four seconds of kicks, onsets and beats as marks moving left, a row each
  (a bar's first beat amber); the dot before each row flashes on its hit.
- **Spectrum:** the `spectrum` output as bars, low to high, coloured as the low, mid and high
  bands (by the `low_hz` and `high_hz` channels).

In a narrow column the spectrum and the rows' names are left out. `view: meters` in the
viewer's header switches to the plot of the channels and back (`ui.options {audio_view:
"meters" | "plot"}`); the choice is kept with the project and holds for every analysis. The
meters keep their short memory (peaks, recent hits) in the viewer: switching nodes starts it
over.

## The meters as a picture

The analysis' third output, `meters`, is the same meters as a texture, to project during a
sound check, put on a monitor output or lay over the show: wire it into a tex node, or pick it
in the viewer's header (`ui.view {path, output: "meters"}`). `width` and `height` in the
analysis' `meters` section (1920 x 1080; `keep_size` pins them against `res.optimize`) set the
picture's size; it is laid out as the viewer would draw it at about 640 x 360 and enlarged, so
type and lines keep their proportions at any size, and below about 150 pixels of height the
spectrum and the rows' names go, as in a narrow viewer.

Around the meters the texture is transparent, ready for a `tex.composite` over the show. For a
solid picture, composite it over a `tex.constant` of the colour wanted. The viewer shows this
output on black, as its meters view.

The output is drawn only while something reads it: a wire into a node that cooks (for an
output, the viewer or a thumbnail), the viewer showing it, or `node.snapshot`. Otherwise the
analysis does not draw, and holds no textures for it. While drawn it costs about a quarter of a
millisecond at 1080p and keeps its own short memory (peaks, recent hits) on a steady clock, as
the viewer does; `node.reset` (or R) on the analysis clears it, and it starts over when the
output is read again after a pause. It is drawn when the analysis cooks, that is when audio
arrives, also while the show is paused. The text is set in Inter; a size seen for the first
time shows its characters a frame or two later.

## Tempo and beat

The tempo is found after about four seconds of music and follows a new track within a few
seconds. It is looked for between `min_bpm` (70) and `max_bpm` (180); keep the two less than
an octave apart, so half or double the tempo is not taken. Through a breakdown without a beat
the beat keeps running at the last tempo while `confidence` drops, so visuals stay in time.

`beat` is meant to be in time with what is heard. A projector shows a picture a few frames
late; `offset` moves beat and bar ahead by that many milliseconds. Set it by eye: flash
something on `tick` and raise `offset` until the flash meets the kick.

The first beat of a bar is a guess: it is the beat where the sound changes most (bass lines
and chords tend to change there). With the same pattern on every beat the guess may land on
another beat; `bar_beat` and `bar` are then counted from there.

## Tips

- Analyse one channel with `channel` (e.g. `ch1`) when the others carry something else.
- `chan.filter` smooths a channel; `chan.math` scales it.
- `chan.trail` after the analysis keeps its last seconds as samples; the viewer and the
  trail's own body plot them with a lane per channel, so `bpm` does not flatten the levels.
- The level meters in Helpers should stay out of the red. A microphone close to the speakers
  clips; turn down its gain.
