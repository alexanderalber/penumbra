# Control

Everything the UI does is a command, and every command is reachable over a local control
channel. That is how Claude Code builds and changes networks, and how a script, a test or
another program can drive penumbra. The full list is the [command reference](reference/commands.md);
`commands` on the channel returns the same list from the running app.

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

## Looking at results

- `node.snapshot {path, file?}` cooks a `tex` node (or the picture of a `geo` node) and writes
  a PNG.
- `chan.read`, `table.read` and `geo.info` read the other kinds.
- `ui.screenshot {file?}` saves a PNG of the whole window; the other `ui.*` commands click,
  drag, type and select as a user would, by automation ids that `ui.elements` lists.
- `ui.window {position?, size?, maximized?}` says where the editor window is and moves it, in
  physical pixels of the desktop as `output.list` gives the displays; the next start opens it
  there again.

## Headless runs

```
cargo run -p penumbra -- --headless script.jsonl [--project DIR] [--software] [--size WxH]
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
Each reply is printed.
Without `--project` it works on a fresh copy of the demo in the temp directory, so no project
is touched. `--software` renders on a software adapter (WARP on Windows): slow but the same on
every machine. `--size` is the window size in points.

## Environment variables

| | |
|---|---|
| `PENUMBRA_PORT` | the control channel's port, for the app and `penumbra-cli` |
| `PENUMBRA_PYTHON` | the Python for script nodes (see [Scripts](scripts.md#python)) |
| `PENUMBRA_LIBRARY` | the component library folder (default `%LOCALAPPDATA%\penumbra\components`) |
| `RUST_LOG` | log detail (`info` by default; `debug`, `warn`, ...) |
