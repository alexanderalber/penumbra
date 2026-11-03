# Audio

`audio.device_in` reads an input device. Three nodes turn what it hears into channels that
params can be bound to:

- `chan.audio_envelope` for loudness: one number per channel, how loud it is now.
- `chan.audio_spectrum` for frequency bands: the spectrum as numbers and as a texture.
- `chan.beat_analysis` for music: beat, bar and tempo, with levels and hits.
- `chan.beat_grid` for a sound file: beat, bar, levels and hits of what an `audio.file` plays, found ahead of time.

The energy of one band is an `audio.filter` (`bandpass`) into a `chan.audio_envelope`.

## Sound is its own family

Sound travels on cyan wires of its own kind, `audio`. A `chan` wire carries values: its last
sample is the value now. An `audio` wire carries a stream: each frame's block of samples
continues the block before, so every sample counts. The two kinds do not connect.
`chan.audio_envelope`, `chan.audio_spectrum` and `chan.beat_analysis` are the bridges from
sound to channels that params can be bound to.

- `audio.device_in` reads a microphone or an audio interface.
- `audio.oscillator` makes a test tone. `shape` is sine, saw, square, triangle or noise. The
  params `frequency` and `amplitude` can be bound; a change glides across one frame, so it
  does not click. `rate` is the sample rate, 48000 by default.
