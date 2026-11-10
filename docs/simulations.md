# Simulations

A simulation is a picture that grows from its own last frame: sand that piles up, waves that
spread. Each one ships as a component in penumbra's library and sits in the add dialog's
`simulations` tab. Its inputs are pictures from the room, such as a scan mask of an object or
the tracked people. Its output is a picture for the projectors, plus a few channels.

## How to use one

- **Add it** from the `simulations` tab, or by command: `comp.load {"file": "sand"}`. Adding
  copies its shaders into the project's `library/<name>/`.
- **Choose a setup** with its main choice param (`scene` in sand and waves, `preset` in growth and chladni). Choosing an entry writes that
  entry's values into the params below it, as one undo step. Then the values are yours: tune
  any slider, and the project saves the tuned value. A line under the choice names the params
  that differ from the entry, with a "back" button. The mechanism is the component's presets:
  [comp.base, Presets](nodes/comp.base.md#presets).
- **Start over:** a new entry starts the simulation over by itself. Bind the component's `reset`
  to a key for a restart by hand. Held, `reset` keeps the simulation at its start.
- **Size:** `width` and `height` on the component are the simulation's size in cells. The
  inputs can have any size; the component samples them.
- **Params no entry writes** stay as you set them, such as `seed`, `pause`, `width` and
  `height`. `node.info` lists the params the entries write under `component.presets`, `owns`.

## sand

![The ten scenes of sand, each after a few seconds to half a minute](../assets/simulations/sand-scenes.jpg)

Falling sand: grains pour, fall, slide down slopes and pile up on walls. A grain never vanishes
or doubles on its way. Only a pour, a drain, an open edge and `in_wall` `delete` change the
count. The [sand example](../examples/sand/README.md) feeds it a word as walls and two moving
dots as sources.

**Scenes.** `scene` picks one. Each writes the walls, the start, where sand comes from, the
edges and the palette.

| scene | what you see |
|---|---|
| `steady` | Rain on the wall input. The floor is open, so as much sand falls out as comes in. |
| `letters` | Rain on the wall input over a closed floor. Heaps grow on every shape and the floor fills up. |
| `blob` | Sand pours from the bright parts of the source input, onto the wall input. |
| `gravity` | A band of layered sand on the wall input, nothing pours. Turn `gravity`. |
| `hourglass` | Layered sand runs through a narrow neck. |
| `cups` | Rain into three bowls that overflow. |
| `pegs` | A heavy, narrow stream through a board of pins. |
| `ramps` | A narrow rain on a zigzag of shelves. |
| `drain` | Rain over bowls and a floor of sand. The drain input swallows sand. |
| `mover` | A bar moves through a pile of layered sand. |

**Inputs.** `wall`: white where sand cannot go, while `object` is on. Grains under a wall that
appears move out on top of it (`in_wall`). `source`: sand pours where it is bright, with
strength `blobs`, in its colour when `colors` is `source`. `drain`: sand vanishes where it is
bright, at the rate `drain`.

**Outputs.** `out` is the picture, 1920 x 1080 (`out_width`, `out_height`). `grains` is the
number of grains, as a channel.

**Params that matter most.** `amount` scales the pour of every scene: bind it to a sound level
to pour with the music. `gravity` is the direction of the fall in degrees, 0 down and 90 right;
bind it to tilt the world. `steps` is sand steps per frame (48): fewer is slower sand and costs
less. The setup a scene writes can be mixed: `shape` puts a built-in wall (`hourglass`, `cups`,
`ramps`, `pegs`, `bar`) under any scene, `rain` with `rain_from` and `rain_to` is the rain line
along the top, `pour` the chance per step that a bright source cell makes a grain, `edges`
closed, open or wrapping. The `grains` group sets how grains move (`fall`, `slide`, `roll`,
`sticky`). The `look` group sets the palette, the light on the piles and how visible the walls
are. For projection onto a real object, set `wall_fill` and `outline` to 0.

**In an installation.** Feed a scan mask of the object into `wall`, so sand piles on the real
object. Feed `tex.blob_track`'s picture of the people into `source` and choose scene `blob`, so
sand pours where people stand. A drain can be any mask: a hole in the floor, a hand.

**Cost.** About 3 ms per frame for the whole example: 640 x 360 cells, 48 steps, picture
1920 x 1080, on an RTX 3090 at a 280 W power limit. The cost grows with `width` times
`height` times `steps`.

**Limits.** Slopes carry little sand; with `amount` above about 2 on a narrow stream, piles grow
steeper than 45 degrees. The `mover` bar and the colour over time (`colors` `time`) follow the
show clock, so a restart does not restart them.

## growth

![The eleven presets of growth, each after 8 to 30 seconds, and window in a warm spell](../assets/simulations/growth-presets.jpg)

Slow growth of three kinds: frost crystals that grow over glass and melt where people stand,
branching trees like coral and lightning (diffusion-limited aggregation, DLA), and colonies of
lichen, mould and rust that spread and die. Growth starts on the edges of a mask, at bright
points or by itself. The [growth example](../examples/growth/README.md) feeds it a word as the
mask, a soft moving blob as a person and seven dots.

**Presets.** `preset` picks one. Each writes what grows (`model`), where it may grow (`area`),
what is there at the start, the steps per frame, whether it starts over when full, every setting
of its model, how visible the inputs are and the colours.

| preset | what you see |
|---|---|
| `window` | Window frost. Crystals start on the mask's edges and here and there and cover the glass in about 20 s. A warm spell every 60 s melts it all, then it grows again. People melt paths, which freeze again with fingers growing in from their edges. |
| `snowflake` | One six-armed crystal from the centre in still, cold air. It stops at about half the picture, stays 20 s and starts over. People do not melt it. |
| `fern` | Four-armed, strongly aligned frost from the mask's edges only, with warm spells. |
| `edges` | Frost from the mask's edges only. It covers everything in about 20 s and starts over. People do not melt it. |
| `dots` | One crystal from each bright point of the dots input. |
| `trail` | Window frost without warm spells. What people melt is the only change. When the glass is full, it starts over. |
| `coral` | Ragged coral trees from the mask's edges and the bottom, around the mask and not into it. |
| `lightning` | One thin tree from the centre, a Lichtenberg figure. It starts over at 5 % of the picture, after about 20 s. |
| `lichen` | 14 colonies spread with growth rings and stop where they meet. After about a minute they die from their centres and are grown over. People do not harm them. |
| `mould` | Pale colonies inside the mask only. People wipe them away. |
| `rust` | Rust patches spread from the mask's edges. People wipe them away. |

**Inputs.** `mask`: the object, white where it is. Growth starts on its edges, and with `area`
`inside` or `outside` it grows only on one side of them. `heat`: people. Frost melts and
branches and colonies die where it is bright, at the strength `heat`. `dots`: bright points.
With `dot_seeds` on, a crystal starts at each. The inputs can have any size. An unwired input
counts as black.

**Outputs.** `out` is the picture, 1920 x 1080 (`out_width`, `out_height`). `coverage` is the
share of the area that is covered, from 0 to 1, as a channel. The area is where growth may
happen: outside the mask for `coral`, inside it for `mould`.

**Params that matter most.** `speed` scales the steps per frame of the preset (`steps`): 2 grows
twice as fast and costs twice as much. `heat` is how strongly people melt or wipe away, 0 for
not at all. `afterglow` is the warmth a person leaves behind: at 0 a melted path freezes again
within seconds, at 2 it stays open for many seconds. `auto_restart` starts over by itself once
the share `full` has been covered for `hold` seconds; key 0 on `reset` starts over by hand. The
setup a preset writes can be mixed: `model` with `area`, `pattern`, `count` and `radius`, and
`edge_seeds`, `dot_seeds` and `spawn` for where new growth starts. The `frost` group shapes the
crystals (`arms`, `aniso`, `latent`, the cooling, the warm spells). The `branch and lichen`
group shapes trees and colonies (`eta` from dense blobs to thin lightning, `grow`, `supply`,
`lifespan`, `spread`). The `look` group sets the colours and the light on the ice. For
projection onto a real object, set `through` and `warm` to 0, so neither the mask nor the
people show in the picture.

**In an installation.** Feed a scan mask of the object into `mask`, so frost starts on the
object's edges or coral grows around it. Feed `tex.blob_track`'s picture of the people into
`heat`, so frost melts where people stand and grows back behind them.

**Cost.** About 1.5 ms per frame for the example with `window` (12 steps) and about 2.3 ms
with `snowflake` or `lightning` (32 steps): 960 x 540 texels, picture 1920 x 1080, on an RTX
3090 at a 280 W power limit. About 0.5 ms of it is the `coverage` measure, which waits for the
GPU's answer. The cost grows with `width` times `height` times the steps per frame.

**Limits.** The full mark of `coral` and `lightning` depends on the mask: another shape may
need another `full`. `snowflake` stops at half the picture; that is the physics of a closed box.
The covered phase of `window` changes little between about 20 s and the warm spell. A
regrown path reads as a comb of parallel fingers, coarser than the first growth. Covered ice
shows a few lighter rectangles with straight edges.

## chladni

![The eight presets of chladni at one figure each, and the shaped plate cut to a star and to a letter](../assets/simulations/chladni-presets.jpg)

Chladni figures (cymatics): sand lies on a vibrating plate, jumps away from the parts that move
and gathers on the lines that stand still. Every tone gives another figure. The plate is a
square or a round one, or any shape from a picture. The
[chladni example](../examples/chladni/README.md) plays a melody to it and gives it a guitar
outline as the shape.

**Presets.** `preset` picks one. Each writes the plate (`plate`, `membrane_steps`, `size`,
`turn`) and the whole look: the view, the colours, how visible the plate is.

| preset | what you see |
|---|---|
| `square` | Light sand on a dark square plate. |
| `gold` | Gold sand on the square plate. |
| `circle` | Light sand on a round plate: rings and spokes. |
| `ice` | Pale blue sand on the round plate. |
| `shape` | Light sand on a plate cut to the input `shape`: lines that follow its outline. |
| `ember` | Orange sand on the plate cut to the input `shape`. |
| `motion` | The shaped plate's motion in blue under the sand. |
| `lines` | The nodal lines of the square plate, drawn crisp, with no sand. |

**Inputs.** `tone`: a channel named `note`, a note number (60 is middle C). `chan.audio_pitch`
gives it from a microphone; any channel named `note` works. Figure 0 is the note `lowest` (48,
C3), and each semitone up is the next figure, 20 figures in all. Notes outside are held at
figure 0 or 19. A new tone never starts over: the sand is tossed up for `toss_time` (0.25 s)
and the new lines are whole again after about 0.75 s. `shape`: the plate's outline for `plate`
`shape`, white on the plate. Any size; keep its sides at 16:9 or the outline is stretched. An
unwired `shape` is black, so the shaped plate shows nothing.

**Outputs.** `out` is the picture, 1920 x 1080 (`width`, `height`). `motion` is how far a grain
moved per step lately, in pixels, as a channel. It is about 0.2 when the sand lies on its lines,
0.4 on the shaped plate, and about 9 while the sand is tossed at a new figure.

**Params that matter most.** `source` `figure` makes the figure follow the param `figure` (0 to
19) instead of the tone. `morph` blends two neighbouring figures by the fraction of a semitone.
`level` is how hard the plate is played: bind it to the music's loudness, 0 lets the sand rest.
`kick` throws every grain up: bind it to an envelope of the beat. `speed` scales the grain
steps per frame (4 at 1): 2 gathers twice as fast. `detune` multiplies the shaped plate's pitch:
sweep it to find the tones where a shape gives clean lines. The `grains` group sets how sand
jumps and slides, the `membrane` group the shaped plate's damping and where it is driven. For
projection onto a real object, set `outline` to 0, so only the sand shows.

**The shaped plate is a membrane.** penumbra simulates a stretched membrane with the wave
equation. A metal plate bends instead, which this simulation does not model. So its figures are
of the same kind as a metal plate's, and their exact lines differ from those of a metal plate of
that outline. The square and round plates use the formulas of a membrane with free edges, which
are exact for a membrane too. `membrane_steps` is the
membrane's steps per frame. The shaped presets write 64, so a figure settles within about a
second. The other presets write 1, since square and round plates do not use the membrane.

**In an installation.** Feed a microphone through `chan.audio_pitch` into `tone`, and sing,
whistle or play one note at a time. Chords and loud drums confuse the pitch tracker. Feed a scan
mask of an object into `shape` and choose preset `shape`, so the figures follow the object's
outline.

**Cost.** About 1.6 ms per frame for the example with preset `square`, and about 4.3 ms with a
shaped plate (64 membrane steps), with 262 144 grains at 1920 x 1080, on an RTX 3090 at a 280 W
power limit. The membrane's steps are most of the difference. A million grains (`grains` 1024)
cost about 2.8 ms on the square plate.

**Limits.** Figures 2 and 3 of the shaped plate are the same. Figure 0 of the shaped plate is a
ring around the driving point, since its tone lies below the membrane's lowest one. Dense lines
show a fine lattice of holes, more at `speed` 2. High figures on the round plate keep a grey
haze in the calm middle. With nothing wired into `tone`, the inner `pick` node warns, also when
`source` is `figure`; the figure is still right.

## waves

![The eight scenes of waves, and double_slit and plane in the energy view](../assets/simulations/waves-scenes.jpg)

Ripples on water (the wave equation): drops fall as rain, on a beat or by themselves, steady
sources make straight or round waves, and the waves reflect off walls, bend around objects and
interfere. A moving person pushes the water. The level of the water stays where it is: a drop
lifts as much water as it pushes down. The [waves example](../examples/waves/README.md) feeds it
a word as the mask, a moving blob as the people and a metronome as the beat.

**Scenes.** `scene` picks one. Each writes the walls, the wave sources, the drops, the start,
the edges, what people do and the look.

| scene | what you see |
|---|---|
| `pond` | Rain on a tiled pool. The mask input stands in the water and the rings bounce off it. People push the water. |
| `drop` | One drop in the middle, a train of rings. Once the water has been calm for 1.5 s, the next drop falls at a random place. |
| `double_slit` | A straight wave from the left runs through two slits. Two fans of waves interfere behind the wall. |
| `two_points` | Two bobbing points: the textbook picture of interference. |
| `plane` | A straight wave rolls past the mask input. It reflects in front and bends into the shadow behind. |
| `wake` | People moving faster than the waves leave a V on open water. A drop falls after 4 s of calm water. |
| `beat` | One big drop on each beat of the beat input. |
| `posts` | Rain among round posts over a sandy floor. |

**Inputs.** `mask`: white where an object stands in the water, while `use_mask` is on. Waves
reflect off it and bend around it. `people`: bright where someone is. With `push` `move` the
water is pushed where the picture changes, so a moving shape leaves a wake. With `push` `press`
the water is held down under the bright part, as under a person standing in it. `beat`: a
channel named `beat`; each rise above 0.5 drops a drop while `rain` is `beat`. The pictures can
have any size; an unwired one counts as black.

**Outputs.** `out` is the picture, 1920 x 1080 (`out_width`, `out_height`). With `cutout` on,
the walls are black and transparent. `ripple` is how much the water moves, as a channel: 0 when
still, about 0.04 in `pond`, 0.03 in `two_points`, under 0.02 in `drop`. It needs a multiply
before it drives a light.

**Params that matter most.** `speed` scales the water steps per frame (8 at 1): 2 runs the water
twice as fast and costs more. Damping counts per step, so at a higher speed rings also fade
sooner. `push_strength` is how hard people push. `damping` is how fast rings fade. The setup a
scene writes can be mixed: `shape` puts a drawn wall (`slit`, `double_slit`, `grating`,
`pillars`, `box`) under any scene, `source` a steady wave maker (`plane`, `point`, `two`),
`rain` drops at random or on the beat, `start` the water at the start, `edges` a rim that
reflects, open water that absorbs, or edges that wrap. `calm_drop` drops a drop after `idle`
seconds of calm water, so a quiet scene never stays dead. The `look` group picks the view: the
lit `water` over a floor, the `height` in colours, the `energy` (where the water moves on
average, which shows interference best) or a grey `relief`. The `surface` group sets the light
of the water view.

**In an installation.** Feed a scan mask of the object into `mask`, so the waves bounce off the
real object, and turn on `cutout`, so nothing is projected onto it. Set `marker` to 0 so the
people are not drawn. Feed `tex.blob_track`'s picture of the people into `people`, so they leave
a wake. Feed an envelope of the music's beat into `beat` and choose scene `beat`.

**Cost.** About 1.4 ms per frame for the example in scene `pond`, and about 2.9 ms at `speed` 4
(32 steps), with a simulation of 1280 x 720 and a picture of 1920 x 1080, on an RTX 3090 at a
280 W power limit. The cost grows with `width` times `height` times the steps.

**Limits.** The wave equation has no dispersion. Real water has it: a drop leaves a widening
train of rings, a slow boat a wake of its own shape. Here a dent travels as one ring of fixed
shape. So a drop starts as a train of rings already, and a person slower than the waves leaves
only a dimple travelling with them. Wave speed and drop sizes count in texels, so on a larger
`width` and `height` the same waves look smaller and slower. The water view is a stylised pool:
refraction only shifts the floor, and the caustics are a trick of the curvature. `drop` repeats
itself: the next drop comes about 4 s after the last, always of the same shape. A shape that
presses lowers the mean level while it stands there. `pause` still runs the step's passes,
which leave the water as it is.
