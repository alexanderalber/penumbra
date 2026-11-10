# Simulations

A simulation is a picture that grows from its own last frame: sand that piles up, waves that
spread. Each one ships as a component in penumbra's library and sits in the add dialog's
`simulations` tab. Its inputs are pictures from the room, such as a scan mask of an object or
the tracked people. Its output is a picture for the projectors, plus a few channels.

## How to use one

- **Add it** from the `simulations` tab, or by command: `comp.load {"file": "sand"}`. Adding
  copies its shaders into the project's `library/<name>/`.
- **Choose a setup** with its main choice param (`scene` in sand). Choosing an entry writes that
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
