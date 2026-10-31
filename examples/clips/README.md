# clips

A video file looped with its sound, the way a VJ program plays a clip. `/clip` shows a clip of
3.6 seconds over and over, `/sound` plays the clip's sound with it, and `/record` can record
the result as a movie with sound.

Look at `/clip`. The sound plays on the system's default output; `device` in `/speakers` picks
another.

The clip shows 81 pendulums from above, swinging out from the centre and back. They fall into
three groups by their distance from the centre, and the groups swing a third of a swing apart.
Each time a group passes its rest point, its pendulums flash and it strikes its note: A, C or
E. `clip.mp4` is 540 x 540 pixels at 60 frames per second, H.264 with AAC sound, 400 KB.

## How it is built

**Picture.** `/clip` (tex.video_file) plays `clip.mp4`. Its `loop` is on, so the first frame
follows the last without a gap. `/screen` (out.projector) shows the clip on a display or
projector once its `display` is set; the [Helpers window](../../docs/helpers.md#outputs) lists them
under **Outputs**. Until then `/screen` warns that no display is chosen. The viewer's `window`
button also shows `/clip` in a window of its own.

**Sound.** `/sound` (audio.file) plays the sound of the same file. `/speakers` (out.audio) plays
it with a little room: `reverb` 0.2. The clip's own sound has no reverb.

**In step.** `/clip` and `/sound` both start at their `start` (0 s) when the project opens, and
both move on with the show clock. The clip's picture and sound are equally long: 216 frames at
60 fps and 172 800 samples at 48 kHz, 3.6 s each. So the sound stays on the picture through
every loop. **Pause** in the status bar holds both, and play goes on from there. After a stall
of more than half a second, or when the show clock jumps back, both go on one frame from where
they were. A change to only one of the two moves them apart: `speed`, `play`, `start` or
`restart` on `/clip` alone, for example. To change both at once, bind the param of both to one
channel, as in the "Try" list below.

**Recording.** `/record` (out.video) records `/clip` with the sound of every out.audio, here
`/speakers` with its reverb. It is off when the project opens. Select `/record` and press the
record button above its params to start, and again to stop (or `video_out.start` and
`video_out.stop` over the control channel). The movie lands in `recordings/record.mp4` beside
the project. For the example opened from the menu, that is
`%LOCALAPPDATA%\penumbra\examples\clips\recordings`. The next recording gets `record_2.mp4`.
With `codec` set to a ProRes, the movie is a MOV with uncompressed sound.

## How the clip was made

penumbra rendered it. `source/project.ron` is the project. Three `chan.lfo` swing at one rate,
1/3.6 Hz, a third of a wave apart. `/notes`, an `audio.instrument` (preset `mallet`, key A,
scale `minor`, `spread` 2), plays a note each time one of them passes 0. `swing.wgsl` draws the
pendulums from the show time. Picture and sound repeat exactly after 3.6 s.
`source/render_clip.py` runs that project headless, records three swings with `out.video`, and
has ffmpeg keep the middle one. During the first swing the notes from before it ring out, so
the middle swing ends exactly as it begins. Run the script with no arguments to make
`clip.mp4` again. Opened in penumbra instead, the source project records
`source/render/take.mov` (about 60 MB) between 7.2 s and 18 s of show time. It needs Python, ffmpeg on PATH and penumbra built in release
(`cargo build --release -p penumbra`).

## Try

- **Another clip:** set `file` in `/clip` and in `/sound` to the same file, an MP4 or MOV with
  AAC sound. For a silent video, point `/sound` at a WAV, FLAC, MP3 or Ogg file of the same
  length.
- **Speed:** add a `chan.slider` with `min` 0.25, `max` 2, `value` 1 and `name` set to `speed`.
  Bind `speed` in `/clip` and in `/sound` to its channel. The slider then changes both at once.
  At 0.5 a swing takes 7.2 s and the notes sound an octave lower, as on tape.
- **Start over together:** add a `chan.button` and bind `restart` in `/clip` and in `/sound` to
  it. A click starts both at their `start`.
- **An echo:** add a `tex.time_machine` with `/clip` in `in` and `offset` 18 (frames, 0.3 s).
  Add a `tex.composite` with `op` set to `screen`, `/clip` in `in0` and the time machine in
  `in1`, and wire it into `/screen`. Each pendulum gets a copy that follows it 0.3 s later.
