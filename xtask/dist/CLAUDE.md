# penumbra (release build)

This folder is an unpacked release of penumbra, a node-based tool for projection mapping, blob
tracking and audio- or DMX-driven installations. There is no source code here: you drive the
running app over its control channel. The documentation of exactly this version is in `docs/`.

## Read first

1. `docs/control.md`: the control channel, `penumbra-cli ctl`, taking turns with the user,
   looking at results, headless runs.
2. `docs/concepts.md`: data kinds, node families, params and bindings, components.
3. As needed: `docs/reference/commands.md` (every command) and `docs/reference/nodes.md`
   (every node type), plus the topic pages listed in `docs/README.md`.

`examples/` holds small finished projects (`examples/README.md` lists them); their
`project.ron` files are good models of networks, bindings and feedback loops. Open one with
`project.open dir=<absolute path to the example>`.

## Talking to the app

The user starts `penumbra.exe` (a double-click opens a demo project). It listens on
`127.0.0.1:9880`, JSON lines over TCP. From this folder:

```
penumbra-cli ctl ping
penumbra-cli ctl commands                       # every command with a one-line summary
penumbra-cli ctl node.kinds kind=tex.noise      # a node type's params, inputs and outputs
penumbra-cli ctl node.list
penumbra-cli ctl node.create path=/noise2 kind=tex.noise
penumbra-cli ctl node.snapshot path=/noise2 file=C:/temp/noise2.png
```

`commands` and `node.kinds` describe the running app itself, so they are always right for this
version.

## Pitfalls

- In Git Bash, node paths like `/noise1` are turned into Windows paths before they reach
  `penumbra-cli`. Prefix the call with `MSYS_NO_PATHCONV=1`, or use PowerShell.
- File paths inside JSON need forward slashes (`C:/temp/x.png`) or doubled backslashes.
- Edits wait until the user has not edited for 3 seconds. For several edits in a row, claim the
  turn with `edit.begin` and end it with `edit.end`; the user sees the UI turn blue meanwhile.
- Look at what you built: `node.snapshot` writes a PNG of a node, `ui.screenshot` one of the
  whole window. Read the PNG before calling a visual change done.
- To try something without touching the user's window or project, run
  `penumbra.exe --headless script.jsonl` (one request per line, `{"wait": seconds}` to let it
  run). It works on a temporary copy of the demo and prints every reply; see
  `docs/control.md#headless-runs`.
