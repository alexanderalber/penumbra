# Architecture

How penumbra is put together, for someone who wants to read or change the code. What the
program does for its users is in [the documentation](docs/README.md).

## One sentence

A project is a network of nodes in text files; the engine cooks the network once per frame on
its own thread, outputs present the results on the projectors, and everything anyone can do to
it is a command that the editor, the command line and the control channel all call alike.

## Crates

A Cargo workspace of 23 library crates, two apps and `xtask`. From the bottom up:

| Crate | What it holds |
|---|---|
| `core` | Time (integer ticks), ids, names and paths, values, parameter metadata, palettes, errors. |
| `graph` | The network, its edits and history, the cook scheduler with dirty tracking, asynchronous jobs, the `Operator` trait. |
| `project` | The project as text: RON format, load and save, diff and merge, autosave, editing modes. |
| `calib`, `track`, `mesh`, `audio`, `osc`, `dmx`, `camera`, `video`, `record`, `script`, `pyhost`, `spacenav`, `system` | One concern each, pure where possible: calibration math, blob tracking, mesh readers, audio input and analysis, OSC, DMX, camera input, video decoding, movie encoding, the Python host, the SpaceNavigator, the show machine's checks. |
| `gpu` | The wgpu device, textures, fullscreen shader passes, readback, image files. |
| `ops` | Every built-in node type: `chan` and `table` nodes on the CPU, `tex` nodes as WGSL passes, `geo`, `mat`, `light` and `view` for 3D, the outputs, the script node. |
| `spout`, `ndi` | Texture sharing with other programs, on the GPU. |
| `engine` | The command registry, the control channel, and the engine that runs an open project: the cook thread and the project thread. |
| `output` | Projector outputs: fullscreen windows on their displays, presented from their own threads. |
| `ui` | The editor (egui): node editor, params, viewer, change log, `ui.*` commands, headless runs. |
| `apps/penumbra` | The app: editor and outputs. |
| `apps/penumbra-cli` | Control channel client, MCP server, soak test. |

### Layers

Lower crates do not know about the GPU or the UI, and `cargo xtask layers` fails when one
starts to:

- `core`, `graph`, `project` and the one-concern crates: no wgpu, no winit, no egui.
- `gpu`, `ops`, `engine`, `spout`, `ndi`: wgpu, no egui.
- `output`: wgpu and winit, no egui.
- `ui` and the apps: everything.

The table is `LAYERS` in `xtask/src/main.rs`. The reason: the engine and every node must run
headless (tests, `penumbra-cli`, scripted runs), and a show must not depend on the editor.

## The network

A node has a kind (`family.name`, for example `tex.blur`), params, inputs wired from other
nodes' outputs, and outputs of one data kind each: `tex` (a GPU texture), `chan` (channels of
numbers, each with one or many samples), `table` (text), `geo`, `mat`, `light`, `view`. A
param can also be bound to a channel. Components hold child nodes and expose some of their
params; `tex.in`, `tex.out` and their siblings for the other data kinds are their ports. Paths are
`/name` and `/comp/name`, resolved relative to the node that holds them, so a component can be
moved.

The project file (`project.ron`) lists one node per line, sorted, so it diffs and merges.
Editing goes through the graph crate's edits, each undoable; the project thread writes the file
a second after the last change and reads it back when it changes on disk, merging external
edits as one history entry.

## Cooking

`graph/src/cook.rs` is the scheduler; its module comment is the authoritative description.
In short: pull based. Once per show frame the engine names what is wanted (projector outputs,
nodes watched over the control channel, visible thumbnails at low priority) and the cooker
evaluates those and everything upstream. A node recooks only when dirty: new or changed params,
an input whose source produced new outputs (every successful cook stamps its outputs with a
version), time-dependent and the clock moved, a finished job, or a lazy output that came to be
read. A failing node keeps its last good outputs, so the projectors keep showing the last
working frame; a panicking operator is caught and replaced.

Three declarations on a node type change the pull. An input marked `previous` reads what its
source made the pass before: it is no dependency, so a wire into it may close a loop
(`tex.feedback`). A node with an input chooser (`tex.switch`, `tex.sequence`) names the inputs
it needs before they cook; its steering inputs (`steers`) cook first, so the choice can read
them. An `eager` node (`chan.clock`, `chan.sequence`) cooks in every pass, wanted or not. The
user-facing version is in [Concepts](docs/concepts.md#how-the-network-runs).

Time is integer ticks derived from the frame number, not wall-clock deltas, so a show is
deterministic and does not drift against audio.

Everything slow runs off the cook thread: decoding, encoding, capture, audio, DMX sending and
Python each have their own threads and hand results over through bounded queues that drop
rather than block.

## Commands

`engine/src/commands.rs` registers every command. The editor's buttons, `penumbra-cli ctl`,
the JSON control channel on localhost and the MCP server all call the same registry, so
whatever the editor can do a script can do, and the reverse. New functionality gets a command
first and UI second. The reference in `docs/reference/commands.md` is generated from the
registry (`cargo xtask docs`).

## Adding a node type

1. Pick the family and a module in `crates/ops/src/<family>/`. A `tex` node is usually a
   `.rs` file with the descriptor and a `.wgsl` file with the pass.
2. Write `fn desc() -> NodeDesc`: the kind, a one-line summary, params with ranges, units and
   docs (`ParamSpec`), inputs and outputs. Declare `time_dependent` if the node reads the clock
   and `stateful` if it keeps anything between cooks; dirty tracking relies on both.
3. Implement `Operator` (`graph/src/node.rs`): `cook` runs on the cook thread and must not
   block; anything slow is a job.
4. Register it in the family's `register` function.
5. Tests: `tex` nodes get a golden image in `crates/ops/tests/golden/` (rendered on the software
   adapter, so the result is the same on every machine); `cargo xtask bless` rewrites the
   goldens after an intended change. Everything that reads files, network or hardware gets a
   hostile-input test.
6. `cargo xtask docs` regenerates the node reference and the one-page documentation.

## What is enforced

The workspace lints (`[workspace.lints]` in `Cargo.toml`) deny `unwrap`, `expect`, `panic`,
`unreachable`, `todo`, indexing that can panic, and `unsafe` code; the exceptions are tests
and the modules that talk to foreign code or the GPU (in `camera`, `gpu`, `mesh`, `ndi`,
`osc`, `pyhost`, `script`, `spout`, `system`, `ui`), each with a local allow and a `SAFETY`
comment per block. `clippy.toml` bans a few wgpu calls that panic on failure. Input
from files, network and hardware is treated as hostile: bounded parsers, budgets on mesh and
image sizes, timeouts.

`cargo xtask ci` is the gate before a commit: `fmt`, `clippy`, `test`, clippy for the Linux
target, the layer check and the third-party notices. There is no hosted CI yet; the gate runs
on the author's machine.

## Threads, in one place

| Thread | Crate | Does |
|---|---|---|
| UI | `ui` | The editor window, egui. |
| Cook | `engine`, `graph` | One cook pass per show frame. |
| Project | `engine`, `project` | Autosave, reading external edits. |
| One per projector | `output` | Presents that output's texture at the display's rate. |
| Control channel | `engine` | Accepts JSON connections on localhost. |
| Per device or file | `camera`, `audio`, `video`, `record`, `dmx`, `ndi`, `script` | Capture, decode, encode, send; results through bounded queues. |
