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
- [DMX](dmx.md): driving lights through the Enttec Open DMX USB, Art-Net, sACN or DDP, and
  receiving Art-Net or sACN from a lighting desk.
- [Recording](recording.md): the show to a movie file with `out.video` (H.264 or ProRes).
- [Spout](spout.md): pictures to and from other programs on this machine (TouchDesigner, OBS, Resolume) with `out.spout` and `tex.spout_in`.
- [NDI](ndi.md): pictures (and sound in) over the network, to and from OBS, vMix, cameras or another machine, with `out.ndi` and `tex.ndi_in`.
- [Camera](camera.md): a live camera or capture card with `tex.video_in`, calmed with `tex.denoise`, its motion with `tex.optical_flow`.
- [Tracking](tracking.md): blobs with ids that stay, from a camera, with `tex.blob_track`.
- [Scanning](scanning.md): structured light: which camera pixel each projector pixel lands on, the objects in front of a wall, blend weights, content on objects across projectors.
- [Looking around in 3D](navigation.md): the viewer's free view, steering a camera, the 3D mouse.
- [Shaders](shaders.md): writing a `tex.shader` in WGSL.
- [Scripts](scripts.md): writing a `script.python` node.
- [Control](control.md): the control channel, `penumbra-cli ctl`, headless runs, environment variables.

[Examples](../examples/README.md): small projects, each showing one side of penumbra.

Generated from the code, so always complete:

- [Node reference](reference/nodes.md): every node type with its inputs, outputs and params.
- [Command reference](reference/commands.md): every command of the control channel.
- `penumbra.html`: all of the above in one page for the browser, with a filter box; the `?` on
  a node opens it at that node (see [the documentation page](getting-started.md#the-documentation-page)).
  Longer texts for single node kinds live in `nodes/<kind>.md` and appear in their entries there.
