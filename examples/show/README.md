# show

A show as a list of cues, the way an event runs one. Five cues follow each other with fades:
dawn, day, a storm, night and an end card. The space key moves to the next cue. So do a GO
button and an OSC message `/go`. A caption under each cue comes from a table.

Look at `/out`. The show starts at the first cue and waits there. Press space with the
editor or the viewer in focus, and dawn fades into day over 3 seconds.

## How it is built

**Cues.** `/show` is a `chan.sequence`. Its `steps` list one cue per line: the piece, its
seconds, the transition into the next cue and its length.

```
# cue   seconds  transition  length
dawn    hold     fade        3
day     hold     fade        2
storm   8        flash       0.8
night   hold     fade        3
end     hold
```

`hold` instead of seconds keeps the cue on screen until the next GO. The storm has 8 seconds:
it plays them and moves on by itself, with a flash into night. `/screen` (a `tex.sequence`)
makes the picture: input `in0` is the first name in `names`, `in1` the second, and so on.
The end card has no input wired, so it is black.

**The looks.** `/dawn` is a `tex.ramp` from orange to blue, `/day` a `tex.primitive` sun on
blue, `/storm` a moving `tex.noise`, `/night` a round `tex.ramp` with a pale glow in the
middle. Any picture can be a cue.

**GO.** `/show`'s `go` param is bound to `/go`. Each time `go` rises past 0.5, a holding cue
starts its transition, and a timed cue ends early. A GO during a transition does nothing, so a
double press does not skip a cue. `/go` (a `chan.math` with `op` `max`) is 1 while the space
key (`/keys`) is down, and for one frame per press of the OSC address `/go`.

**OSC.** `/go_button` is a `chan.button` named `go`. `/desk` (a `chan.osc_out`) sends it as
`/go` to port 8000 on this machine, and `/osc` (a `chan.osc_in`) receives it there. This loop
stands in for a lighting desk or QLab. `/osc` lists `/go` under `triggers`, so every
message `/go 1` (or `/go` alone) to port 8000 is one press and moves the show on. `/go 0`
counts as a release, not a press. Two presses within one frame still count as two. Helpers >
OSC lists what arrives. To send from another computer, turn on `lan` in `/osc`. `/osc` keeps
no other address as a channel, so other addresses cannot steer the show. **learn from OSC** in a param's link menu binds that param to the next address that
arrives: start it, then press GO, and the param follows `/go`.

**Caption.** `/cues` is a `table.text` with one row per cue: `cue`, `hour` and `rain_db`.
`/cue` (a `chan.from_table`) turns one row into channels. Its `row` is bound to `/show`'s
`step`, so each cue reads its own row. `/caption`'s `text` is bound to the `hour` channel with
the format `{.0}:00`, so it shows `6:00` at dawn and `22:00` at night. `/out` lays the caption
over the picture.

**Show screen.** `/go_button` has `show` ticked. With show mode on, the editor shows a plain
screen with the GO button below **Unlock**, and a click on it moves the show on.

## Before the show

Lock the project: **Show mode** in the editor's menu (right-click on empty canvas), Ctrl+Shift+L,
or `show.lock` over the control channel.
Edits, saving and project switches are then refused. Keys, OSC and the GO button keep working.
Show mode also restarts penumbra by itself after a crash (exit code 3: the GPU device lost or
the editor failed), at most 3 times within 10 minutes. Test that once with the show
running: `penumbra-cli ctl gpu.simulate_loss confirm=true`. The app closes and opens again
within a few seconds, with the clock running on. The cue list goes on at the cue that was
playing, and a holding cue still waits for GO. A timed cue may repeat up to a second.
`show.unlock`, or **Unlock** clicked twice, ends show mode. See
[show mode](../../docs/getting-started.md#show-mode).

## Try

- A number instead of `hold` in `steps`, for example `day 6 fade 2`: day plays 6 seconds and
  moves on by itself. With `mode` `loop` the show starts over after the end card.
- `black` instead of `fade` dips to black between two cues; `cut` cuts.
- Another line in `/cues`, for example `6,23,-60`, then a sixth cue in `steps` and `names`.
- Rain under the storm: add an `audio.ambience` with `preset` `rain`, and an `out.audio`
  with the ambience wired into it. Bind the ambience's `level` to `/cue`'s `rain_db` channel.
  The column holds the level in dB for each cue: -10 in the storm, -24 at night, -60 (all but
  silent) in the other cues. A `glide` of 2 s fades the rain in and out.
- Lit windows at night: add a `tex.shapes` with the line
  `window rect 0.18,0.3 0.06,0.09 repeat 6x4 0.11,0.15` in `shapes`, and a `tex.composite`
  with `/night` in `in0` and the shapes in `in1`. Wire the composite into `/screen`'s `in3`.
  The shapes are white on transparent, so they need the composite over a picture.
