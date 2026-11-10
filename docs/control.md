# Control

Everything the UI does is a command, and every command is reachable over a local control
channel. That is how Claude Code builds and changes networks, and how a script, a test or
another program can drive penumbra. The full list is the [command reference](reference/commands.md);
`commands` on the channel returns the same list from the running app. This page is about that
channel; OSC from a desk, QLab or a phone is in [Helpers](helpers.md#osc) and `chan.osc_in`;
a control page in the browser is in [concepts](concepts.md#a-control-page-in-the-browser) and `chan.http_in`.

## `penumbra-cli ctl`

`penumbra-cli` sends one request to the running app and prints the reply:

```
cargo run -p penumbra-cli -- ctl node.list
penumbra-cli ctl node.create path=/noise1 kind=tex.noise
penumbra-cli ctl node.set '{"path": "/noise1", "params": {"speed": 0.5}}'
penumbra-cli ctl node.snapshot path=/noise1 file=noise.png
```

Params are one JSON object, or `key=value` pairs where each value is read as JSON when it parses
and as a string otherwise. `penumbra-cli` with no arguments pings the app. The exit code is 0
when the reply says `"ok": true`. `--port N` before `ctl` picks another port.

`penumbra-cli soak` runs the engine headless for a while and reports leaks and stalls
(`penumbra-cli soak --help`).

## MCP server

`penumbra-cli mcp` serves the [Model Context Protocol](https://modelcontextprotocol.io) on
stdin and stdout, so Claude Code, Claude Desktop and other MCP clients can drive a running
penumbra without a shell call per command and get pictures back inline. It is a client of the
control channel like `ctl`, so it reaches the app on 9880 (`--port N` before `mcp`, or
`PENUMBRA_PORT`). For Claude Code:

```
claude mcp add penumbra -- penumbra-cli mcp
claude mcp add penumbra -- C:\penumbra\penumbra-cli.exe --port 9881 mcp
```

A running server keeps its exe locked, and every Claude Code session starts one. So when you
build penumbra yourself, point the client at a copy, not at the build output: `cargo xtask mcp`
builds `penumbra-cli` in release and puts the copy in `target/mcp/`. Run it again after a
change to `penumbra-cli`; sessions already open keep the old server until they reconnect
(`/mcp` in Claude Code).

| Tool | |
|---|---|
| `command_list` | every command with its summary; `filter` |
| `command_run` | one command: `{id, params?}` |
| `command_batch` | several commands in order: `{steps: [{id, params?}], stop_on_error?, label?}`, as one change log entry named `label` (see [Taking turns](#taking-turns)); steps before a failure stay applied |
| `doc_inspect` | the project (`project.info`) and every node (`node.list`) |
| `render_preview` | a node's picture as PNG (`node.snapshot`): `{path, output?, max_side?}` |
| `ui_inspect`, `ui_elements`, `ui_click`, `ui_drag`, `ui_key`, `ui_type` | the `ui.*` commands of the same name |
| `ui_screenshot` | the window as PNG: `{max_side?}` |
| `ui_control` | any other `ui.*` command: `{method, params?}` |

Pictures are shrunk to `max_side` pixels on their longer side (960 for nodes, 1568 for the
window; 0 keeps the full size). The tools that only look are marked read-only, so a client
can run them without asking. An unknown or mistyped argument is refused with the names the
tool accepts; a failing command comes back as an error result with the app's message. The
resources `penumbra://document` and `penumbra://commands` hold what `doc_inspect` and
`command_list` return. Requests are answered one at a time, so a command that measures for a
minute holds the next one.

## The channel

JSON lines over TCP on `127.0.0.1`, port 9880 (`--port`, or `PENUMBRA_PORT` for the app and
`penumbra-cli` alike). One request per line, one reply per line:

```
{"id": 1, "method": "node.info", "params": {"path": "/noise1"}}
{"id": 1, "ok": true, "result": {...}}
```

`id` and `params` are optional. A failure answers `{"ok": false, "error": "..."}`. The channel
listens on the loopback interface only; a line that is not a JSON object with a `method`, or is
longer than 4 MiB, gets one error reply and the connection is closed.

At most 16 connections are open at once. A further one takes the place of the connection that
has waited longest for its next request: that client gets an error line, then its connection
closes. A connection in the middle of a request is never closed. When all 16 are busy, the new
connection gets the error line instead.

## Taking turns

Edits from the channel pass the same gate as the user's (see [who is
editing](concepts.md#who-is-editing)):

- `edit.begin {label?}` claims the turn, once the user has not edited for 3 seconds. Until
  `edit.end`, all edits form one entry in the change log (named by `label` when there is
  one), and the UI is blue and read-only for
  the user. The claim lapses after 2 minutes without a command, and the user can take over at
  any time.
- Without a claim, each editing command waits for the same 3 seconds of quiet and becomes an
  entry of its own. It keeps the turn for 5 seconds, so the UI stays blue while commands come
  in one by one, and goes back to the user when they stop. The hold is a signal, not a lock:
  an edit of the user's (also from OSC, a key or the 3D mouse) ends it, and the next command
  waits for quiet again. Convenient for one-off
  `penumbra-cli ctl` calls and for `command_run` over MCP.
- `node.batch {ops}` applies several operations all or nothing.

Commands that only look (`node.info`, `node.snapshot`, `chan.read`, `ui.screenshot`, ...) work
at any time.

In [show mode](getting-started.md#show-mode) (`show.lock`) every editing command is refused
until `show.unlock`, and so are commands that write into the project folder or switch projects,
and the test aid `gpu.simulate_loss`.
Commands that only look, `cook.play` and OSC/DMX keep working. The exception among the edits:
`chan.press` and `node.set` still set the `value` of a control shown on the show screen
(`show` ticked), without an undo step; the reply says `"history": "none_in_show_mode"`.

## Looking at results

- `node.snapshot {path, file?, opaque?}` cooks a `tex` node (or the picture of a `geo` node) and
  writes a PNG. Its reply has the channels' minimum, maximum and mean as stored. The PNG has
  straight alpha. `opaque: true` writes the colour channels as stored and leaves alpha out: a
  picture then lies on black. A texture whose alpha is 0 everywhere but whose colour is not (a
  simulation's state, say) is written that way by itself, and the reply says `"opaque": true`.
  Without that, its PNG would be fully transparent.
- `chan.read`, `table.read` and `geo.info` read the other kinds.
- `ui.screenshot {file?}` saves a PNG of the whole window; the other `ui.*` commands click,
  drag, type and select as a user would, by automation ids that `ui.elements` lists.
- `ui.window {position?, size?, maximized?}` says where the editor window is and moves it, in
  physical pixels of the desktop as `output.list` gives the displays; the next start opens it
  there again.
- `ui.pointer {show?, glide?, size?}` draws a mouse pointer into the window's picture, for
  screenshots and recordings, where clicks from the control channel show no cursor. `show`
  draws an arrow and a ring on each click. `glide` is a number of frames: `ui.click`,
  `ui.drag`, `ui.scroll` and `ui.hover` then move the pointer to their target over that many
  frames instead of jumping there. `ui.hover {id | x, y}` only moves the pointer and leaves it
  there, so a port shows its tooltip.

## Headless runs

```
cargo run -p penumbra -- --headless script.jsonl [--project DIR] [--software] [--size WxH] [--scale F] [--speakers] [--save]
```

runs the real UI without a window and feeds it a script: one request per line
(`{"method": ..., "params": {...}}`, any command including `ui.*`), `{"wait": seconds}` to
let the show run, `{"wait_for": id, "timeout": seconds}` to let it run until the element `id`
is on screen (`timeout` optional, 30 s by default), or `{"resize": [width, height]}` to change
the window's size as a maximize would. `wait` is wall-clock time, so on a busy machine less
has happened by its end; `wait_for` an element that appears only once something cooked (the
viewer's handles, a helper's list) before clicking it. Empty lines and lines starting with
`//` are skipped. `$HERE` in any string of a line is the project's folder (with forward
slashes), so a script kept beside `project.ron` names its files wherever the project lies:
`{"method": "node.snapshot", "params": {"path": "/out", "file": "$HERE/snap/out.png"}}`.
Each reply is printed as `method: {json}` on one line, as soon as its line of the script is
done.
A node that fails writes its error to the log once, with its path and kind, also when the
script never asks for it: a shader that does not compile inside a component, behind the
`/out` the script snapshots, shows up as `WARN ... /waves/step (tex.shader): error: ...`.
Shaders compile in the background, so the line can come a few frames after the node first
cooks. `node.list {"problems": true}` lists every node with an error or a warning as of its
last cook, with the message; a node that has not cooked yet has none. The run's exit code does
not depend on them.
Without `--project` it works on a fresh copy of the demo in the temp directory, so no project
is touched. With `--project` the run leaves the project folder as it found it. A param the
script sets is gone when the run ends, and the next run starts from the same file. The run
writes `project.ron` only where the script calls `project.save`. It writes no `layout.ron` and
no state snapshot, except where the script calls `state.save`. Files the script asks for
(snapshots, recordings) are written as usual. A folder without a project gets a new
`project.ron` at the start, as in the app. `--save` makes the run save as the app does:
autosave, the save on closing, the editor layout and the state snapshot. `--no-save` is the
default and is accepted, so older scripts still run.
`--software` renders on a software adapter (WARP on Windows): slow but the same on
every machine. `--size` is the window size in points. `--scale` is the number of pixels per
point, from 0.5 to 4: `--size 1280x720 --scale 2` draws the 1280 by 720 layout at 2560 by 1440
pixels, so its text stays sharp and large in a video.
A headless run plays no sound: `out.audio` plays into nothing, since frames without a window
come too slowly and unevenly for a speaker. `--speakers` lets it open its devices after all.

### Recording a headless run

`{"record": {"dir": "E:/rec", "fps": 30}}` in the script starts writing every drawn frame as
`frame_000000.png`, `frame_000001.png`, ... into `dir`, which must not hold frames yet.
`{"record": false}` stops; so does the end of the script. Put the frames on a timeline in a video
editor as an image sequence at the same rate.

While it records, the show clock moves by exactly one recorded frame per picture, however long a
picture takes to draw (`cook.stepping`, see below). So a recording looks the same on a fast and
a slow machine, and a sound file the show plays lines up with the frames when the editor puts
both at the start. `fps` must divide the show rate: 30 or 60 for a 60 fps show, not 25.

- `wait` and `wait_for` count show time while recording. `{"wait": 2}` records 2 seconds.
- Every frame a request takes is recorded. A `ui.drag` with 30 `steps` lasts a second at
  30 fps; with `ui.pointer {glide}` the pointer first travels to the drag's start.
- egui's own animations run at the recording's rate too.
- `marks.jsonl` beside the frames has a line per script line that took frames: its line number
  in the script, what it was (`ui.drag`, `wait`, ...) and its first and last frame. Use it to cut,
  zoom or put a caption on one step.
- `resize` is refused while recording: every frame has one size.

`cook.stepping {on?}` and `cook.step {frames?}` are the commands underneath. With stepping on,
neither the timer nor a projector's vsync moves the clock. Each `cook.step` cooks the given
number of frames at once and answers with the frame and the pass, once the GPU has drawn them. So
a read-back right after a long step does not have to wait for queued frames. Stepping off, the show
goes on from where the steps left it.
While stepping, a node that reads a picture back (`chan.from_tex`, `tex.blob_track`) waits for
the GPU. So its channels belong to the frame they were read in, however slow the GPU. Live, it
waits at most 4 ms and otherwise shows the result a frame later.

## Environment variables

| | |
|---|---|
| `PENUMBRA_PORT` | the control channel's port, for the app and `penumbra-cli` |
| `PENUMBRA_PYTHON` | the Python for script nodes (see [Scripts](scripts.md#python)) |
| `PENUMBRA_LIBRARY` | the component library folder (default `%LOCALAPPDATA%\penumbra\components`) |
| `RUST_LOG` | log detail (`info` by default; `debug`, `warn`, ...) |
