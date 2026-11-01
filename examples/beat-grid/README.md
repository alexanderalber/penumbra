# beat-grid

A sound file's beats and bars, found ahead of time, beside the live beat tracker. `/music`
plays `music.wav` over and over. `/grid` (chan.beat_grid) and `/live` (chan.beat_analysis) both
take its sound, and `/out` shows them side by side: left the grid, right the live tracker.

Look at `/out`. The sound plays on the system's default output; `device` in `/speakers` picks
another.

`music.wav` is 32 bars at 124 BPM, about 62 seconds: kick, snare, hats, a bass line and a chord
that change every bar, a fill every 8 bars. Bars 17 to 24 are a breakdown without kick and
snare. The file ends where bar 33 would begin, so the loop keeps the beat. It is mono, 16-bit,
22 050 Hz.

## How it is built

**The grid.** `/grid` does not listen to the sound. It asks which file `/music` plays and where
its playhead stands. The Beat This! network finds the beats and downbeats of the whole file
once, in the background. The result is kept in a cache (`%LOCALAPPDATA%\penumbra\cache\beats`),
so the next open has the grid at once. While the analysis runs, `analyzing` is 1, `progress`
rises from 0 to 1, and the node's panel shows a progress bar. `ready` is 1 where the grid under
the playhead is final. Beyond that, `coasting` is 1 and the beat runs on at the last tempo.

**Levels.** `/grid` also gives `level`, `low`, `mid`, `high`, `onset` and `kick`, the values
`/live` gives. It measures them over the whole file once, beside the beats, and keeps them in
the same cache. A binding moved from `/live` to `/grid` keeps its channel and loses the lag.

**Offset.** A projector shows each frame some milliseconds late. `offset` on `/grid` moves the
grid that much ahead. The beats are known, so `tick` comes exactly that early, with no guessing.
`/live` has an `offset` too, but it has to predict.

**Live.** `/live` listens to the sound while it plays. It takes a few seconds to lock on, and it
follows the kick. In the breakdown it slips half a beat onto the hats and finds back some bars
after the kick returns. `/grid` stays on the beat. The live tracker is the one for live input:
a microphone, a DJ mixer, a band. For a file, the grid is right from the first beat and through
breaks.

**Picture.** Both halves are drawn alike:

- The disc flashes on `tick`, each beat. `/grid_flash` lets it fade over 0.15 s.
- The orange ring is `downbeat`, the first beat of each bar. The live tracker has no
  `downbeat`: `/live_first` is 1 while its `bar_beat` is 0, so its ring stays lit for that whole
  beat.
- The blue line grows with `bar`, from 0 to 1 over each bar. `/grid_sweep` scales it to the
  half's width.
- `beat 1` to `beat 4` is `bar_beat` plus 1 (`/grid_count`).
- The thin line under the left title is `progress`, shown while `analyzing` is 1.
- The yellow bar at each half's right edge is `low`, the bass. Both nodes give it, and they
  agree. `/live` reports it about 20 ms after the sound; `/grid` has it on time.

**Table.** `/beats` shows the grid's second output, `grid`: one row per beat with `time`
(seconds into the file), `beat`, `bar`, `beat_in_bar` and `downbeat`. An `out.table` writes it
to a CSV, for a cue list. Over the control channel, `beats.grid {path: "/grid", from, to}`
gives the beat times between two moments, and `beats.status` tells how the analysis stands.

## How the music was made

`tools/beat_grid_music.py` synthesises it from numbers alone: Python's standard library, no
samples. Run it with no arguments to write `music.wav` again; every run writes the same file.
With `--times DIR` it also writes the beat and downbeat times it placed.

## Try

- `offset` in `/grid`: 100. The flashes come a tenth of a second before the kick.
- `fall` in `/grid_flash`: 0.4 for longer flashes.
- `file` in `/music`: a track of your own. Its first open analyses it; a 4 minute track takes a
  few seconds.
- Open the example a second time: the grid is there from the first beat, and only the live
  tracker has to find the tempo again.
