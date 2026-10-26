# Concepts

## Nodes, families and data kinds

A project is a network of nodes. Each node has a kind, written `family.name` (`tex.noise`,
`chan.lfo`), inputs, outputs and params. An output connects only to an input of the same data
kind, and the wire takes that kind's colour:

| kind | colour | what flows | families that make it |
|---|---|---|---|
| `tex` | purple | 2D pictures on the GPU, linear light, premultiplied alpha, values above 1 allowed | `tex` |
| `chan` | green | named channels of numbers; the last sample is the value now | `chan`, `script` |
| `table` | blue | rows of text cells | `table`, `script` |
| `geo` | orange | meshes with parts, materials and transforms | `geo` |
| `mat` | pink | a material | `mat` |
| `light` | yellow | one or more lights | `light` |
| `view` | silver | one camera | `view` |

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
  look values up in, and `chan.from_tex` averages a picture down to a few pixels and turns them
  into channels (at 1 x 1, its mean colour). Channels named `r`, `g`, `b` (and `a`), as
  `chan.from_tex` makes them, come back from `tex.from_chan` as a colour.
- **`table`**: text tables, typed in or read from a file, for scripts and OSC.
  `table.file` reads a CSV, TSV or text file and reloads it when the file changes.
  `chan.from_table` turns a table of numbers into channels, a channel per column (or row), named
  by its first cell. A param bound to a channel follows the last value in that column (or row). `out.table`
  writes a table to a CSV or TSV file. With `file` empty it writes `export/<node name>.csv` in
  the project folder. In mode `changes` it writes whenever the table differs from what the file
  holds; in mode `manual` it writes only on `table_out.write`. `overwrite` on replaces the file
  in place, for a program that keeps reading it; off, every write gets a new name with `_2`,
  `_3`, ...
- **`geo`**, **`mat`**, **`light`**, **`view`**: a 3D scene. Geometry from files or
  primitives, transformed and merged; materials put on it with `geo.assign_material`; lights and a
  `view.camera` wired into a `tex.render`, which makes a picture of it all (a render needs its
  camera; the camera is perspective, orthographic or calibrated). `light.projector` throws a
  picture through a camera onto the geometry: a virtual projector, for trying a mapping on a
  model of the room. Coordinates are metres, z up.
  `out.mesh` writes geometry to an OBJ, STL or glb file, every model where it stands in the
  scene. Each part stays a part: an OBJ object or a glTF node. Only glb keeps material colours;
  textures are not written. File, `overwrite` and the modes `changes` and `manual` work as for
  `out.table`; the command is `mesh_out.write`. Copies from `geo.tex_instance` are read back
  from the GPU and written one by one (`instances` on `expand`). For copies that move every
  frame, use mode `manual`: in mode `changes` such an input is written every frame. With
  `instances` on `skip` each model is written once, without its copies. A file holds at most
  4 Mi triangles and 256 MiB; over that the node shows an error and writes nothing.
- **`script`**: Python that turns channels and tables into channels and a table (see
  [Scripts](scripts.md)).
