# Concepts

## How the network runs

This section says when a node does its work, in what order, and what decides it. For someone
reading the code, [ARCHITECTURE.md](../ARCHITECTURE.md#cooking) has the same from the inside.
Words used here are also in the [glossary](glossary.md).

**Cooking.** A node *cooks* when it computes its outputs. penumbra runs one cook pass per show
frame, 60 a second. A pass starts from what is wanted: every node without outputs (projectors,
recorders, senders such as `out.ndi` or `out.osc`), the node in the viewer, and the visible
thumbnails while there is time left. From there it pulls: before a wanted node cooks, the nodes
feeding it cook, and the nodes feeding those. A node nothing wants does not cook and costs
nothing. The editor marks it "not cooked".

**When a node cooks again.** Only when something it depends on changed:

- one of its params changed, by hand, from the control channel or through a binding;
- a node feeding it made a new result;
- it follows the clock (a noise that moves, a video, a shader that reads `pn_time`) and the
  clock moved;
- a slow job of its own finished (a file loaded, a frame decoded).

Otherwise its last result is reused. So a still picture behind a heavy filter costs that filter
once, not every frame. The reference says for each node type whether it follows the clock.

**When a node fails,** it keeps putting out its last good result and shows the error. A typo
in a shader during a show leaves the projector on the last working picture.

### Wires, bindings and node references

Three ways a node can use another, each with its own line in the editor:

| | what it carries | drawn as | across a component's edge |
|---|---|---|---|
| **wire** | a whole output (picture, channels, table, 3D) into an input of the same kind | a solid line in the kind's colour | only through its [ports](#components) |
| **binding** | one channel's value or one table cell into one param | a dashed line into the node's top, green from a channel | anywhere |
| **node reference** | a node named in a param (`tex.select`'s `node`) | a dashed line | anywhere |

All three make the source cook first. A bound param recooks its node only when the value it
takes changes. Wires may not form a loop, except into a feedback input (below). Bindings are
explained [below](#bindings), references under [Paths](#paths).

### Inputs and nodes that cook differently

- **Feedback (`previous`).** The `previous` input of a `tex.feedback` or a `chan.feedback` sees
  what its source made in the frame before. The wire into it may run backwards and close a loop;
  it is drawn dashed. This is the only way to build a loop. A binding closes a loop the same way:
  bind to a `chan.feedback`, and wire the end of the loop into its `previous`. The source of a
  `previous` input cooks in every frame its feedback node cooks, even when nothing else shows it.
  A `tex.feedback`'s `repeat` runs its loop several times per frame (see
  [A step of several nodes](shaders.md#a-step-of-several-nodes)).
- **Inputs a node skips.** `tex.switch` with `inactive: skip` and `tex.sequence` cook only the
  inputs they show. So do `geo.merge` and `tex.composite` with a `show` input: channel k of
  `show` says whether `in<k>` shows, and at 0 or below it does not. The chains behind the others
  stand still and cost nothing until they are shown again. A feedback loop, cache or time
  machine there carries on from where it stopped.
- **Steering inputs.** `tex.sequence`'s `sequence` input and the `show` input of `geo.merge`
  and `tex.composite` decide which inputs are needed. So they cook first, before the inputs.
- **Every frame (eager).** `chan.clock` and `chan.sequence` cook in every pass, wanted or not.
  So a clock sees its `run` turn off and on while the piece it drives is skipped. The reference
  marks such nodes "Cooks in every frame".
- **Outputs made on demand (lazy).** Some outputs are made only while something reads them:
  `chan.beat_analysis`'s `meters`, `tex.ndi_in`'s `audio`, `tex.fluid`'s `velocity`. Wire one
  up, show it in the viewer or reference it, and the node makes it from then on.

### Time

- **The show clock** counts frames at 60 a second and starts at 0 when a project opens. Every
  node that follows the clock reads the same time in a pass. It is paced by a projector's
  display, or by a timer when no output runs (see [Helpers](helpers.md#outputs)).
- **Late frames are skipped.** When a pass takes longer than a frame, the clock jumps ahead to
  where it should be. Animations stay in step with the wall clock and with the music.
- **Pause** (the status bar, or `cook.play`) stops the clock. Everything that follows it stands
  still. Edits, cameras and incoming OSC still show.
- **Cues and restarts.** A `chan.sequence` whose steps `hold` is a cue list: its `go` moves
  it to the next cue (see [Shows](#shows)). A piece or a clip starts over
  when a channel turns on: a `chan.clock`'s or `chan.sequence`'s `run`, a `tex.video_file`'s
  `restart`. Bind that channel to a key, an OSC message or a show step (see [A clock per
  piece](#a-clock-per-piece), [Shows](#shows), [Video clips on cue](#video-clips-on-cue)).
  Feedback loops, caches and other nodes that keep state start over with R, the reset button
  or `node.reset`.

## Nodes, families and data kinds

A project is a network of nodes. Each node has a kind, written `family.name` (`tex.noise`,
`chan.lfo`), inputs, outputs and params. An output connects only to an input of the same data
kind, and the wire takes that kind's colour:

| kind | colour | what flows | families that make it |
|---|---|---|---|
| `tex` | purple | 2D pictures on the GPU, linear light, premultiplied alpha, values above 1 allowed | `tex` |
| `chan` | green | named channels of numbers; the last sample is the value now | `chan`, `script` |
| `audio` | cyan | sound: a gapless stream of sample blocks at a sample rate | `audio`, `tex` (`ndi_in`) |
| `table` | blue | rows of text cells | `table`, `script`, `physics` |
| `geo` | orange | meshes with parts, materials and transforms | `geo`, `physics` |
| `mat` | pink | a material | `mat` |
| `light` | yellow | one or more lights | `light` |
| `view` | silver | one camera | `view` |
| `vol` | grey-blue | a volume: a grid of cells filling a box in space, on the GPU | `vol` |
| `bodies` | brick red | one group of rigid bodies: their starts, their model, what they collide as and what they are made of | `physics` |

Some nodes take any number of inputs of one kind: `tex.composite`, `tex.switch`,
`tex.sequence`, `chan.merge`, `geo.merge` and `light.merge`. Their inputs are numbered from 0
(`in0`, `in1`, ...). The node shows the wired ones and one free input below the last; wire into
the free one and the next appears. An input left empty in between stays, because the order counts:
the composite's layers, the switch's index. `node.connect` without `input` takes the next free one.
Up to 64 fit.

The families:

- **`tex`**: pictures. Generators (noise, constant, text, image, video, a PSD layer), filters
  (level, blur, transform, ...), compositing, a shader node for anything else, `tex.mapper`
  for projection mapping, `tex.render` for 3D.
- **`chan`**: numbers that change over time. Generators (`lfo`, `noise`, `constant`), `math`,
  `filter`, `trail` (the last seconds of a channel as samples, to plot or to feed on; it starts
  empty and fills up), and OSC
  in and out. Channels drive params through [bindings](#bindings).
  A channel holds one or many samples, so a set of channels is a small grid of numbers:
  `tex.from_chan` turns it into a picture (a row per channel, a pixel per sample) for shaders to
  look values up in. `chan.from_tex` measures a picture, or a few areas of it, as channels: the
  mean (at 1 x 1, its mean colour), the sum, the smallest or largest value, or the share of pixels
  above a threshold. With `layout` `texels` it reads every texel of a small picture as channels
  of their own, named by `names`. A sum of 0 and 1 counts pixels exactly (see
  [chan.from_tex](reference/nodes.md#chanfrom_tex)). Channels named `r`, `g`, `b` (and `a`), as
  `chan.from_tex` makes them, come back from `tex.from_chan` as a colour.
- **`audio`**: sound. A device input, a test tone, a mix, ready-made instruments and drones.
  `chan.beat_analysis` turns sound into channels (see [Audio](audio.md)).
- **`table`**: text tables, typed in or read from a file, for scripts and OSC.
  `table.file` reads a CSV, TSV or text file and reloads it when the file changes.
  `chan.from_table` turns a table of numbers into channels, a channel per column (or row), named
  by its first cell. A param bound to a channel follows the last value in that column (or row).
  `table.from_chan` goes the other way: channels become the cells of a table, so an LFO or a
  tracker can place what a table places, such as the bodies of a `physics.bodies`. `out.table`
  writes a table to a CSV or TSV file. With `file` empty it writes `export/<node name>.csv` in
  the project folder. In mode `changes` it writes whenever the table differs from what the file
  holds; in mode `manual` it writes only on `table_out.write`. `overwrite` on replaces the file
  in place, for a program that keeps reading it; off, every write gets a new name with `_2`,
  `_3`, ...
- **`vol`**: volumes, such as smoke. `vol.smoke` simulates smoke in a box: emitters blow hot
  smoke in, the smoke rises against `gravity`, swirls and fades. `vol.emitter` is one emitter;
  wire several into the `emitter` inputs of `vol.smoke`, 64 at most, later ones acting on what
  earlier ones left. Any table with columns x and y (see `vol.emitter`'s output) gives one
  emitter per row, so a computed table can move or multiply them. With no emitter the box stays
  empty. Fuel in the smoke burns once it is hot enough: it heats, makes more smoke and swells,
  so a jet of it widens like a flame. A `vol.obstacle` on the `obstacle` inputs is a still sphere or box the smoke flows around.
  `tex.render_volume` draws a volume as a picture, seen from the front through the whole box. Hot
  smoke glows with `glow`, in the colours of a palette picture on its `palette` input. Its
  `density` output shows how much smoke covers each pixel, as grey, for `tex.lookup`. Burning
  smoke is dense: a `thickness` well below 1 lets the eye see into it. `vol.smoke` keeps its
  smoke in the state snapshots. Its params carry NVIDIA Flow's names and defaults where they mean
  the same; how strongly a number acts was matched by eye to one Flow scene.
- **`physics`**: bodies that fall and collide. A `physics.bodies` is one group of bodies: a table
  with one row per body places them, a model shows them, and its params say what they collide as
  (`hull`, `box`, `sphere`) and what they are made of (`friction`, `bounce`, `density`). Its
  `mode` says who moves them: the solver (`dynamic`), the table on every frame (`driven`, a hand
  that pushes the others aside) or nobody (`fixed`).
  `physics.rigid_bodies` takes one group on each of its `bodies` inputs. The bodies of all groups
  fall under `gravity`, hit each other, a floor and the geometry on its `obstacles` input, and
  come to rest. Its `out` is geometry: every body where it is now, drawn as the model of its
  group, for `tex.render`. Its `table` lists where the bodies are, in the columns `geo.instance`
  reads, with the number of their group. A dynamic group starts over when its table or its shape
  changes; the other groups go on (see [the solver's page](nodes/physics.rigid_bodies.md)).
- **`geo`**, **`mat`**, **`light`**, **`view`**: a 3D scene. Geometry from files or
  primitives, transformed and merged; materials put on it with `geo.assign_material`; lights and a
  `view.camera` wired into a `tex.render`, which makes a picture of it all (a render needs its
  camera; the camera is perspective, orthographic or calibrated). `light.projector` throws a
  picture through a camera onto the geometry: a virtual projector, for trying a mapping on a
  model of the room. Coordinates are metres, z up.
  `geo.file` reads OBJ, STL, glTF and FBX. FBX works from file version 7.0 on, binary or
  ASCII. Binary is the default of Blender, Maya, 3ds Max and Unity. An FBX 6 file gives an
  error: export it again as FBX 2011 or later. Each FBX model becomes a part with its name,
  placed as in the file, in metres and z up. Materials bring their diffuse colour and texture,
  embedded or from a file beside the FBX.
  Animated models play: each part moves as its model does in the file. A skinned character
  bends with its bones: each vertex follows the bones that weigh on it, blended by the weights
  (linear blend skinning). `take` picks the animation (the `takes` output lists them), `play`,
  `speed` and `loop` work as for a video, and with `play` off a channel bound to `time` drives it.
  Posing runs on the CPU and costs about 20 ns per vertex and frame. A file whose skin asks for
  dual quaternion blending is blended linearly, with a warning; twisting joints then thin out a
  little. Blend shapes, cameras and lights are not read.
  `out.mesh` writes geometry to an OBJ, STL or glb file, every model where it stands in the
  scene. Each part stays a part: an OBJ object or a glTF node. Only glb keeps material colours;
  textures are not written. File, `overwrite` and the modes `changes` and `manual` work as for
  `out.table`; the command is `mesh_out.write`. Copies from `geo.tex_instance` are read back
  from the GPU and written one by one (`instances` on `expand`). For copies that move every
  frame, use mode `manual`: in mode `changes` such an input is written every frame. With
  `instances` on `skip` each model is written once, without its copies. A file holds at most
  4 Mi triangles and 256 MiB; over that the node shows an error and writes nothing.
- **`script`**: Python that turns channels, tables and pictures into channels, a table, a
  picture and a mesh (see [Scripts](scripts.md)).
- **`out`**: sinks. `out.projector` shows a picture fullscreen on a display, or with `window`
  on in a window on the desktop.
- **`comp`**: components, nodes that hold nodes (see [Components](#components)).

Every type, with every param, is in the [node reference](reference/nodes.md). When each node
cooks is under [How the network runs](#how-the-network-runs).

## Params

Params are numbers, vectors, colours, choices, text, files or [palettes](#palettes). Number params have a slider and a
field: the slider covers the usual range, a typed value may go beyond it (the reference says
"soft" for such a range); a hard range clamps. Colours are linear and may go above 1. File
params are relative to the project directory and reload when the file changes.

- **Folded sections** group params that belong together (a render's `occlusion`, a mapper's
  `warp`); **"more"**, always the last, holds the rarely used ones. Only the main rows are
  shown at first; a section stays open for every node of that type once opened.
- **The circled "i"** before each param's name opens its help beside the panel: what the
  param does, what each value of a choice does, its type, default, range and unit, and whether
  a channel can drive it. A click elsewhere or Escape closes it; hovering the name shows the
  text alone. `node.kinds {kind, param}` returns the same help over the control channel.
- **A greyed-out param** does nothing with the node's other settings as they are: `key` of an
  `audio.instrument` with preset `kick`, `threshold` while nothing is wired into `play`. It
  stays editable, so a value can be set ahead. Hovering its name says why; `node.info` reports
  it as `applies: false` with the reason. A param greyed in italics is locked as well, until
  the setting that replaces it changes (a camera's `fov` while `calibration` is set).
- **A reset button** appears beside a param that is not at its default.
- **One drag is one undo step**, however long it takes.
- **Params from a file:** a shader's header and a script's `PARAMS` add their own rows, under
  "from the shader" or "from the script".
- **A stray value** (a param the node type no longer has, from an older project file) is
  listed with a remove button and a warning on the node; it does not stop the project from
  loading.

## Palettes

A palette param picks a set of colours by name, so a look is tried by switching the name
instead of editing colours one by one. `tex.ramp` and `tex.lookup` have one (a gradient, or a
picture recoloured through a colormap, in one node), so do the library's `breathe` and `pulse`,
and a shader declares its own (`// @param look palette = blood`, see
[Shaders](shaders.md#params)).

- **Palettes** are a handful of colours meant one by one (a colour per face): `sea`, `blood`,
  `candy`, `pumpkin`, `ember`, `neon`, `pastel`, `forest`, `primaries`,
  `mono`. **Colormaps** are ramps meant to be blended along: `grey`, `fire`, `ice`, `viridis`,
  `magma`, `inferno`, `plasma`, `cividis` and `twilight` (the last five from matplotlib).
  Either works anywhere; a node decides whether it takes the colours one by one or blends.
- **Your own:** type hex colours straight into the param, as colorhunt or coolors give them:
  `31aaa9 f8e0a4 a82020 6c1a1a`, `#31aaa9, #f8e0a4`, `31aaa9-f8e0a4-a82020`, a pasted
  colorhunt or coolors URL, or the digits run together. 6 digits per colour (sRGB, as every
  colour picker shows them), 8 with alpha, up to 32 colours. To give one a name, put it in
  `palettes.txt` in the project directory, one per line:

  ```
  # my palettes
  dusk: 2c3e50 fd746c ff8c52
  moss = #1b4332 #52b788
  ```

  or use `palette.set {name, colours}` (`palette.remove {name}` takes one away). Every node
  naming a palette picks up an edit of the file at once. A line that does not parse, a name
  given twice or a built-in name is skipped with a warning on the nodes that read the file.
- **In the param panel** the row shows the colours, the name or hex list as text, and a menu
  (`...`) with every palette and its colours.
- **From a channel:** bind the param like a number. The channel's value picks a palette by
  its place in the list `palette.list` returns (the built-in set in the order above, then
  the project's own), rounded and wrapping around: a counter or a MIDI knob steps through
  them, a `chan.sequence` step can switch the look of a piece.
- **Empty** means no palette; what that does is the node's: `tex.ramp` uses its `keys`,
  `tex.lookup` its `lut` input, `breathe` and `pulse` their colour params.

## Bindings

A param can follow a channel instead of its stored value: drag a wire from a `chan` output
onto the param's row, or use the link button at the end of the row, which also offers a
specific channel by name. A vector or colour binds per component: drop the wire on one field.

A bound row shows its name in the channel colour, the live value, and the source. The stored
value stays and comes back when the binding is removed; it is also used, with a warning, while
the channel is missing. Integers round, booleans are on from 0.5, hard ranges clamp. The editor
draws every binding as a dashed line into the top of the node, green from a channel. With "All
binding lines" unticked in the editor's right-click menu, a binding shows as a short stub on the
bound node, and as a line only while the source or the bound node is selected or under the
pointer.

**A binding into a loop.** A param may not follow a channel that depends on the param's own
node: `binding /loop.reset to /alive would make a loop`. To let a simulation restart when its
measurement says it has died, put a `chan.feedback` between. Wire the measurement into the
`chan.feedback`'s `previous` and bind the param to the `chan.feedback`. The param then follows the
measurement a frame later. The recipe is on the [chan.from_tex](reference/nodes.md#chanfrom_tex)
page.

### Text and choices

Text params and choices bind too, so a table or a `script.python` can change them during a
show. The link button lists the chan nodes and the table nodes; a table offers its rows and
then its cells. Dropping a chan wire on the row binds the whole param.

- **A table cell** is one cell of the source's first `table` output. Rows and columns count
  from 0, or go by header: a row by its first cell, a column by its cell in the first row.
  A text param takes the cell as it is. A number param reads it as a number. A choice takes
  the entry with that name, case and surrounding spaces aside.
- **A channel into a text** is written by a format: the placeholder `{}` stands for the
  number. `{}` gives up to three digits after the point and drops trailing zeros (`3`,
  `0.5`). `{.2}` gives exactly two, `{.0}` none. Text around it stays: `{.1} m`,
  `Round {}`, `{.0}%`. Write `{{` and `}}` for a brace. Without a format the binding uses
  `{}`.
- **A row per cue.** A cell binding names a fixed cell. For a value that changes with the cue,
  bind the `row` of a `chan.from_table` to a `chan.sequence`'s `step`: its channels then hold
  that row's numbers, which a format turns into text.
- **A row per entry of a choice.** Bind the `key` of a `chan.from_table` to the choice, for
  example `parent.look`. Its channels then hold the numbers of the row whose first cell is the
  entry's name, in the same frame. A param that only takes a number, such as `iterations`,
  binds to one of those channels. Lines to copy are on the
  [chan.from_table](reference/nodes.md#chanfrom_table) page.
- **A channel into a choice** picks the entry by its place: 0 is the first, rounded, held at
  the first and the last.
- **A `chan.dropdown` into a text or a choice** gives the name of its entry. A choice takes its
  own entry of that name, wherever that entry stands in its list. So one dropdown can drive the
  same choice in several nodes, shader params included, and the lists need not share an order.
  A number param bound to the dropdown gets the entry's place, 0 for the first. To pick a
  choice by place instead, name the channel: `node.bind {param, from, channel: "value"}`.
- **A text a choice does not have** leaves the choice at its last entry, and the node shows a
  warning that names the text.

File paths, node references and scripts cannot be bound. From the control channel:
`node.bind {param, from, cell: {row, col}}` for a cell and
`node.bind {param, from, format: "{.1} m"}` for a formatted channel. In `project.ron` such a
binding is written as `(node: "scores", cell: (row: 1, col: "name"))` or
`(node: "lfo1", format: "{.1} m")`. A plain channel binding keeps the short form `"lfo1:x"`. A
follower is written `"parent.preset"`, by command `node.bind {param: "/rd/step.preset", from:
"parent.preset"}`.

### Following a component's param

A param can follow a param that a component declares with an `@param` line. The follower takes
the value whole, in the same frame:

- **Numbers:** a bool, int or float goes into a bool, int or float. Ints round, a bool counts 0
  or 1. One field of a vector or colour takes a number too.
- **Vectors and colours** go whole into a vector of as many numbers. A colour takes a colour or
  three or four numbers.
- **Choices** go by name, so the two lists need not share an order. A name the follower lacks
  leaves it at its last entry, with a warning. A number picks the entry by place.
- **Text** takes a choice's name or a text, and writes a number with `{}`.

Other pairs are refused when you bind. In `project.ron` and by command the source is
`parent.preset` for the node's own component, or a path such as `/rd.preset` for another one.
`parent` reaches one level up: a component inside a component passes a value down by declaring
a param of its own that follows `parent.preset`. An `@expose` param cannot be followed; bind to
the inner param it shows instead.

- **Saved to the library, copied, renamed:** `parent` names whatever component the node sits in,
  so every copy follows its own.
- **Pasted into another component:** the node follows that component's param of the same name.
  When there is none, the paste leaves the binding out and says so.
- **Grouped (Ctrl+G):** the nodes go on following the old component, written as `/rd.preset`.
- **Dissolved:** each follower keeps the value it had. When the component's param was bound,
  the follower takes over that binding.
- **A param the component does not declare:** a command is refused and lists the component's
  params. A file still loads: the follower keeps its own value and warns, for example
  "follows parent.preset, but /rd has no param `preset`".

### Buttons, sliders and dropdowns on the node

`chan.button`, `chan.slider` and `chan.dropdown` are steered right on their node in the network
editor, without selecting it. **Button:** with `mode` `momentary` its channel is 1 while the mouse button is held
on it; with `mode` `toggle` each click switches it between 1 and 0. **Slider:** dragging on it
sets `value` between `min` and `max`. With Shift held, the slider moves a tenth of the mouse's
way, for fine steps; pressing or letting go of Shift mid-drag does not make it jump. One drag
is one undo step. **Dropdown:** a click opens the list of its `entries`, and picking one sets
`value` to that entry's name. One pick is one undo step. `entries` holds one entry per line.
The line above the control shows the channel's value: `1` or `0` for a
button, three decimals for a slider, the entry's place for a dropdown (0 for the first). A
toggle's state, a slider's value and a dropdown's `value` are params, saved with the project. A momentary press is no edit: it leaves no history
and works while Claude edits. Bind params to their channel like to any other. From the control
channel: `chan.press {path}` (or `{path, down}` to hold and let go) presses a button,
`node.set` moves a slider. `chan.pick {path, entry}` picks a dropdown's entry by its name
(`"maze"`) or its place (`2`); `node.set` on `value` picks by name too. A `value` that names
no entry leaves the dropdown at the entry it gave last, and the node shows a warning.
With `show` ticked, the control also appears on the show screen while
[show mode](getting-started.md#show-mode) is on. There it still works, but a change makes no
undo step and is saved only after show mode ends. A crash restart in show mode keeps it.

### Keys

`chan.keyboard_in` turns keys into channels: with `keys` at `1 2 3 4` (the default) it has
`key_1` to `key_4`, each 1 while its key is held, and `last`, the index in `keys` (from 0) of
the key pressed last, which stays until another listed key is pressed and starts at 0 when the
project opens. Bind a switch's `index` (or a `chan.constant` that several switches follow) to
`last`, and the number keys switch the show's pieces. Keys count only while a penumbra
window has the focus: the editor, the viewer's own window or a projector output (click it
once). A key typed into a text field and a key held with Ctrl, Alt or the Windows key do not
count. The number pad's digits are the digits; letters are named as the keyboard layout labels
them. **learn from OSC** in a param's link menu also takes a key a `chan.keyboard_in` lists,
and binds its gate. From the control channel: `keyboard.press {key}` (or `{key, down}` to hold
and let go) and `keyboard.status`.

### Mouse and 3D mouse

`chan.mouse_in` turns the mouse pointer into channels, wherever it is on the desktop, inside
penumbra or not: `x` and `y` are shares of a display, 0 to 1 from its bottom left (`y` is 1 at
the top); `inside` is 1
while the pointer is on that display; `left`, `right` and `middle` are 1 while held. Its
`display` names the display as an output's `display` does (or `\\.\DISPLAY2`); empty, it
is the display the pointer is on. On a named display `x` and `y` keep following the pointer
past 0 and 1 when it leaves. Name a projector's display, and the pointer over the projection
is a position in the projector's picture. `wheel` sums the notches turned anywhere (up
positive); headless runs have no wheel. The node reads the pointer once a frame, so a click shorter than
a frame can be missed. From the control channel: `mouse.status` (pointer, displays, wheel) and
`mouse.fake {x, y, buttons?}` (desktop pixels; `{real: true}` ends it; `{wheel}` turns the
wheel).

`chan.spacemouse_in` turns the 3D mouse (see [navigation](navigation.md)) into channels, each
-1 to 1: `right`, `up`, `forward` (slide, lift, push the cap away), `pitch`, `yaw`, `roll`
(tilt it nose up, twist it clockwise, tilt it to the right), `button_left` and `button_right`
(1 while held), `connected`. A viewer flying a free view and the node both follow the cap at
the same time. **learn from OSC** also takes the 3D mouse while such a node exists: press a
button, or push the cap clearly along one axis.

### MIDI controllers

`chan.midi_in` turns a MIDI controller into channels. Its `device` is a part of the device's
name, such as `easy control`; the … menu beside it lists the devices. Empty, it reads every
MIDI input. Each control you move becomes a channel, named after its MIDI channel (1 to 16)
and number:

- `cc_1_24`: control change 24 on channel 1, a fader, knob or button, 0 to 1.
- `note_1_36`: a key or pad, 1 while held. A tap shorter than a frame still shows for one
  frame. `note_1_36_vel` holds how hard it was hit, 0 to 1, and keeps it after the release.
- `bend_1` (-1 to 1), `pressure_1` (0 to 1), `program_1` (the number, 0 to 127).

A control appears only after it has moved once. **learn from OSC** in a param's link menu
takes MIDI too: move a fader, and the param is bound to it. A pad binds its `note_` channel.
The channels hold their last value when the device is unplugged. penumbra looks for the
device every second, so a device plugged in again goes on by itself. Many controllers send
7-bit steps, which can show on a slow fade; a `chan.filter` after the node smooths them.
`pairs` reads 14-bit controls (control changes 32 to 63 refining 0 to 31). Leave it off for a
controller that uses 32 to 63 as controls of their own, as most do.

MIDI clock from a drum machine or another program becomes a tempo on the same node, with the
channels `chan.beat_analysis` has: `bpm`, `beat` (0 at a beat, rising to 1), `tick` (1 for a
frame on each beat), `bar`, `bar_beat`. `beats` counts beats since the start, `playing` is 1
between start and stop, `clock` is 1 while clocks arrive. Bind to these instead of the audio
analysis, and the show follows the device's tempo exactly.

`out.midi` sends its input's channels to a device, named as `chan.midi_in` names them:
`cc_1_24` sends control change 24, `note_1_36` a note on while above 0 (its value the
velocity, or the value of `note_1_36_vel` when that channel comes too) and a note off at 0. Without a MIDI channel in the name (`cc_24`, `note_36`) the
node's `channel` is used. This lights pads and moves motor faders on controllers that take it,
or drives another program. It sends only what changed, and everything again when the device
is opened anew.

From the control channel: `midi.status` (the devices, what is open, recent messages, the
clock's tempo), `midi.inject {device, bytes}` (bytes as if a device had sent them, to try a
project without the controller), `midi.send {device, bytes}` and `midi.config {enabled}`.

Under Windows a device may be open in one program at a time (unless Windows MIDI Services
is installed). If another program holds it, the node says so and penumbra tries again every
second.

### A control page in the browser

`chan.http_in` makes a web page a control surface: a phone or tablet on the same network, or
another program. **The built-in page:** add a `chan.http_in` and open `http://localhost:8090/`.
The page shows every `chan.slider`, `chan.button` and `chan.dropdown` with `show` ticked, in
the order of the show screen. Sliders drag, toggles switch, dropdowns offer their entries,
momentary buttons stay pressed while a finger is on them, several at once. All open pages and the show screen show the same values. A control that
is bound or locked is greyed out. The node's `page` param switches the built-in page off.
**Your own page:** the node serves the files in its `folder` (`www` in the project), so
`www/index.html` replaces the built-in page. With `bind` at `lan`,
other devices reach it at `http://<this computer's address>:8090/`; `http.status` and
[Helpers > HTTP](helpers.md#http) name the address. **Messages:** a JSON object of addresses and values, sent over a WebSocket at `/ws`,
as `POST /set`, or as `GET /set?fader=0.5`. `{"/fader": 0.5}` makes channel `fader`; an array
of numbers makes one channel per number (`xy_0`, `xy_1`); an array of arrays or of objects is a
table on the `table` output. Bind params to the channels as for OSC. **Show controls:** with
`params` at `controls` (the default), `{"/fade/value": 0.3}` moves the `chan.slider` `/fade`
flagged `show`, and `{"/look/value": "maze"}` picks an entry of the `chan.dropdown` `/look`. **Buttons:** `{"press": {"/go": true}}` holds the momentary button `/go`,
`{"press": {"/go": false}}` lets go. A page sends the press again every 250 ms while the finger
is down. penumbra lets go of a press that is not renewed for a second, and of every press of a
WebSocket that closes. A repeated press while held is no new press, so a held GO counts once.
Over `POST /set` or `http.send`, a press is a tap of one frame. **What comes back:** a WebSocket gets
`{"in": {...}, "chans": {...}, "controls": [...]}` whenever something changed, at most `rate`
times a second: what the node received, the channels at its input, and the show controls with
their values. A page from another site or a file cannot send. From the control channel:
`http.send {message}` sends as a page does, `http.status` lists clients and drops.

```html
<input id="f" type="range" min="0" max="1" step="0.01">
<pre id="back"></pre>
<script>
  const ws = new WebSocket("ws://" + location.host + "/ws");
  document.getElementById("f").oninput = (e) =>
    ws.send(JSON.stringify({ "/fader": Number(e.target.value) }));
  ws.onmessage = (e) =>
    (document.getElementById("back").textContent = JSON.stringify(JSON.parse(e.data).in));
</script>
```

The [control-page](../examples/control-page/README.md) example has a page to copy: its
`www/index.html` draws a slider, a switch or a button for every show control of the project it
is in.

### A clock per piece

`chan.clock` counts the seconds since it was switched on. Its `run` is on from 0.5 up; each
time it turns on, the clock starts over from 0, and while it is off the clock stands at 0. With
`equals` set, `run` is on while it equals that number instead: bind `run` to the channel that
says which piece plays (`last` of a `chan.keyboard_in`, or the `chan.constant` bound to it) and
set `equals` to the piece's number, and the piece has a clock that starts with it, with one
binding. `mode` says what happens after `length` seconds: `once` holds at the end, `loop` starts
over, `free` runs on. Its channels are `seconds`, `fraction` (of `length`) and `done` (1 once a
full length has passed). A seek back starts every clock over. To compare channels elsewhere,
`chan.math` has `op` `equal`: 1 where a equals b, else 0.

`run` is the only way a clock starts over: a clock left at `run` 1 runs on while its piece is
not shown. A clock cooks in every frame, shown or not, so it sees its `run` turn off and on
again even while a `tex.switch` or `tex.sequence` skips the piece it drives, and it behaves the
same with the editor's thumbnails on or off and headless.

### Shows

A show plays pieces one after another, with cuts, cross-fades, dips to black and flashes
between them. Two kinds of node make it:

- **`chan.sequence`** holds the steps and the show's clock. Its `steps` are one line per step:
  piece, seconds, the transition into the next step and its length.

  ```
  # piece    seconds  transition  length
  intro      21
  faces      10       fade        2
  pumpkin    10       black       1.5
  tiles      12       flash       0.6
  water      15
  ```

  `cut` (or nothing) cuts at the end of the step; `fade` cross-fades into the next piece over
  the step's last seconds; `black` fades this piece out to black over its last seconds and the
  next one cuts in; `flash` cuts to white at the end, and the white fades into the next piece
  over its first seconds. A step's seconds are its whole slot, its transitions included. Before
  the first step there are `lead_in` seconds of black, after the last the show ends in black (the
  last step's transition leads into it). Instead of `steps`, `file` may name a file with such
  lines or with JSON: `{"lead_in": 1, "steps": [{"piece": "faces", "duration": 10,
  "transition": "fade", "length": 2}, ...]}`; saving the file applies it.

  `names` lists the pieces in a fixed order (`intro faces pumpkin tiles water show`): piece k
  is input `in<k>` of a `tex.sequence` and gets a channel `run_<name>`. Steps naming a piece not
  in the list are left out with a warning. Left empty, the pieces are numbered as the steps first
  name them.

  **Cues by hand.** `hold` instead of seconds keeps a step on screen until `go` rises past 0.5;
  then its transition plays: `faces hold fade 2`. In a timed step, `go` ends the step early. A
  `go` during a transition does nothing, so a double press skips no cue. Bind `go` to a key's
  gate or to an OSC trigger: the [show](../examples/show/README.md) example steps five cues with
  the space key, a button on the show screen and OSC.

  **GO from a desk.** List the desk's GO address under `triggers` on the `chan.osc_in`
  (`/go`), and bind `go` to its channel. Each message is then one press: `/go 1` or `/go`
  alone, as QLab sends it. `/go 0` is a release and not a press. The channel is 1 for one
  frame per press, so a desk that never sends 0 moves the show on each time. A sender that does
  send `/go 0` counts its next `/go 1` only after a `/go 0`. A button held while it is sent again
  (an `out.osc` resends every channel once a second) is then one press. Two presses within
  one frame come out one after the other.

  **After a crash in show mode** the cue list goes on at the cue that was playing. A holding cue
  still holds, and the cues let go before stay let go. A timed cue may repeat up to a second.
  When the steps changed meanwhile, the show starts from its beginning.

  `run` (with `equals`, as for a clock) starts the show from its beginning each time it turns
  on; while it is off the show is black. `mode` `loop` plays it round after round. Its channels:
  `seconds`, `step` (counted from 0; -1 in the lead-in), `piece`, `from`, `to` and `mix` (the
  entries shown, a piece number, -1 for black or -2 for white, and the share of `to`), `done`,
  `length`, and `run_<name>` per piece, 1 while that piece is shown, also during its fades. Bind a
  piece's clock's `run` to its `run_<name>` and the piece starts from its beginning each time
  the show brings it in (a piece listed twice in a row runs on).

- **`tex.sequence`** makes the picture: wire the `chan.sequence` into its `sequence` input and
  each piece's picture into `in<k>`. It passes the piece shown on as it is and blends two during
  a transition; only the one or two inputs shown cook, the others cost nothing. Black and white
  are made by the node (at the size of the picture they blend with, else `width` x `height`),
  unless `black` or `white` are wired: a projector's output can take its own black and a white
  limited to what it reaches. An input not wired shows black. For pictures per projector, use
  one `tex.sequence` per output, all wired to the same `chan.sequence`.

### Video clips on cue

A `tex.video_file` starts its clip over from `start` in three ways:

- **`restart`** turning on. Bind it to a channel: each time the channel crosses 0.5 upwards,
  the clip starts over. A channel that stays at 1 does not restart again.
- **`restart_on_show`** on: the clip starts over each time it comes back into use after at
  least half a second out of it. A `tex.switch` with `inactive: skip` or a `tex.sequence`
  picking the clip counts. A switch with `inactive: cook` keeps every input in use, so its
  clips never come back. The editor's thumbnails do not count as use.
- **The `restart` button** in the param panel, or the command `video_file.restart {path}`.

The clip shows its first frame at once when the player holds it decoded: always for a clip
with `loop` off, and for a looping clip whose `start` is 0. Otherwise the first frame takes a
seek, a few frames on H.264, none on HAP. With `play` off, the clip jumps to `start` and holds
there.

**A cue from QLab or a lighting desk.** Add a `chan.osc_in` and send it `/scene <number>` from
the desk. When the desk is another machine, turn on the node's `lan`. Bind a `tex.switch`'s
`index` to the `scene` channel, set `inactive: skip` and turn on `restart_on_show` on each
clip. A GO then cuts to the clip's scene and the clip plays from its
start. For a fade, turn on the switch's `blend` and bind `index` through a `chan.filter`; the
incoming clip starts with the fade. To start the clip on screen over, send `/restart 1` and then
`/restart 0`, and bind the clip's `restart` to the `restart` channel. In a `chan.sequence`
show, bind `restart` to the piece's `run_<name>`, so a clip shown in two steps in a row starts
over in each.

A saved component could hold the same nodes, but the show itself (the steps, the row of
entries, the gates) is the native node; a component around a `chan.sequence` and its
`tex.sequence` nodes is only worth it to reuse a whole show setup.

### Scrubbing a clip

Bind a channel to `position` on a `tex.video_file`, and the channel says where the clip is, in
seconds. `play`, `speed` and restarts are ignored while it is bound. So a `chan.slider` moves
the clip by hand, an envelope pushes it forward on each hit, and a `chan.lfo` swings it back
and forth. A channel that stands still freezes the clip. A looping clip wraps: 5 s in a clip of
3.6 s shows 1.4 s of the next pass. Unbind `position`, and the clip plays on from where it
stands. The same channel on an `audio.file` with the same file scrubs its sound in step (see
[Playing sound files](audio.md#playing-sound-files)).

While scrubbed, the clip keeps up to 256 MB of decoded frames around the position: the whole
clip when it is short, about 85 frames of 1920 x 1080 (1.4 s at 60 fps), about 20 frames of 4K.
Moving back and forth within them shows each frame at once. A jump beyond them waits for the
decoder: about a tenth of a second for 1080p H.264, about half a second for 4K. The last frame
stays up meanwhile. HAP clips jump fastest, since each of
their frames stands alone.

**Jumps.** A change of `start` jumps there at once, and so does `position` set by hand. The
command `video_file.seek {path, seconds}` jumps without an edit. A looping clip stays in its
pass, so `loops` keeps counting.

## Paths

Every node has a path: `/noise1` at the top level, `/scene/noise1` inside the component
`scene`. A param is addressed as `/scene/noise1.speed`, one component of a vector as
`/t.translate.0`.

Where a path refers to another node (a wire's source in the project file, a binding, the
`node` param of `tex.select`), it is read from the level of the node it belongs to: `/a/b` is
absolute, `x` is the node `x` beside it, `sub/x` is below that, `../x` one level up. So a
component's inside refers to its own nodes by name and keeps working when the component is
moved or saved to a file.

## Components

A `comp.base` holds nodes. Its input and output sockets are port nodes inside it, one pair per
data kind: `tex.in` and `tex.out`, `chan.in` and `chan.out`, and so on for `table`, `geo`,
`mat`, `light`, `view` and `vol` ([Component ports](reference/nodes.md#component-ports)). What is
wired to the component outside comes out of its `tex.in` inside. The port nodes read or write
no files or devices: that is the job of the `out` nodes and the sources. Wires cross a component's edge only through ports; bindings and node references may
reach anywhere.

Commands name a component's sockets by the names of its port nodes. `node.connect` wires them,
and `chan.read`, `table.read`, `geo.info` and `node.snapshot` read them, with `output` set to
the port node's name (`/room` with `output: "colours"`). Without `output`, a read takes the
first output of the kind it reads.

- **Group:** select nodes and press Ctrl+G. Ports are added where wires cross.
- **Enter and leave:** I or a double-click goes in, U or the breadcrumb at the top left goes
  up. Wires from outside the level shown end in a stub labelled with where they go.
- **Dissolve:** the node menu moves the nodes back up.
- **Params of its own, by menu:** inside a component, the link button of a param opens a
  section named after the component. "Put on component" gives the component a param of that
  name, kind, range and value, and the inner param follows it. "Show on component" shows the
  inner param as a row of the component; the value stays inside. "follow preset" makes the
  param follow the component's `preset`. Each is one undo step (`comp.put_param`,
  `comp.show_param`, `node.bind`).
- **Params of its own, by text:** the component's `params` text ("edit params" under its rows)
  declares them, one per line. TouchDesigner calls these custom parameters. `@param preset choice(coral|maze) = coral
  -- the pattern` makes a param that the component holds, in the grammar of a shader header
  (the `//` may stay). It has a default, a range and a doc, and `node.set` and `node.bind` treat
  it like any param: a value it does not take is refused at once. `@group look` puts the params
  after it into a fold.
- **Inner nodes follow it:** bind an inner param to `parent.preset`, the param `preset` of the
  component the node sits in. TouchDesigner writes this `parent().par.Preset`. Every follower
  takes the component's value in the same frame, also when that param is bound itself. More
  under [Following a component's param](#following-a-components-param).
- **Exposed params:** an `@expose speed = noise1.speed` line (or just `@expose noise1.speed`)
  in the same text shows an inner param on the component. Setting it sets the inner param,
  which keeps the value and the range. Files from before this text existed listed these lines
  in a param `expose`; they load into `params`.
- **The component's rows** follow the order of the text. At the right end of each row, "used
  by step" names who follows it (a click lists them and goes there). A param nothing follows
  says its job instead: "writes 12" for a choice with presets, "restart rule" for a
  bool an `@reset` line asks, "restarts" for a name on an `@reset` line. "unused" marks a param
  with none of these: it changes nothing. A shown row names the inner param it sets
  (`step.feed`). A follower inside shows `parent.preset` in the components' turquoise; a click
  goes to the component. When a follower cannot take the value, the component lists the warning
  at the top of its panel and its node shows a warning badge.
- **Starting over:** a component's `reset` starts every node inside that remembers earlier
  frames over (feedback loops, simulations, trails, clocks), in each frame while it is on. Bind
  it to a key, such as a `chan.keyboard_in`'s `key_0`, and the key restarts the simulation;
  TouchDesigner users know this as a pulse parameter. A line `@reset preset seed` in the text
  starts the component over once when one of these params changes, by hand, by undo or through
  a binding. The new start uses the new value in the same frame. `@reset preset unless morph`
  counts only while the bool `morph` is off. The `reset` button in the
  panel's header restarts once. More in [comp.base](reference/nodes.md#compbase).
- **Presets:** `@preset preset=maze feed=0.029 kill=0.057` lines give the values that choosing
  an entry of a choice writes into the params. A `preset=*` line lists the params the presets
  write and the value every entry starts from. Choosing writes in the same edit, so one undo
  takes it back. Afterwards the values are ordinary values: tune them, and they are saved.
  While some differ from the entry, the choice's row says so (`feed, kill changed · back`), and
  "back" writes the entry's values again. Opening a project writes nothing. A choice bound to a
  key or a dropdown writes the entry and its values one frame after the binding changes,
  without an undo step. More in [comp.base](reference/nodes.md#compbase).
- **Mistakes in the text:** a line that declares nothing shows right below the text, with its
  line number and the reason. The other lines still work. A name may be declared once, and not as
  one of the component's own built-in params (`params`, `shelf`, `about`, `reset`).
- **Rename:** renaming a node (the name field at the top of the param panel, or `node.rename`)
  also rewrites the `@expose` lines and node references (`tex.select`'s `node`) that name it or a
  node inside it. One undo takes all of it back. Wires and bindings follow a rename by
  themselves. Params that name a node in plain text, such as `tex.scan`'s `projector`, keep the
  old name: fix them by hand.
- **Library:** "Save to the library" in the node menu writes a `<name>.comp.ron` file. The add
  dialog's `library` tab lists the project's `components/` folder, the library shared by all
  projects (`%LOCALAPPDATA%\penumbra\components`, or `PENUMBRA_LIBRARY`) and the content
  library that ships with penumbra (marked `penumbra`; the `library` folder beside the app,
  catalogued with a preview of each in [its README](../library/README.md)): effects for scanned
  objects such as water in a crate, a lamp going round a pumpkin, contour lines or a carved
  face. The tab shows one shelf at a time. A component names its shelf in its `shelf` param,
  in one or two levels such as `dmx` or `dmx/safety`. A new name makes a new shelf. Its
  `about` param says in one line what it does: the foot of the tab shows that line under the
  pointer, with `preview.png` from the component's folder if there is one. Typing searches all
  shelves, in the names, the shelves and the `about` lines. The shelf `mine` holds all your own
  components and the project's, also those that have a shelf of their own. A shelf whose
  first level is `mapping` or `simulations` puts the component into the add dialog's tab of
  that name instead, beside the node types there (`mapping/warps` goes there too): it is then
  on no shelf of the `library` tab, not even `mine`. Their place is at
  the right of the entry. Adding one adds a copy. The cross beside a user's library entry moves its file into a
  `removed/` folder beside it; penumbra's own cannot be removed.

Files a component refers to (a shader, an image) are relative to the project. A component in a
folder of its own, `<name>/<name>.comp.ron` (as all of penumbra's are), brings the other files
of that folder along: adding it copies them into the project's `library/<name>/`, which is where
its nodes name them (`library/water/water.wgsl`). A file the project has already is kept, so a
changed copy survives adding the component again; `comp.load` says what it `copied` and
`kept`. Its preview picture and notes stay behind. Saving a component does not collect its
files yet.

A wire or a binding in a component file that would close a loop costs only itself, as in
`project.ron`. The component is added with every node and every other wire. The status bar,
the log and the reply of `comp.load` (`left_out`, `file`) name the file and walk the loop. The
file keeps the wire. Saving the component over that file first copies it to
`<name>.comp.ron.bak` beside it, and the reply of `comp.save` names the copy (`backup`).

## Undo and the change log

Every edit, by hand, by Claude over the control channel, or by editing `project.ron` in a text
editor, is an entry in one linear history, shown under Changes. Ctrl+Z undoes the latest,
whoever made it; to take back one of Claude's changes while keeping later tweaks of your own,
ask Claude to revert it. Moving nodes is an edit like any other: a drag (or one arrange) is one
entry, and `history.undo` takes it back as well.

Values that arrive by OSC are live control, not edits: they stay out of the history, and undo
or redo of other edits does not roll them back. An edit by hand to the same param takes it
back into the history.

## Who is editing

You and Claude take turns. While Claude edits, the whole UI turns dark blue, the title says
"Claude is editing" with what it is doing, and editing widgets are disabled; looking,
selecting and pinning the viewer still work (moving nodes is an edit, so it waits too). Claude gets the turn only after
3 seconds without an edit of yours. "Take over" in the blue bar ends Claude's turn at once,
and the turn lapses by itself after 2 minutes without a command. Single commands without a
claim turn the UI blue too, for 5 seconds after the last one; there is no bar then, and an
edit of yours ends that turn right away.

## What the editor shows

| | |
|---|---|
| header colour | the family (components: turquoise with a double border) |
| red tint over the body | cook time: from about 2 % of a frame to full red at half a frame |
| white outline | selected |
| blue outline, fading | changed by Claude or the project file in the last seconds; also the nodes of the change under the pointer |
| badge | an error or a warning; the message is in the param panel |
| red badge and outline on a component | a node inside it is in error, at any depth; the tooltip names it |
| padlock | locked |
| "not cooked" | nothing needed the node since it last changed, so there is no current picture |
| dashed line | a binding (green), a node reference, or a wire into a feedback loop's `previous` |

**Errors keep the last good result.** A node that fails (a shader that does not compile, a
file that does not load) shows the error and keeps putting out what it made last, so a typo
during a show does not black out a projector.

**Thumbnails** follow the editor's options (right-click on empty canvas): always live, only
when free (nothing is cooked just for a thumbnail), or never. A live thumbnail cooks every
frame by default; the same menu lowers that to every 2nd, 3rd or 6th frame for large networks.
Both are project settings (`thumbnails` and `thumbnail_every` in `project.ron`), so a change is
undoable and travels with the project; `cook.thumbnails` over the control channel sets the same.
A picture is only shown while it is up to date; an outdated one would be misleading. "Pause
thumbnail" in a node's menu does the same for one node.

**Locking** a node (node menu, or the lock in the param panel) holds its current outputs: it no
longer cooks, whatever happens upstream, and its inputs may even be removed. Locked outputs are
kept across restarts. Use it for calibration results and anything expensive to make again.

**Notes** are comments on the canvas: a sand-coloured box of text, added as `ui.note` from the
add dialog's `system` tab. Its `text` param holds what it says. Double-click a note to write in
it on the canvas; Escape or a click outside ends writing, as one undo step. The param panel
edits the same text. A line starting with `# ` is a heading, one starting with `- ` a bullet,
and a name in backticks stands out. Drag a note to move it; drag its lower right corner to size
it (`width` and `height`). A note has no ports and changes nothing in the show. Wires run
across notes. **Arranging** (L) takes each note along with the nodes it lies on, or else with
its nearest node, at the same offset. A note that would then cover a node or another note moves
to the nearest free place. The demo and the examples use notes to explain their networks.

## The project directory

| | |
|---|---|
| `project.ron` | the network: nodes, params, wires, bindings, where each node sits in the editor, project settings. Written a second after each change |
| `layout.ron` | view state: pan and zoom, open sections, the right column's width, the viewer's aspect, the editor's other options |
| `state/` | what nodes hold across restarts (feedback loops, caches, locked outputs); ignored by git through its own `.gitignore` |
| `components/` | the project's own component files |
| `library/` | the files of components added from a library (`library/<name>/`) |
| `palettes.txt` | the project's own [palettes](#palettes), one per line |
| `project.ron.bak` | the file as it was before penumbra wrote over a file that did not load or held a wire that would close a loop; worth a line in a project's `.gitignore` |
| anything else | shaders, scripts, images, videos, meshes, referenced by relative path |

`project.ron` is plain text, meant to be read and edited. The app notices a change to it and
merges only what differs, as one undoable entry, keeping your unsaved work. A project keeps
working under git: one node per line, sorted. A node's `pos` is where it sits in the editor;
without one the editor places it and the next save writes where. Projects from builds that
kept positions and the thumbnail mode in `layout.ron` (format version 1) take them from there
once as they open.

A node kind that was renamed in a later build still loads. A file naming `chan.speed` opens
with a `chan.integrator` in its place, and the next save writes the new name. `project.open`
and `project.info` (`renamed`) list the nodes this happened to. `node.create` and
`node.replace` take the old names too, with a note in the reply.

A node whose line does not load (a kind this build does not know, an input its kind does not
have, a param value its kind refuses) does not stop the rest: the project opens, and the node
stays as a placeholder (`node.broken`) that shows the reason as its error, in the bar at
the top of the window and in `project.info` (`broken`). The placeholder does nothing; wires from
it carry nothing. Saving writes its line back as it was, so nothing is lost by opening and
saving. It can be renamed or deleted; to change it, fix its line in `project.ron`, and the
fixed node takes its place.

A wire or a binding in the file that would close a loop does not stop the rest either. The
project opens with every node and without that one wire, and says which and why: in the bar at
the top of the window, in the log, and in `project.info` and the reply of `project.open`
(`left_out`). The message walks the loop round, input by input:

```
the connection from /fb to /tail (input `in`) would close a loop: /tail -> /seed (`in`) -> /fb (`initial`) -> /tail (`in`)
```

Of a loop's
wires, the one into the node whose path sorts last is left out; bindings load after all wires.
A feedback's `initial` input counts like any other: it is read in the same frame, only
`previous` waits a frame. In the editor and from commands such a wire is refused. A component
file loads the same way (see Components).

**Until the next save** the file keeps the wire. Opening and closing without an edit write
nothing. To get the wire back, take another wire of the loop out of `project.ron`: the edit
merges and the wire arrives with it.

**The next save** (autosave after an edit, `project.save`, closing after an edit) first copies
the file to `project.ron.bak`, then writes it without the wire. The bar at the top and
`project.info` keep listing the wire, and `left_out_backup` names the copy. The note stays
until the project is opened again, or until an edit of `project.ron` brings the wire back.

**An edit of `project.ron` while the project is open** that would close a loop does not merge.
The bar shows "project file not merged" with the loop, autosave pauses, and the file stays as
you typed it until the loop is gone.

A file that does not parse at all, or has a format version this build does not read, is reported
and left alone (autosave pauses) until it is fixed. `project.save` writes over it anyway; it first
copies the file to `project.ron.bak` (also when nodes are kept as placeholders; one copy, the next
such save replaces it). A file penumbra wrote itself in this session is never copied, so its
copy cannot replace a `project.ron.bak` that holds a left-out wire or your broken text. When
the copy cannot be made (`project.ron.bak` is read-only or locked), nothing is written: move the
old copy away or make it writable.