- `audio.mix` adds up to eight sounds. Each input has its own gain (`gain0` to `gain7`), and
  `gain` sets the whole. Inputs may have different sample rates. See
  [Mixing rates and devices](#mixing-rates-and-devices).
- `audio.file` plays a sound file (WAV, FLAC, MP3, Ogg Vorbis, or the AAC sound of an MP4, MOV
  or M4A), looped or once, at `rate` whatever the file's rate. See
  [Playing sound files](#playing-sound-files).
- `audio.filter` shapes a sound by frequency. See [Filter](#filter).
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

## Mixing rates and devices

**Rate.** `audio.mix` plays at the rate of its first wired input. Set `rate` (under more) to
pick another, e.g. 48000. Every input at another rate is converted to it. The conversion keeps
the sound flat within 0.01 dB up to 20 kHz. Everything it adds is more than 90 dB below the
sound.

**Pace.** The first wired input sets the pace: the mix makes a block whenever that input brings
one. Wire the sound that matters most for timing to `in0`, usually a generator or instrument.

**Delay.** Two sounds on the show clock at the mix's rate are added sample for sample, with no
delay. A sound at another rate comes about 2 ms late. A microphone or interface
(`audio.device_in`) delivers uneven pieces on its own clock. The mix queues it, and it comes
about 12 to 16 ms late, about one of the device's buffers. The queue also follows the device's
clock when it runs a little fast or slow.

**Gaps.** When a device delivers too little, its sound fades out and back in. When it delivers
far too much at once, the oldest part is dropped with a short crossfade. The node warns with
the counts.

## Playing sound

`out.audio` plays its input on a speaker or headphones. Mono plays on every speaker, stereo on
left and right. `device` is part of an output's name; the `...` menu lists them, and empty is
the system's default output. `level` is the volume in dB, and `mute` silences the node. Both
glide across one frame.

**Room.** Every output plays its sound in a small room by default, so a synthesised sound does
not come out bone dry. `reverb` sets how much room you hear: 0 plays the sound dry, 0.2 (the
default) a small room you just notice, 1 only the room. At 0.2 the room's sound is about 11 dB
below the direct sound. `room` sets how long the sound rings on: 0 dies away in 0.3 s, 0.4 (the
default) in 0.85 s, 0.7 in 1.8 s and 1 in 4 s. `damping` (under more) makes high tones die away
faster, for a warmer room. The room gives a mono sound stereo width; a mono speaker plays the
mean of both sides. `mute` and a pause silence the room's tail too, and a seek clears it. Set `reverb` to
0 when the output feeds a room that already rings, or another reverb.

Every output ends in a limiter that keeps the sound below -1 dBFS. It is always on, because a
sudden blast in a room full of people is the worst thing an installation can do. When the
limiter has to work, the node warns how much it takes off. Turn `level` down until the warning
goes. Several `out.audio` nodes on one device are added together, and their sum is limited too.
They must share one sample rate. To play sounds at different rates on one device, add them in
one `audio.mix` first.

Sound plays while the show runs. Pause the show and the output fades out; play it and the sound
comes back. This holds for a microphone routed to `out.audio` too. `audio.mute` mutes every
output at once: a panic button, also in Helpers > Audio.

The delay from a frame to the speaker has two parts. `latency` is what penumbra queues, 50 ms
by default. The device adds its own buffer, about 30 to 50 ms in Windows' shared mode. A smaller
`latency` answers faster. 30 ms holds on a calm machine, but then a late frame is heard as a
gap. `audio.status` shows the delay and counts the gaps. `audio.measure_latency` plays quiet
clicks and finds them again on an input. The input `loopback:Speakers` records what the output
named Speakers plays, without a cable.

**Recording.** `out.video` records the sound with the picture, each frame of sound at its show
time. The sound is the chain wired to its `audio` input, or else the sum of every `out.audio`
as the speakers get it. This holds for a render slower than real time too, where nothing plays
on the speakers. See [Recording](recording.md#sound).
`out.audio_file` records sound alone, see below.

## Recording sound to a file

`out.audio_file` records sound to a WAV file. Turn `record` on, or press its record button, to
start a new file. Turn it off to finish the file. `audio_file_out.start` and `.stop` do the same
by command. A binding to a channel starts and stops it from OSC or a button. A project always
opens with `record` off.

`source` says what is recorded. With `input` (the default), it is the sound wired to `in`: its
rate, and its channels up to 64. With `all`, it is what every `out.audio` plays, added up as
the speakers get it, in stereo. `audio.mute` silences the room; the file keeps the sound.

`format` `pcm24` stores 24-bit integers, which every editing program opens. Sound above full
scale is clipped. `float32` keeps it, in files a third larger.

The file goes to `file`, relative to the project. Left empty, it goes to
`recordings/<node name>.wav`. An existing file is never overwritten: the recording takes the
next free name with `_2`, `_3`, ... The file appears with the first sound. A recording that got
no sound writes no file.

Each sample goes where its show time falls. A pause records nothing. A jump back, or more than
a second ahead, continues the file without a gap. A shorter jump ahead leaves silence for the
time skipped. When the input changes its rate or channel count, the new sound is left out and
the node warns. Turn `record` off and on for a file in the new format.

The file stays readable while it records: its header is brought up to date every second. A
crash loses at most the last second. A WAV file holds at most 4 GB, about 4 hours of stereo at
48 kHz in `pcm24`. A recording that reaches it is finished there. `audio_file_out.status` shows
the file, its length and what was dropped or left out.

## Playing sound files

`audio.file` plays a recorded sound: music, an atmosphere, a sample. Wire its `out` into an
`out.audio` to hear it.

Its playhead moves as a `tex.video_file`'s does. Playback begins at `start` and follows the
show clock, times `speed`. So a video and its sound stay together when both nodes get the same
file, `start` and `speed`. A node that nothing reads holds its place. After a pause of more
than half a second it goes on where it was.

`speed` works as on tape. At 2 the file plays twice as fast and an octave higher; at 0.5, half
as fast and an octave lower. `play` off fades the sound out and holds the place.

A restart plays the file again from `start`. You restart with the play button in the panel,
with `audio.play {path}`, or by turning `restart` on (bind a channel to cue it). With `loop`
off, a restart plays the file once, as a one-shot sample. `restart_on_show` restarts whenever
a switch or sequence picks the sound again.

**After a crash in show mode** the file plays on from where it was, to the second, instead of
from `start`. A show that follows the music comes back with it. With `position` bound, the
channel says where the file is.

**Jumps.** A change of `start` jumps there at once, and so does `position` set by hand. The
command `audio.seek {path, seconds}` does the same without an edit. The old sound fades out and
the new place fades in, over 4 ms each. A looping file stays in its pass, so `loops` keeps
counting.

**Scrubbing.** Bind a channel to `position`, and the channel says where the file is, in
seconds. `play`, `speed` and restarts are ignored while it is bound. The sound follows like a
tape moved by hand. Moving at real time, the file plays as it is; backwards, it plays
reversed; faster or slower, the pitch follows. A position that stands still is silent. A
position that moves faster than 4 times real time is silent too, so a slider thrown across the
file makes no noise and no clicks. The sound comes back, faded in, once it moves slower again.
Bind the same channel to the `position` of a `tex.video_file` with the same file, and picture
and sound scrub together. A `chan.beat_grid` on the file follows the position: its beat is the
one at the position, and it ticks only while the position moves forward across a beat. Unbind
`position`, and the file plays on from where it stands.

Short files (up to about 87 s of 48 kHz stereo) are decoded into memory, so loops, restarts,
jumps and scrubbing are instant. Longer files are decoded a few seconds ahead while they play.
Scrubbed backwards, a longer file is decoded a few seconds behind instead. A jump far into a
long file can leave a moment of silence; the node counts these gaps and warns. A file that is
missing or cannot be read plays silence and warns. Ogg files with FLAC or Opus sound, and ALAC,
MP1 or MP2 sound, are not played: convert them to FLAC or MP3.

The sound comes out at `rate` (48000 by default), with as many channels as the file has.
`channels` makes it mono or stereo. `audio.file_info {path}` tells the file's codec, rate,
channels and length and where the playhead is.

## Filter

`audio.filter` filters any sound, every channel on its own, at any sample rate. Wire a sound
into `in`; the output has the same channels and rate.

**Types.** `lowpass` removes the highs above `cutoff`, `highpass` the lows below it.
`bandpass` keeps only a band around `cutoff`, `notch` cuts a narrow band out, e.g. a hum.
`peak` turns a band around `cutoff` up or down by `gain` (dB). `low_shelf` turns everything
below `cutoff` up or down by `gain`, e.g. to tame a room's boom. `high_shelf` does the same
above `cutoff`.

**Resonance.** `resonance` 0.2 is a plain filter without a bump (Q 0.7). Higher adds a ringing
peak at `cutoff` for lowpass and highpass, and narrows the band of bandpass, notch and peak.
At 1 (Q 20) a lowpass nearly whistles. `slope` 24 removes twice as much per octave as 12, for
lowpass and highpass.

**Moving it.** `cutoff`, `resonance`, `gain` and even `type` glide to a new value in about
15 ms. Bind `cutoff` to tracking or an lfo: a sweep never clicks or zippers. `mix` (under
more) blends the filtered sound with the input.

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

## Loudness

`chan.audio_envelope` gives one channel per sound channel, with the same names (`ch1`,
`ch2`, ...). Its value is how loud that channel is now.

**Detect.** `rms` (the default) averages the power: loudness as heard. A sine at -20 dBFS reads
-23 dB. `peak` follows the waveform's crests, as a peak meter: the same sine reads -20 dB.

**Attack and release.** `attack` (0.01 s) is how fast the value rises when the sound gets
louder: about two thirds of the way in that time. `release` (0.2 s) is how slowly it falls: to
about a third in that time. A long release holds each hit long enough for the eye.

**Unit.** `linear` (the default) is the amplitude: 1 is full scale, -20 dBFS is 0.1. `db` is
dBFS. `level` maps `floor` to 0 and full scale to 1, ready to drive a param. Quieter than
`floor` (-60 dB) counts as silence: 0 in `linear` and `level`, the floor itself in `db`. There
is no normalising: a quiet microphone reads quiet. `chan.math` scales the value.

Every sample counts, at any sample rate. A device that delivered nothing for a frame does not
count its last block twice.

## Spectrum

`chan.audio_spectrum` measures how loud each frequency band is now. `bands` (32, from 4 to
512) run from `min` (30 Hz) to `max` (16 kHz). With `spacing` `log` (the default) each band is
the same share of an octave, as the ear hears; `linear` gives each the same width in Hz. A
`max` above half the sample rate ends there, with a warning. `channel` picks one channel;
empty mixes all.

**Values.** `unit` is `level` by default: 0 at `floor` (-60 dB), 1 at full scale. `linear` and
`db` are as for the envelope: a sine at -20 dBFS reads 0.1 or -20 dB in its band. A band rises
over `attack` (0 s, at once) and falls over `release` (0.2 s).

**Outputs.** `out` is one channel, `spectrum`, whose samples are the bands, low to high. The
viewer draws it as a curve over the band number. `texture` is the same numbers as a picture,
`bands` pixels wide, in grey. `history` (1) sets its rows. With more rows, row 0 at the top is
now, and every new block of sound moves the older rows down by one: a scrolling spectrogram. In
a shader, `uv.x` is the frequency band and `uv.y` how long ago. The texture is made only while
something reads it; its rows start empty when it is read again.

**Resolution.** Each frame looks at the newest 11 to 85 ms of sound (512 to 4096 samples at
48 kHz): more for narrow bands, less for wide ones. Bands below about 200 Hz are narrower than
that resolution with 32 log bands. A tone there spreads into its neighbours, and with many
bands neighbouring low bands repeat one value.

## Setting up

1. Add an `audio.device_in` (Tab > audio > device_in). With `device` empty it reads the system's
   default input; the empty `device` box names that device in grey (hover the box for the device
   a typed name picked). `audio.status` names it too. The `...` menu beside `device` lists the inputs of this machine; part of a
   name typed in works too, e.g. `Focusrite`. The [Helpers window](helpers.md#audio) shows a
   level meter per channel. The device is opened in shared mode, so other programs can use it too.
2. Connect it to a `chan.beat_analysis`.
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
`chan.beat_analysis` in between and bind to the analysis.

## What the analysis gives

All values are channels of the `out` output, in this order:

| channel | what |
|---|---|
| `level` | loudness, 0 to 1 |
| `low`, `mid`, `high` | loudness of the bands below 150 Hz, between, above 2.5 kHz (params `low`, `high`) |
| `onset` | 1 in the frame a sound starts (a hit, a note), else 0 |
| `kick` | 1 in the frame a sound starts in the low band |
| `beat` | 0 at each beat, rising to 1 at the next: drive anything that should move with the beat. Keeps running at the last tempo while not `locked` |
| `tick` | 1 in the frame a beat starts, only while `locked` or `coasting` |
| `bar` | 0 at each bar's first beat, rising to 1 at the next bar |
| `bar_beat` | the beat within the bar: 0, 1, 2, 3 |
| `bpm` | the tempo, 0 until a beat has locked once |
| `confidence` | 0 to 1: how much more clearly the sound repeats at that tempo than at the other tempos in range. Noise and speech stay below 0.15; music with a clear beat reads 0.4 to 0.8 |
| `rms`, `peak` | the raw level, full scale is 1 |
| `beats_per_bar`, `low_hz`, `high_hz` | the params `beats_per_bar`, `low` and `high` as they are (bound ones evaluated), for what draws the analysis |
| `locked` | 1 while a beat is heard: the tempo has held with a `confidence` of at least 0.2 for about 2 seconds. 0 on noise, speech and silence. Multiply what pulses with `beat` by `locked` to stop it there |
| `coasting` | 1 when the lock was lost but the sound still has onsets at the tempo: `tick` goes on from the running beat for up to 30 seconds. Never 1 while `locked`. Gate with `locked` alone to stop at once, with `locked` plus `coasting` to ride through a weak passage |

With `normalize` on (the default), a quiet microphone still spans 0 to 1. `level`, the bands
and the spectrum are measured against their range over the last 4 seconds. A single knock
does not squash them: it is too short to set the top of that range. Sound below `floor`
(-60 dB) counts as silence: the values read 0, no onsets or beats are found in it, and the range stays as it was. If a silent room
does not read 0, raise `floor` above the room's noise. If quiet sound reads 0, lift it with
`trim` (up to 24 dB), which changes the analysis only.

`level` covers 20 Hz to 16 kHz, the three bands together. `rms` and `peak` are the whole signal
as it comes in, including noise above the hearing range that some interfaces add at 96 kHz.
A microphone on one input of a stereo interface arrives 6 dB quieter when both channels are
averaged: set `channel` to its input.

The second output, `spectrum`, is one channel whose samples are the loudness of `bands` (32)
bands from 30 Hz to 16 kHz, low to high, each 0 to 1 over 60 dB.

## The meters in the viewer

Selected, the analysis shows meters in the viewer, made to be read at a glance during a show:

- **Level and bands:** `level`, `low`, `mid` and `high` as bars (in the colours their curves
  have in the plot), each with a white line that holds its peak for a second, then falls.
- **Tempo:** `bpm` in large type, `--` until a tempo is found. The less sure the analysis is,
  the dimmer the number; the bar below it is `confidence` (red below 0.2, amber, green from 0.4). Without a beat
  the caption says "no beat".
- **Beat light:** a disc that flashes on each beat, amber on a bar's first, blue on the others,
  in a ring that fills with `beat` from one beat to the next. It flashes only while `locked`
  or `coasting`; the ring dims while no beat is locked. While coasting the caption says
  "coasting".
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
an octave apart, so half or double the tempo is not taken.

The beat locks (`locked` reads 1) after about 2 seconds of a clear beat, so 4 to 6 seconds
after the music starts. It lets go about 4 seconds after the music stops or falls below
`floor`. When speech follows the music, letting go takes about 9 seconds: the tempo is measured
over the last 8 seconds, and the music has to leave them first. While no beat is locked, `tick`
stays 0 and the beat light stays dark. `beat` and `bar` keep running at the last tempo, so
visuals stay in time through a breakdown. Gate them with `locked` where they should stop.

A breakdown or a loud room can drop the lock while the beat goes on. Then the beat
**coasts**: `coasting` reads 1 and `tick` keeps coming from the running beat. It needs onsets
in the last second and a half that still repeat at the tempo, with a `confidence` of 0.1 or
more. It ends when they stop, and at the latest 30 seconds after the beat last counted as
clear. Noise alone, speech and silence do not coast.

The beat follows small drifts within a second or two. A jump of the beat by a quarter or half
of a beat, as at a hard cut in a DJ mix, takes about 9 seconds to follow. A short stretch of
off-beat hits (a fill, a syncopated break) therefore leaves the beat where it was.

In a loud room, with crowd noise or a fan near the microphone, turn on `superflux` (in the
`more` section). The onsets then ignore the slow wobble of broadband noise, so the beat locks
sooner and holds longer. In clean sound and in reverb it finds the beat a little less well, so
it is off by default.

`beat` is meant to be in time with what is heard. A projector shows a picture a few frames
late; `offset` moves beat and bar ahead by that many milliseconds. Set it by eye: flash
something on `tick` and raise `offset` until the flash meets the kick.

The first beat of a bar is a guess: it is the beat where the sound changes most (bass lines
and chords tend to change there). With the same pattern on every beat the guess may land on
another beat; `bar_beat` and `bar` are then counted from there.

## Beats of a sound file

`chan.beat_grid` gives beat and bar for the file an `audio.file` plays. Wire the file's `out` into its `audio` input, the same wire a `chan.beat_analysis` takes. Its channels mean the same as the analysis's (`beat`, `tick`, `bar`, `bar_beat`, `bpm`, `beats_per_bar`, `locked`, `coasting`), so bindings keep working when one node replaces the other. It adds `downbeat` (1 in the frame a bar begins), `beat_index` and `bar_index`.

**Levels and hits.** `level`, `low`, `mid`, `high`, `onset` and `kick` are the analysis's too, with its default settings. The node measures them over the whole file beside the beats and reads them at the playhead. The live analysis reports them 20 to 30 ms after the sound; the grid has them on time. So a binding moved from `chan.beat_analysis` to `chan.beat_grid` gets the same values, early by that lag. `offset` moves them ahead with the beat. The levels take about 40 seconds for a 2 hour mix and keep about 9 MB in memory and in the cache. Until they reach the playhead, they read 0.

**Finding the beats.** The node finds the beats of the whole file once, in the background, with the Beat This! network (Foscarin, Schlüter and Widmer, JKU Linz, ISMIR 2024; [github.com/CPJKU/beat_this](https://github.com/CPJKU/beat_this)). A 4 minute track takes a few seconds, a 2 hour mix a few minutes. The result is kept in a cache, so the next time the file opens the grid is there at once. While the analysis runs, `progress` rises from 0 to 1, `analyzing` reads 1, and the panel shows a progress bar with `cancel`. Both count the levels too.

**Twice or half the tempo.** In some passages the network counts the half beats, for example in a hi-hat run, or only every second beat, for example in a sparse intro. The node folds such passages back onto the tempo of the minute around them: it drops the extra beats and inserts the missing ones, evenly spaced. Gaps of up to four beats are filled; longer breaks stay as they are. A tempo change between two tracks stays too. On a 2 hour mix this fixed a half-time intro and two passages at double tempo, of 16 and 29 seconds. `beats.grid` gives both: `beats` are the beats the node uses, `found_beats` the network's.

**Bars.** The network marks each beat that may begin a bar. On DJ mixes these marks alone give bars of one, two or five beats, so the node does not take them one by one. Each beat looks at the 64 beats around it, and the pattern of 4 (or 3) beats that the marks favour there sets the bars. On a 2 hour mix this made 98 % of the bars four beats long, against 79 % from the marks alone. Where the mix shifts by a beat, one bar of 3, 5, 6 or 7 beats takes up the change. A waltz gets bars of 3. `beats.grid` gives both: `downbeats` are the bars the node uses, `marked_downbeats` the network's marks.

**Loops.** When the `audio.file` loops, the last beat of the file runs on into the first beat of the next pass, and `beat_index` and `bar_index` start again from there. Levels and hits run on across the end too; a hit at the file's very start counts in every pass.

**Using it early.** The grid fills from the start of the file. `ready` reads 1 where the grid under the playhead is final. The grid is final about a minute of music behind the last beat found: the tempo the beats are folded onto takes the minute after each beat, and the bars of the last 32 beats wait for the beats after them. At the analysis's speed that minute takes a second or two. Further on, `coasting` reads 1 and the beat runs on at the last tempo found. The panel says how far the grid is ready.

**Timing.** The grid is known ahead, so `offset` (milliseconds) makes `tick` come exactly that early, without guessing. Set it to the projector's delay.

**Overview.** The viewer shows the whole sound file above and `span` seconds around the playhead below. Switch between it and the curves with `view: overview` and `view: plot`. The waveform's colour mixes bass (red), mid (green) and treble (blue). Grey lines are bars and ticks are beats. The red line is the playhead, and the box in the upper lane is what the lower lane shows. Grey stretches have no levels measured yet. A veil with an orange line marks where the grid is not final yet. A click or a drag plays from there (`audio.seek`). While the `audio.file`'s `position` is bound to a channel, the status bar says so instead. The `overview` output is the same picture as a texture (`width`, `height`), drawn only while something reads it.

**Sections.** Once the beats and the levels of the whole file are known, the node finds its tracks and the sections within them, in about a tenth of a second. A mix's tracks come from where the sound changes at the scale of 48 bars, sections from changes at the scale of 8. `section_index` and `track_index` count from 0. `section_label` numbers the kind of sound: the same number anywhere in the file where it sounds alike. `section_letter` numbers the repeats within a track (0 is A). `phrase_bar` counts the bars of an 8-bar phrase, and `phrase` rises from 0 to 1 over it. `bars_to_boundary` and `seconds_to_boundary` count down to the next section, and `build` rises from 0 to 1 over the 8 bars before it. `section_seed` is a number fixed per label, for random looks that come back with the music. Mixes blend one track into the next: `transition` rises from 0 to 1 through such a blend, and `transition_from` and `transition_to` are the two labels. Until the sections are found, `sections_ready` and all these channels read 0. A looped file's end counts as a boundary. On a 2 hour mix the node found about 40 tracks, 187 sections and 10 kinds. The overview's strip shows the sections, coloured by label, with their letter in the lower lane.

**Moving by the music.** `travelled` counts beats since the file's start, at a steady pace. Move a world by it: a road scrolls one cell per beat, a camera flies on, a galaxy turns. `travel_bpm` is the pace: the grid's tempo, kept between `travel_floor` and `travel_cap` (80 and 180 BPM by default) and smoothed over 1.5 s. So the world does not stop in a break without beats, and it does not jump when the next track is faster. `beat_length` is how many travelled beats one grid beat takes, 1 where the tempo is steady. Travelled beats and grid beats part slowly, through the clamps, the smoothing and bars of 3 or 5 beats. `drift` measures that: the bars the grid counts (`bar_index` plus `bar`) minus `travelled` divided by the file's usual bar length (4 for most music). To put an event on a downbeat in travelled units, place it where `travelled / 4 + drift` is a whole number; an element with a period of 2 bars takes `drift` modulo 2. On a 2 hour mix `drift` grows to about 43 bars, a few hundredths of a bar per bar. In a looped file `travelled` starts at 0 again in each pass.

**Energy and drops.** `energy`, `energy_low`, `energy_mid` and `energy_high` are level, bass, mid and treble on one scale for the whole file. 0 is the quietest 5 % of the file, 1 the loudest 5 %. They are smoothed: 0.05 s to rise, 0.3 s to fall. `level` and the bands follow the last four seconds, so a quiet intro reads as loud as the peak. `energy` does not: a breakdown reads lower than the full beat. `section_energy` is the mean `energy` of the section under the playhead. `drop` is 1 where a section begins with at least 0.2 more `section_energy` than the one before, for example where the kick comes back after a breakdown. It falls to 37 % within 0.5 s, so it can flash a light or kick off an effect. The `sections` table has the same mean as its `energy` column. All these channels read 0 until the sections are found. `beats.status` says `bus: {ready: true}` then.

**A section's character.** `section_change` says how strongly the music changes where the section under the playhead begins, from 0 to 1. Scale a change of the look by it: a small step of the palette below 0.3, a new element around 0.5, a new scene near 1. On a 2 hour mix every track start reads 0.84 or more. Drops and the starts of breakdowns mostly read 0.5 to 1. Most changes within a track read below 0.3. The number combines how sharply the sound changes (`strength`), the jump in `section_energy` and a track start. `section_role` says what the section does in its track: 0 intro (the first section), 1 break (few kicks, or much quieter than the rest of the track), 2 build (rises into a drop), 3 drop, 4 outro (the last section), 5 main. `section_bass` is the section's mean `energy_low`. `section_density` counts its onsets per beat: about 2 in the example track (kick, snare, hats), from 0.5 to 4 on a 2 hour mix. `section_kick` is the share of its beats with a loud kick, near 0 in a breakdown. All read 0 until the sections are found.

**Cues.** The `grid` output is a table with one row per beat: time in the file, beat, bar, beat in bar, downbeat. `beats.grid {path}` gives the same over the control channel; `beats.status` and `beats.analyze` show and start an analysis. The `sections` output is a table with one row per section: start, end, bars, label, letter, track, strength, the blend zone, energy and the character (bass, density, kick, jump, role as a word, change). `beats.grid` lists the same as `sections`, and `beats.status` counts them. The example [beat-grid](../examples/beat-grid/README.md) shows the grid beside the live tracker on the same music.

**Cues from the sections.** `beats.cues {path, sequence, level?, pieces?, transition?}` writes the steps of a `chan.sequence` from the sections a `chan.beat_grid` found. `history.undo` takes the whole edit back. `path` is the grid, `sequence` the `chan.sequence`. There is one step per section, or one per track with `level: "track"`. A step's piece is named by the section's kind (`kind0`, `kind1`, ...), by its letter (`pieces: "letter"`: `A`, `B`, ...) or by its track (`pieces: "track"`). `names` lists the pieces in number order: piece k is `in<k>` of the `tex.sequence`, and `piece` equals `section_label`. Where the music blends from one section into the next, the show cross-fades over the same seconds, and the next step begins where the blend ends. `transition: "cut"` cuts at each section start instead.

The command also binds the sequence's `clock` to the grid's `seconds`: the playhead plus `offset`, within the pass for a looped file. The show then stands at the section under the playhead, also after a seek, a loop or a change of `offset`. `hold` and `go` do nothing then. The steps are a copy: run `beats.cues` again after the sound file changes. When the grid finds no beats (no network file, a file it cannot read), it warns and `seconds` still follows the playhead, so the show still follows the music.

**An arrangement from the sections.** `beats.arrange` writes what a music stage shows in each section into a table, one row per section. Example: `{path: "/grid_beats", table: "/arrangement", sequence: "/cues", slots: {floor: ["/blocks", "/mountains"], sky: ["/galaxy", "/sun"], mid: ["/glider"]}, stage: "/stage"}`. The table is a `table.text` in csv: `start` and `end` in seconds, `role`, `change`, then the `presence` of each element (0 to 1; the column is named like the element's node), `warmth` and `intensity` (0 to 1). Each slot shows one element at a time, the first one at the start. At a boundary with `change` below 0.3 only the colour moves on. From 0.3 one slot swaps to its next element, the slots taking turns. From 0.7 every slot swaps. The colour walks on at every boundary by `change` times 0.25 and never walks back; `warmth` goes from 0 up to 1 and down again along that walk. The role sets the rest: an intro or outro is calm (floor at 0.4, `intensity` 0.3), a break empties the floor and dims the sky to 0.6, a drop shows everything at `intensity` 1. Elsewhere `intensity` is the section's energy. The slots named `sky` and `mid` follow the sky's rules: they stay on through a break at 0.6 and are fully there in an intro or outro. Every other slot follows the floor's rules, `ceiling` too. A glider in `mid` then keeps flying through a breakdown.

**Weights.** A slot of many elements shows each one rarely. Give an element a weight to show it more often: write `{path: "/glider", weight: 3}` in place of `"/glider"`, as in `mid: [{path: "/glider", weight: 3}, "/particles", "/debris"]`. An element of weight 3 comes on three times as often as one of weight 1. The default is 1, the most 100. A swap takes the element owed the most turns, never the one already on, so a heavy element's turns spread out evenly. With equal weights a slot goes through its elements in order. The same file always gives the same table. The `post` slot ignores weights.

**Post, camera, emptiness and stages.** The slot `post` holds effects over the finished picture, such as `post_scanlines`. Each of its elements is on or off by itself, so several can be on at once. All are off in an intro, outro or break and all come on at a drop. Elsewhere a swap of the slot switches its next element on or off, and a change from 0.7 switches every one. The `camera` column picks one of the stage's six camera presets: 0 in a main section, 1 in a build, 2 on a drop, 3 in a break. Intros, outros and a main section that starts a track take a variant, 4 and 5 by turns from track to track. Emptiness: a boundary of 0.7 or more into a main section, after at least 3 full sections, may leave only the stage and one slot. The chance is `change` times 0.5, drawn from the section's seed, so the same file always gives the same table. The `stage` column says which of several stages shows. It changes only at a track start with `change` 0.8 or more, and at most once in 8 minutes. With one stage it stays 0. The `gaps` column says how much of the floor breaks off into gaps: 0.3 in a build, 0.6 in a break, 0 in every other section. It binds the stage's `gap`, so the gaps open and close through the fades. Give other shares as `gaps: {"build": 0.5, "break": 0.8}`.

Edit the table by hand to change a row: the picture follows at once. Running `beats.arrange` again overwrites the table. The command also fills `sequence` with a step per row, its `clock` on the grid's `seconds`, and creates readers beside the table: `<table>_now` and `<table>_next` read the row under the playhead and the one after it, and `<table>_mix` blends them by the sequence's `mix`. `<table>_mix` has one channel per number column. It binds each element's `presence`, `warmth` on `stage` and on each element that has it, and with `intensity: "/stage.dim"` that param too. `<table>_pick` (with `_half`, `_snap` and `_jump`) jumps from one row to the next half way through each fade, for the columns that are a number of something: it drives the stage's `camera`, and with `switch` the param that picks the stage. `stage` takes one path or a list of stages side by side; `switch: "/stages.index"` binds a `tex.switch`, and on a `tex.cross` the switch follows `_mix` and fades. A fade into the next row lasts the blend zone, at least `fade_min` seconds (default 2), also where the music cuts: elements sink and rise over that time. `start` and `end` in the table only show where a section lies; the fades come from the steps, which `beats.arrange` writes. `warmth` is a line between the stage's warm and cold palette for now.

## Tips

- Analyse one channel with `channel` (e.g. `ch1`) when the others carry something else.
- `chan.filter` smooths a channel; `chan.math` scales it.
- `chan.envelope` in `trigger` mode turns `onset`, `kick` or a beat's `tick` into a flash:
  attack, decay, `hold`, release, each in seconds.
- `chan.trail` after the analysis keeps its last seconds as samples; the viewer and the
  trail's own body plot them with a lane per channel, so `bpm` does not flatten the levels.
- The level meters in Helpers should stay out of the red. A microphone close to the speakers
  clips; turn down its gain.
