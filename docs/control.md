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
  `edit.end`, all edits form one entry in the change log, and the UI is blue and read-only for
  the user. The claim lapses after 2 minutes without a command, and the user can take over at
  any time.
- Without a claim, each editing command waits for the same 3 seconds of quiet and becomes an
  entry of its own: convenient for one-off `penumbra-cli ctl` calls.
- `node.batch {ops}` applies several operations all or nothing.

Commands that only look (`node.info`, `node.snapshot`, `chan.read`, `ui.screenshot`, ...) work
at any time.

## Looking at results

- `node.snapshot {path, file?}` cooks a `tex` node (or the picture of a `geo` node) and writes
  a PNG.
- `chan.read`, `table.read` and `geo.info` read the other kinds.
- `ui.screenshot {file?}` saves a PNG of the whole window; the other `ui.*` commands click,
  drag, type and select as a user would, by automation ids that `ui.elements` lists.

## Headless runs

```
cargo run -p penumbra -- --headless script.jsonl [--project DIR] [--software] [--size WxH]
```

runs the real UI without a window and feeds it a script: one request per line
(`{"method": ..., "params": {...}}`, any command including `ui.*`), or `{"wait": seconds}` to
let the show run. Empty lines and lines starting with `//` are skipped. Each reply is printed.
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
