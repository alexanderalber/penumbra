# Recording

`out.video` (Tab > system > video) records its input to a movie file: the show as it runs,
for documentation, for an edit, or to play back later with `tex.video_file`.

## Recording

1. Add an `out.video` and connect the picture to record, usually what feeds the projector.
2. Press **record** at the top of its params (or call `video_out.start {path}`). The row beside
   the button shows the file, the length so far, frames written and dropped, and how fast the
   encoder runs. **stop** (or `video_out.stop {path}`) finishes the file.
3. To start and stop from a channel (an OSC button, a timer, a beat), bind `record`: turning it
   on starts a new file, turning it off finishes it.

The `record` param is the one switch: the button and the two commands turn it on and off, as
an ordinary edit (undo undoes it, so undoing a start stops the recording). While a channel
drives `record`, the button is greyed and the commands refuse. A project always opens with
`record` off, whatever was saved, so opening a project never starts a recording by itself.
When a recording cannot start or ends with an error, the node shows why and does not try again
until `record` turns off and on.

`file` empty records to `recordings/<node name>.mp4` (or `.mov`) in the project folder. A
file that exists is never overwritten: the next recording goes to `<name>_2`, `<name>_3`, ...
and the status says which. A name without an extension gets the codec's.

The file is finished when recording stops, and also when the node is deleted, the project
closed or the app quit (closing waits up to 5 seconds for that). A file whose recording was cut
off by a crash or a power failure is not playable; recordings are meant to be stopped.

## Codecs

| `codec` | file | what for | size at 1080p60 |
|---|---|---|---|
| `h264` (default) | MP4 | watching, sharing; any player opens it | 0.1 to 10 MB/s, depending on the picture |
| `prores_422` | MOV | editing: every frame a keyframe, 10 bits | up to about 37 MB/s |
| `prores_422_hq` | MOV | editing at a higher quality | up to about 55 MB/s |
| `prores_4444` | MOV | with transparency: alpha kept, 4:4:4, 12 bits | up to about 83 MB/s |

All four are encoded on the CPU by FilmCraft's encoders (pure Rust; no GPU encoder yet). How
fast depends on how busy the picture is far more than on anything else. Measured on a Ryzen 7
5800X3D (8 cores, 16 threads) at 1920 x 1080 with 14 encoder threads, outside the show:

| picture | `h264` | `prores_422` | `prores_422_hq` | `prores_4444` |
|---|---|---|---|---|
| calm (a recorded projection show) | 187 fps | 165 fps | 162 fps | 135 fps |
| busy (the curl-noise example) | 81 fps | 46 fps | 42 fps | 29 fps |
| very busy (a zooming Mandelbrot set) | 88 fps | 48 fps | 44 fps | 26 fps |

(`h264` there with `preset` balanced.) Inside the running show the encoder shares the CPU:
`prores_422_hq` managed about 39 frames per second of the curl-noise example and dropped 40 %
of the frames. The cook thread took about 0.4 ms more per frame while recording. So: `h264`
for recording a busy show in real time, ProRes for calm pictures, smaller sizes or lower rates.

## Sound

