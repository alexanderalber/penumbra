# penumbra documentation

A short user guide. The UI stays sparse on purpose; what it does not explain on screen is
explained here. All of it, with every node and command, is also one page to search and read
offline: [penumbra.html](penumbra.html), which the `?` on every node opens.

- [Getting started](getting-started.md): running the app, the window, building a first network.
- [Concepts](concepts.md): how the network runs (cooking, wires and references, time), data kinds
  and node families, params and bindings, components, undo and the change log, who is editing,
  what the editor's colours and badges mean, the project directory.
- [Glossary](glossary.md): the words of the first hour, from binding and cook to premultiplied
  alpha and show mode.
- [The Helpers window](helpers.md): outputs, resolution, show-machine checks, persistent state,
  OSC, audio.
- [Audio](audio.md): reacting to sound (levels, onsets, tempo, beat and bar), and making it
  with ready-made instruments.
- [DMX](dmx.md): driving lights through the Enttec Open DMX USB, Art-Net, sACN or DDP, and
  receiving Art-Net or sACN from a lighting desk.
- [Recording](recording.md): the show to a movie file with `out.video` (H.264 or ProRes).
- [Spout](spout.md): pictures to and from other programs on this machine (TouchDesigner, OBS, Resolume) with `out.spout` and `tex.spout_in`.
- [NDI](ndi.md): pictures (and sound in) over the network, to and from OBS, vMix, cameras or another machine, with `out.ndi` and `tex.ndi_in`.
- [Camera](camera.md): a live camera or capture card with `tex.video_in`, calmed with `tex.denoise`, its motion with `tex.optical_flow`.
- [Tracking](tracking.md): blobs with ids that stay, from a camera, with `tex.blob_track`.
- [Mapping without a camera](mapping-without-camera.md): corners, mesh warp, soft edges and masks by eye, with `tex.mapper` and `tex.shapes`.
- [Scanning](scanning.md): structured light: which camera pixel each projector pixel lands on, the objects in front of a wall, blend weights, content on objects across projectors.
- [Looking around in 3D](navigation.md): the viewer's free view, steering a camera, the 3D mouse.
- [Shaders](shaders.md): writing a `tex.shader` in WGSL, porting from Shadertoy, a shader in a feedback loop that follows the music.
- [Scripts](scripts.md): writing a `script.python` node.
- [Control](control.md): the control channel, `penumbra-cli ctl`, the MCP server, headless runs, environment variables.

[Examples](../examples/README.md): small projects, each showing one side of penumbra.

[Content library](../library/README.md): ready-made effects for scanned objects, as components
with a preview each (add them from the add dialog's `library` tab).

Generated from the code, so always complete:

- [Node reference](reference/nodes.md): every node type with its inputs, outputs and params.
- [Command reference](reference/commands.md): every command of the control channel.
- `penumbra.html`: all of the above in one page for the browser, with a filter box; the `?` on
  a node opens it at that node (see [the documentation page](getting-started.md#the-documentation-page)).
  Longer texts for single node kinds live in `nodes/<kind>.md` and appear in their entries there.
