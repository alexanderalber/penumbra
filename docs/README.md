# penumbra documentation

A short user guide. The UI stays sparse on purpose; what it does not explain on screen is
explained here.

- [Getting started](getting-started.md): running the app, the window, building a first network.
- [Concepts](concepts.md): data kinds and node families, params and bindings, components,
  undo and the change log, who is editing, what the editor's colours and badges mean, the
  project directory.
- [The Helpers window](helpers.md): outputs, resolution, show-machine checks, persistent state,
  OSC, audio.
- [Audio](audio.md): reacting to sound: levels, onsets, tempo, beat and bar.
- [Camera](camera.md): a live camera or capture card with `tex.videoin`.
- [Tracking](tracking.md): blobs with ids that stay, from a camera, with `tex.blobtrack`.
- [Shaders](shaders.md): writing a `tex.shader` in WGSL.
- [Scripts](scripts.md): writing a `script.python` node.
- [Control](control.md): the control channel, `pn ctl`, headless runs, environment variables.

Generated from the code, so always complete:

- [Node reference](reference/nodes.md): every node type with its inputs, outputs and params.
- [Command reference](reference/commands.md): every command of the control channel.
