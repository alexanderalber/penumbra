# music-synthwave

A complete music show on one song, with no hand-placed cues. A synthwave world flies to
krzna's "Tell Me What You Want" (5:14, 120 BPM). At the drop after the break, the world rolls up
into a tunnel. penumbra finds the song's beats, sections and break, and one command writes the
arrangement from them.

The song is by krzna and comes with this example with the producer's permission. It is not under
penumbra's licences; see [MUSIC-LICENSE.md](MUSIC-LICENSE.md).

Look at `/out`, from the start of the song. The sound plays on the system's default output.
`/preview` (an `out.projector`, off) shows `/out` in a window of its own when you set `active` on.

**The first open waits for the grid.** `/music` has `wait_for_grid` on, so it stays silent while
`/grid_beats` analyses the file. The analysis runs on the CPU: about 11 s on a Ryzen 7 5800X3D, 32 s with other
work on that CPU. The picture stands still until then. Later opens find the result in the cache and start at once.

## What to look at

| time | what happens |
|---|---|
| 0:02 to 0:13 | the song is nearly silent; sky and glider first, then the mountains, then the lamps |
| 0:13 | first drop: every slot swaps, wireframe blocks shoot out of the floor on the beat, the sun, the HUD and the scanlines come on |
| 0:32 to 2:22 | a change about every 16 s, each swapping one slot: palms, horse, mountains; sun, aurora, skyline; particles, lasers, glider |
| 2:22 | the break: no bass for 26 s. The floor breaks off in gaps, the glider flies on over nothing, only the galaxy stays in the sky |
| 2:48 | the drop: the floor rolls up into the tunnel over 4 s, with the fade's middle on the drop |
| 2:48 to 4:54 | the tunnel: rings of equaliser bars fly past in every row; glider, lasers or particles between them |
| 4:54 | the outro: a calm camera, lasers and equalisers under the aurora until the end |

The world moves one grid cell per beat. The glider's lane turns every two bars, on a downbeat.
Over the last 8 bars before a new section, nebula and galaxy brighten.

## How it is built

**The network.** The top level reads left to right, 17 nodes and 3 notes. `/music`, `/speakers`
and `/grid_beats` are the sound and its grid. `/arrangement` holds the table. `/floor_slot`,
`/ceiling_slot`, `/sky_slot` and `/mid_slot` hold the elements. `/stage` and `/tunnel` draw them,
`/stages` shows one of the two, `/post_slot` draws over the picture, `/out` and `/preview` show
it. `/lanes`, `/camera_mix` and `/lift` connect the rest. Most components have a note inside.