- **`out`**: sinks. `out.projector` shows a picture fullscreen on a display.
- **`comp`**: components, nodes that hold nodes (see [Components](#components)).

Every type, with every param, is in the [node reference](reference/nodes.md). Nodes cook only
when something needs them (an output, the viewer, a visible thumbnail) and only when something
they depend on changed. A node may need only some of its inputs: `tex.switch` with
`inactive: skip` cooks only the chain it shows, and the others stand still (their state pauses,
their thumbnails cook only as thumbnails) until it switches to them; `tex.sequence` does the
same for a show. A few cheap nodes cook in every frame whether anything shows them or not
(`chan.clock`, `chan.sequence`; the reference says so), so they see every change of their
params.

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
draws every binding as a dashed green line into the top of the node. With "All binding lines"
unticked in the editor's right-click menu, a binding shows as a short stub on the bound node,
and as a line only while the source or the bound node is selected or under the pointer.

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
the desk. Bind a `tex.switch`'s `index` to the `scene` channel, set `inactive: skip` and turn on
`restart_on_show` on each clip. A GO then cuts to the clip's scene and the clip plays from its
start. For a fade, turn on the switch's `blend` and bind `index` through a `chan.filter`; the
incoming clip starts with the fade. To start the clip on screen over, send `/restart 1` and then
`/restart 0`, and bind the clip's `restart` to the `restart` channel. In a `chan.sequence`
show, bind `restart` to the piece's `run_<name>`, so a clip shown in two steps in a row starts
over in each.

A saved component could hold the same nodes, but the show itself (the steps, the row of
entries, the gates) is the native node; a component around a `chan.sequence` and its
`tex.sequence` nodes is only worth it to reuse a whole show setup.

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
`mat`, `light` and `view` ([Component ports](reference/nodes.md#component-ports)). What is
wired to the component outside comes out of its `tex.in` inside. The port nodes read or write
no files or devices: that is the job of the `out` nodes and the sources. Wires cross a component's edge only through ports; bindings and node references may
reach anywhere.

- **Group:** select nodes and press Ctrl+G. Ports are added where wires cross.
- **Enter and leave:** I or a double-click goes in, U or the breadcrumb at the top left goes
  up. Wires from outside the level shown end in a stub labelled with where they go.
- **Dissolve:** the node menu moves the nodes back up.
- **Expose params:** the component's `expose` param lists inner params, one per line,
  `speed = noise1.speed` or just `noise1.speed`. They show on the component and act on the
  inner param.
- **Rename:** renaming a node (the name field at the top of the param panel, or `node.rename`)
  also rewrites the `expose` lines and node references (`tex.select`'s `node`) that name it or a
  node inside it. One undo takes all of it back. Wires and bindings follow a rename by
  themselves. Params that name a node in plain text, such as `tex.scan`'s `projector`, keep the
  old name: fix them by hand.
- **Library:** "Save to the library" in the node menu writes a `<name>.comp.ron` file. The add
  dialog's `library` tab lists the project's `components/` folder, the library shared by all
  projects (`%LOCALAPPDATA%\penumbra\components`, or `PENUMBRA_LIBRARY`) and the content
  library that ships with penumbra (marked `penumbra`; the `library` folder beside the app,
  catalogued with a preview of each in [its README](../library/README.md)): effects for scanned
  objects such as water in a crate, a lamp going round a pumpkin, contour lines or a carved
  face. All of them stand in one list by name; the place each comes from is at the right of
  its entry. Adding one adds a copy. The cross beside a user's library entry moves its file into a
  `removed/` folder beside it; penumbra's own cannot be removed.

Files a component refers to (a shader, an image) are relative to the project. A component in a
folder of its own, `<name>/<name>.comp.ron` (as all of penumbra's are), brings the other files
of that folder along: adding it copies them into the project's `library/<name>/`, which is where
its nodes name them (`library/water/water.wgsl`). A file the project has already is kept, so a
changed copy survives adding the component again; `comp.load` says what it `copied` and
`kept`. Its preview picture and notes stay behind. Saving a component does not collect its
files yet.

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

## The project directory

| | |
|---|---|
| `project.ron` | the network: nodes, params, wires, bindings, where each node sits in the editor, project settings. Written a second after each change |
| `layout.ron` | view state: pan and zoom, open sections, the right column's width, the viewer's aspect, the editor's other options |
| `state/` | what nodes hold across restarts (feedback loops, caches, locked outputs); ignored by git through its own `.gitignore` |
| `components/` | the project's own component files |
| `library/` | the files of components added from a library (`library/<name>/`) |
| `palettes.txt` | the project's own [palettes](#palettes), one per line |
| `project.ron.bak` | the file as it was before `project.save` wrote over a file that did not load; worth a line in a project's `.gitignore` |
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

A file that does not parse at all, or has a format version this build does not read, is reported
and left alone (autosave pauses) until it is fixed. `project.save` writes over it anyway; it first
copies the file to `project.ron.bak` (also when nodes are kept as placeholders; one copy, the next
such save replaces it).
