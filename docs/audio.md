# Audio

Two nodes make a show react to sound: `chan.audio_in` reads an input device, and
`chan.audio_analysis` turns what it hears into channels that params can be bound to.

## Setting up

1. Add a `chan.audio_in` (Tab > chan > audio_in). With `device` empty it reads the system's
   default input; the empty `device` box names that device in grey (hover the box for the device
   a typed name picked). `audio.status` names it too. The `...` menu beside `device` lists the inputs of this machine; part of a
   name typed in works too, e.g. `Focusrite`. The [Helpers window](helpers.md#audio) shows a
   level meter per channel. The device is opened in shared mode, so other programs can use it too.
2. Connect it to a `chan.audio_analysis`.
3. Bind params to the analysis' channels, as to any channel (see
   [bindings](concepts.md#bindings)): drag a wire from its output onto a param row, or pick the
   channel in the param's link menu.

`chan.audio_in` outputs the samples that arrived since its last cook, one channel per device
channel (`ch1`, `ch2`, ...) at the device's sample rate. It keeps listening while the show is
paused. If the device goes away (a pulled cable), the node shows an error and the input is
opened again every two seconds until it is back.

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

`tex.audio_meters` draws the same meters into a texture, to project during a sound check, put
on a monitor output or lay over the show. Connect the analysis' `out` to its `in` and its
`spectrum` to `spectrum` (left unconnected, the spectrum is left out). `width` and `height`
(1920 x 1080) set the picture's size; it is laid out as the viewer would draw it at about 640 x
360 and enlarged, so type and lines keep their proportions at any size, and below about 150
pixels of height the spectrum and the rows' names go, as in a narrow viewer. `background`
is the colour behind the meters; by default it is transparent, so only the meters are drawn,
ready for a `tex.composite`; give it alpha 1 for a solid picture.

The node keeps its own short memory (peaks, recent hits) on the show clock: a paused show
stands still, a jump back in time starts it over, and `node.reset` (or R) clears it. It draws
on the GPU every frame; a cook takes about a quarter of a millisecond at 1080p. The text is
set in Inter; a size seen for the first time shows its characters a frame or two later.

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