**Elements.** Two stages and 15 elements from penumbra's library, copied under `library/`. Edit
those copies to change this project only. [The library's catalogue](../../library/README.md#a-stage-for-music)
documents each one.

| slot | elements | shows on |
|---|---|---|
| floor, `/floor_slot` | `mountains`, `blocks`, `palms`, `animal` (a horse of lines) | `/stage` only |
| ceiling, `/ceiling_slot` | `lights` (two rows of lamps with a running light) | `/stage` only |
| sky, `/sky_slot` | `galaxy`, `sun`, `aurora`, `city`; `sun` is for `/stage` only | both |
| mid, `/mid_slot` | `glider`, `particles`, `lasers`; `equalisers` under them in the tunnel | both |
| post, `/post_slot` | `scanlines`, `hud` | over both |

Each slot is a component: its elements, a merge, and a `_show` select that stops an element at
`presence` 0 from cooking. The glider sits outside `mid_show`, so it can fly away after its
`presence` reaches 0.

**The music bus.** `/grid_beats` (chan.beat_grid) finds the beats, bars and sections ahead of time.
Its whole output goes into `bus` on both stages and every element, through the slots'
`grid_beats_in`. Each one reads the channels it needs: `travelled` moves the world, `drift` places
the lane's turns, `beat` times the blocks, `energy_low` brightens the sky. After a seek, the world
stands where it stands at that second.

**The arrangement.** `/arrangement/table` has one row per section: `start` and `end` in seconds,
`role`, `change`, the `presence` of each element, then `warmth`, `intensity`, `camera`, `stage`,
`gaps`, `flight`, `pattern` and `see_through`. This project has no reader for `pattern`. The rest
of `/arrangement` plays the table on the song's seconds: `table_mix` blends through each fade,
`table_pick` jumps half way through it, and bindings carry the values to the nodes. `table_mix_out`
goes by wire into the slots and `/camera_mix`.

This call wrote the table, and it writes it again for another song:

```
beats.arrange {path: "/grid_beats", under: "/arrangement",
  slots: {
    floor: [
      {path: "/floor_slot/mountains", stages: ["/stage"]},
      {path: "/floor_slot/blocks", stages: ["/stage"]},
      {path: "/floor_slot/palms", stages: ["/stage"]},
      {path: "/floor_slot/animal", stages: ["/stage"]}
    ],
    ceiling: [{path: "/ceiling_slot/lights", stages: ["/stage"]}],
    sky: [
      {path: "/sky_slot/galaxy", weight: 2},
      {path: "/sky_slot/sun", stages: ["/stage"]},
      "/sky_slot/aurora", "/sky_slot/city"
    ],
    mid: [
      {path: "/mid_slot/glider", weight: 2},
      "/mid_slot/particles", "/mid_slot/lasers",
      {path: "/mid_slot/equalisers", stages: ["/tunnel"], base: true}
    ],
    post: ["/post_slot/scanlines", "/post_slot/hud"]
  },
  stage: ["/stage", "/tunnel"], switch: ["/stages.index", "/lanes.index"],
  intensity: ["/stage.dim", "/tunnel.dim"],
  dwell: 3, stage_fade: 4.0, find_break: true, split_bars: 8, stage_gap: 120}
```

No cell of the table is edited by hand. On this song the call gives 21 rows:

- `find_break` finds the break from 2:21.90 to 2:47.88.
- `split_bars: 8` cuts the sections every 8 bars, so a slot swaps about every 16 s.
- `stage_gap: 120` allows the stage to change at the drop after the break. The stage stays on
  `/tunnel` to the end, since there is no second break.
- `base: true` keeps the equalisers on in every tunnel row, under the element whose turn it is.

A second run changes no cell and creates no node. An edit to a cell shows at once, and the next
run overwrites it. `start` and `end` only show where a section lies: changing them moves nothing.

**Two stages.** `/stages` and `/lanes` are `tex.switch` nodes with `blend` on and `inactive: skip`.
Their `index` follows the table's `stage` column, so only the stage on show renders, and both
during the fade. `/lanes` hands the mid elements the lane of the stage on show. `/camera_mix` does
the same for the camera: it adds the share `stage` of the difference between the two poses. By
binding, `roll_up` on both stages follows the same share, so the floor rolls up into the tunnel.

**Frame cost.** A cook pass takes 4.2 to 6.3 ms at 1920 x 1080, as the median in five rows from both stages (RTX 3090, headless, viewer on
`/out`). The project allocates about 1.2 GB of GPU memory.

## Recipes

**Swap an element.** This puts `floor_pillars` in the place of the palms:

1. `comp.load {file: "floor_pillars", path: "/floor_slot/pillars"}` copies it into `library/`.
2. Wire it: `node.connect {from: "/floor_slot/grid_beats_in", to: "/floor_slot/pillars", input: "bus"}`
   and the same from `lane_in` to its `lane`.
3. `node.connect {from: "/floor_slot/pillars", output: "out", to: "/floor_slot/floor", input: "in2"}`
   takes the input of `palms`.
4. On `/floor_slot/floor_show`, set `channels` to `mountains blocks pillars animal`.
5. `node.delete {path: "/floor_slot/palms"}`.
6. Run the call above with `/floor_slot/pillars` in place of `/floor_slot/palms`.

**Use your own song.**

1. Choose **Save copy as** in the folder menu, so your changes stay.
2. Copy the sound file into the project's folder and set `file` on `/music` to its name.
3. `beats.analyze {path: "/grid_beats"}` waits for the analysis.
4. Run the call above without any change.

The stage changes on the drop after a stretch of 4 bars or more without bass, when that drop comes at
least `stage_gap` seconds (120 in the call) after the song's start. A song with no such drop stays on
`/stage`, so the tunnel never shows. For a short song, lower `stage_gap`.

**Record a film.** `out.video` records `/out` with the sound of `/music`. It is not part of this
project. Add it for the take:

```
node.create {path: "/film", kind: "out.video", params: {audio: "input", rate: 60, file: "film.mp4"}}
node.connect {from: "/out", to: "/film", input: "in"}
node.connect {from: "/music", to: "/film", input: "audio"}
audio.seek {path: "/music", seconds: 0}
video_out.start {path: "/film"}
```

The song ends after 5:14.57. `video_out.stop {path: "/film"}` finishes the file. A window of the
app drops a frame when a frame takes too long. For a film with every frame, use `cook.stepping`
and `cook.step` in a [headless script](../../docs/control.md). The whole song that way took 6:15 on an RTX 3090: 18 875 frames at 1920 x 1080, none dropped, 1.6 GB with `encoder: gpu` and `preset: best`.

## Try

- In `/arrangement/table`, set a row's `camera` (0 to 5) or `warmth`, and seek to that row's `start`.
- On `/stage`: `bars_per_turn` (1 for a turn every bar, 4 for calmer flights), `swing`, `turn`.
- On `/floor_slot/blocks`: `rise` (0.05 for a hard snap, 1 for blocks that grow over a beat).
- On `/sky_slot/galaxy`: `tilt`, `size`, `twist` and `arms` for another galaxy.
- `b` on `/lift/build_lift`: how far the light lifts before a change (0 for none).
- `offset` on `/grid_beats`: a projector's delay in milliseconds.