With `audio` at `all` (the default), the file also gets the project's sound: what every
`out.audio` plays, added up as the speakers get it. That is after each node's room, `level`,
`mute` and limiter. `audio.mute` silences the room only; the file keeps the sound. A project
without an `out.audio` records no sound track. `audio` off records none either.
For sound without a picture, `out.audio_file` writes a WAV file (see
[Recording sound to a file](audio.md#recording-sound-to-a-file)).

| `codec` | sound | why |
|---|---|---|
| `h264` (MP4) | AAC-LC, 320 kbit/s, stereo | every player opens it |
| ProRes (MOV) | PCM, 24 bits, stereo, uncompressed | editing programs expect it beside ProRes |

The sound is always two channels: a mono `out.audio` is on both, a third channel and up are left
out. Its rate is the rate of the `out.audio` input, usually 48 000 Hz. AAC takes only the
standard rates (44 100, 48 000 and others); at another rate an MP4 records no sound and the
status says why, a MOV takes any rate.

**Picture and sound stay together.** Each frame of sound goes into the file at its show time,
counted from the same start as the pictures. Dropped frames cost no sound. A pause records
neither. A jump back or more than a second ahead continues both without a gap. A jump forward
of less than a second leaves the same time in both: the picture holds a frame, the sound gets
silence (or the sound for the skipped time, for jumps up to half a second). An MP4's sound ends
up to 21 ms after its last picture: AAC comes in blocks of 1024 samples.

`video_out.status` shows the sound under `audio`: `codec`, `rate`, `sources` (the `out.audio`
nodes playing; 0 records no sound), `seconds`, and the samples of silence put in or left out at
jumps.

### H.264 presets

`preset` trades the H.264 encoder's speed against size and quality:

- `fast` (default): Baseline profile, constant quality 23, the encoder's quickest search. The
  files are no larger (often smaller) than balanced's; fine detail is a little softer.
- `balanced`: High profile, constant quality 18, the quickest search.
- `best`: High profile, quality 18, a wider search; about a third slower than balanced again,
  so for calm pictures, smaller sizes or lower rates.

Recorded at 1920 x 1080, 60 frames per second, inside the running app (12 s each, 14 encoder
threads, same machine), the encoder's own speed and the share of frames dropped:

| picture | `fast` | `balanced` | `best` |
|---|---|---|---|
| the demo (reaction-diffusion) | 105 fps, 0 dropped, 1.5 MB/s | 73 fps, 0 dropped, 3.6 MB/s | 50 fps, 16 % dropped |
| full-screen colour noise, 5 octaves, moving | 91 fps, 0 dropped, 1.9 MB/s | 74 fps, 0 dropped, 2.6 MB/s | 52 fps, 13 % dropped |
| the curl-noise example | 75 fps, 0 dropped, 7.2 MB/s | 62 fps, 0 dropped, 13.5 MB/s | 22 fps, 59 % dropped |

The debug build records as fast as the release build: the encoders are always built optimised.

`threads` (under more) sets the encoder threads; automatic (0) takes all logical cores but
two. With 8 a curl-noise recording dropped a third of its frames.

## When the encoder falls behind

Recording never makes the show wait. Each frame is converted on the GPU and read back without
waiting; a few frames queue up for the encoder. When the encoder is slower than the frames come,
the queue fills and frames are **dropped**: the status and the node's warning count them, and in
the file the frame before lasts longer instead, so the recording keeps time with the show. To
drop fewer, use `h264` with `preset` fast for busy pictures, record at a lower `rate` or a
smaller size (`size` fixed, `width` and `height`), or give the encoder more `threads`.

## Rate, time and size

- `rate` 0, shown as "show rate (60 Hz)", records at the show's rate; any other value is the
  file's frames per second. Frames follow the show clock: frame k is the picture k / rate
  seconds after the start. While the clock is paused nothing is recorded. When the show runs slower than `rate` (a heavy network),
  frames last longer (counted as **skipped**); a jump back in time or of more than a second
  forward continues the file without a gap. The sound follows the same clock (see Sound).
- `size` input records at the input's size when recording starts (rounded down to even sides);
  `fixed` scales the input to `width` x `height`, aspect not kept. The size of a running
  recording stays: an input that changes size is scaled to it.

## Looping a recording

A media player that repeats a recording jumps from its last frame back to its first. With
generative content the jump shows: noise never returns to the same picture. A loop is a copy
of the recording made so that a player can repeat it without that jump.

**Automatically:** set `loop` on the `out.video`. When recording stops, the node writes
`<name>_loop.mp4` (or `.mov`) beside the recording and shows "looping..." until it is done.
`video_out.status` shows the loop under `loop`. The params count as they are when recording
stops.

**By command:** `video.loop {file, method}` loops any movie file; `video.loop_status` reports
progress and the result. A loop never overwrites a file.

| method | what it does | use it when |
|---|---|---|
| `cut` | keeps the stretch whose end looks most like its start, at least `loop_min` long (5 s) | the content comes back to similar pictures: slow noise, rotation, cycles |
| `fade` | cross-fades the last `loop_fade` seconds (1 s) into the start | anything; the fade can show as a soft double picture |
| `pingpong` | plays forward, then backward | the content still looks right played backward |
| `cut_fade` | cuts, then cross-fades over the seam | `cut` alone leaves a small jump |

**What cut cannot do:** content that drifts away and never returns has no good cut. The
status gives the seam's `score` and the recording's `motion`, both as mean luma differences.
A `score` no larger than `motion` makes a seam as smooth as any other frame change. A much
larger `score` shows as a jump: use `fade` or `cut_fade`, or record longer, which gives `cut`
more to choose from.

**Players:** many media players stutter at the loop point whatever the file holds. Try the
player before the show. An H.264 file can only start playing at a keyframe: every loop starts
with one and has one every 2 seconds. ProRes has a keyframe on every frame.

**Sound:** a loop has no sound, even when the recording has. Cutting, fading or playing sound
backward at the seam would be heard.

**Time:** the loop runs in the background, on the CPU. A 10 s 1080p60 H.264 recording took 6
to 18 s, depending on the method (pingpong writes twice the frames).

## Colour

Pictures in penumbra are linear light. The recording encodes them with the sRGB curve, the
BT.709 matrix and video range, and tags the file BT.709, as players expect; `tex.video_file` reads
such a file back with the same curve, so a recording played in penumbra looks as it did when
recorded. Values above 1 are clipped. Without alpha (H.264, ProRes 422) the picture is taken
over black; ProRes 4444 keeps the alpha channel.

## Not yet

There is no hardware encoder (NVENC) yet. A recording takes the sum of every `out.audio`; it
cannot pick one of them.
