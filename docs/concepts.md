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

The families:

- **`tex`**: pictures. Generators (noise, constant, text, image, video, a PSD layer), filters
  (level, blur, transform, ...), compositing, a shader node for anything else, `tex.mapper`
  for projection mapping, `tex.render` for 3D.
- **`chan`**: numbers that change over time. Generators (`lfo`, `noise`, `constant`), `math`,
  `filter`, `trail` (the last seconds of a channel as samples, to plot or to feed on), and OSC
  in and out. Channels drive params through [bindings](#bindings).
  A channel holds one or many samples, so a set of channels is a small grid of numbers:
  `tex.from_chan` turns it into a picture (a row per channel, a pixel per sample) for shaders to
  look values up in, and `chan.from_tex` averages a picture down to a few pixels and turns them
  into channels (at 1 x 1, its mean colour). Channels named `r`, `g`, `b` (and `a`), as
  `chan.from_tex` makes them, come back from `tex.from_chan` as a colour.
- **`table`**: text tables, typed in or read from a file, for scripts and OSC.
- **`geo`**, **`mat`**, **`light`**, **`view`**: a 3D scene. Geometry from files or
  primitives, transformed and merged; materials put on it with `geo.assign_material`; lights and a
  `view.camera` wired into a `tex.render`, which makes a picture of it all (a render needs its
  camera; the camera is perspective, orthographic or calibrated). `light.projector` throws a
  picture through a camera onto the geometry: a virtual projector, for trying a mapping on a
  model of the room. Coordinates are metres, z up.
- **`script`**: Python that turns channels and tables into channels and a table (see
  [Scripts](scripts.md)).
- **`out`**: sinks. `out.projector` shows a picture fullscreen on a display.
- **`comp`**: components, nodes that hold nodes (see [Components](#components)).

Every type, with every param, is in the [node reference](reference/nodes.md). Nodes cook only
when something needs them (an output, the viewer, a visible thumbnail) and only when something
they depend on changed. A node may need only some of its inputs: `tex.switch` with
`inactive: skip` cooks only the chain it shows, and the others stand still (their state pauses,
their thumbnails cook only as thumbnails) until it switches to them.

## Params

Params are numbers, vectors, colours, choices, text or files. Number params have a slider and a
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

A `comp.base` holds nodes. Its inputs and outputs are port nodes inside it (`tex.in`,
`tex.out`, `chan.in`, ...): what is wired to the component outside comes out of its `tex.in`
inside. Wires cross a component's edge only through ports; bindings and node references may
reach anywhere.

- **Group:** select nodes and press Ctrl+G. Ports are added where wires cross.
- **Enter and leave:** I or a double-click goes in, U or the breadcrumb at the top left goes
  up. Wires from outside the level shown end in a stub labelled with where they go.
- **Dissolve:** the node menu moves the nodes back up.
- **Expose params:** the component's `expose` param lists inner params, one per line,
  `speed = noise1.speed` or just `noise1.speed`. They show on the component and act on the
  inner param.
- **Library:** "Save to the library" in the node menu writes a `<name>.comp.ron` file; the add
  dialog's `comp` tab lists the project's `components/` folder and the library shared by all
  projects (`%LOCALAPPDATA%\penumbra\components`, or `PENUMBRA_LIBRARY`). Adding one adds a
  copy. The cross beside a library entry moves its file into a `removed/` folder beside it.

Files a component refers to (a shader, an image) stay relative to the project, so they do not
travel with a saved component yet.

## Undo and the change log

Every edit, by hand, by Claude over the control channel, or by editing `project.ron` in a text
editor, is an entry in one linear history, shown under Changes. Ctrl+Z undoes the latest,
whoever made it; to take back one of Claude's changes while keeping later tweaks of your own,
ask Claude to revert it. Moving nodes is undoable as well, in the same sequence.

Values that arrive by OSC are live control, not edits: they stay out of the history, and undo
or redo of other edits does not roll them back. An edit by hand to the same param takes it
back into the history.

## Who is editing

You and Claude take turns. While Claude edits, the whole UI turns dark blue, the title says
"Claude is editing" with what it is doing, and editing widgets are disabled; looking,
selecting, moving nodes and pinning the viewer still work. Claude gets the turn only after
3 seconds without an edit of yours. "Take over" in the blue bar ends Claude's turn at once,
and it lapses by itself after 2 minutes without a command from Claude.

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
Both choices are kept with the project (`cook.thumbnails` over the control channel changes them
for the run only). A
picture is only shown while it is up to date; an outdated one would be misleading. "Pause
thumbnail" in a node's menu does the same for one node.

**Locking** a node (node menu, or the lock in the param panel) holds its current outputs: it no
longer cooks, whatever happens upstream, and its inputs may even be removed. Locked outputs are
kept across restarts. Use it for calibration results and anything expensive to make again.

## The project directory

| | |
|---|---|
| `project.ron` | the network: nodes, params, wires, bindings, project settings. Written a second after each change |
| `layout.ron` | where the nodes sit, pan and zoom, open sections, the right column's width, editor options (thumbnails included) |
| `state/` | what nodes hold across restarts (feedback loops, caches, locked outputs); ignored by git through its own `.gitignore` |
| `components/` | the project's own component files |
| anything else | shaders, scripts, images, videos, meshes, referenced by relative path |

`project.ron` is plain text, meant to be read and edited. The app notices a change to it and
merges only what differs, as one undoable entry, keeping your unsaved work. A file that does not
parse is reported and left alone (autosave pauses) until it is fixed. A project keeps working
under git: one node per line, sorted.
