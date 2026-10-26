# Glossary

The words a newcomer meets in the first hour, in alphabetical order. Each entry says first what
the word means for you, then where to read on.

**Binding.** A param that follows a channel instead of its typed value: an LFO turning a
brightness, a MIDI knob moving a position. Drag a wire from a `chan` output onto the param's row,
or use the link button at the row's end. The typed value comes back when the binding is removed.
See [Bindings](concepts.md#bindings).

**Blend.** Where two projectors light the same spot, each gives part of the light, so the spot is
not twice as bright and small misalignments do not double. With a camera, `scan.blend` measures
the weights ([Scanning](scanning.md#6-blend-scanblend)). Without one, two `tex.mapper` nodes fade
into each other where their regions overlap ([Mapping without a
camera](mapping-without-camera.md#two-projectors-side-by-side-the-soft-edge)). A composite's
"blend modes" (`add`, `multiply`, ...) are a different thing: how two pictures are laid on top
of each other.

**Channel.** A named number that changes over time, such as `level` from the audio analysis or
`x` from the mouse. Channels flow along green wires, between `chan` nodes, and drive params
through bindings. A channel can also hold many samples at once (a spectrum, a trail).

**Component.** A node that holds other nodes, to keep a network tidy or to reuse a piece of it.
Select nodes and press Ctrl+G to make one; I goes in, U comes out. Saved to a file, it goes into
the add dialog's `library` tab. See [Components](concepts.md#components).

**Content library.** Ready-made effects for scanned objects, shipped with penumbra as
components: water in a crate, a lamp going round a pumpkin, contour lines. They are in the add
dialog's `library` tab, marked `penumbra`. See [its catalogue](../library/README.md).

**Control channel.** The way a script, a terminal or Claude drives the running app: JSON
requests over a local network connection, port 9880. Everything the editor does is a command
there too. `penumbra-cli ctl` sends one from the command line. See [Control](control.md).

**Cook.** A node *cooks* when it computes its outputs. penumbra cooks only what an output, the
viewer or a thumbnail needs, and only when something it depends on changed. A node that nothing
needs shows "not cooked" and costs nothing. See [How the network
runs](concepts.md#how-the-network-runs).

**Cue.** A signal that starts something at a moment of the show: a key, an OSC message from QLab
or a lighting desk, a step of a `chan.sequence`. In penumbra a cue is a channel that turns on.
A clock, a show or a clip bound to it starts over. See [Video clips on cue](concepts.md#video-clips-on-cue).

**Expose.** Showing an inner param of a component on the component itself, so it can be set
without going in. List it in the component's `expose` param, one per line:
`speed = noise1.speed`. See [Components](concepts.md#components).

**Face.** A flat part of a scanned scene: a crate's lid, a box's side, a wall. `scan.scene` finds
the faces and numbers them, and content can be put on each one. Face numbers change with every
scan; regions keep their names. See [Scanning](scanning.md#5-faces-and-objects-scanscene).

**Family.** The first part of a node's kind: `tex` for pictures, `chan` for channels, `geo`,
`mat`, `light` and `view` for 3D, `table` for text, `script` for Python, `out` for outputs,
`comp` for components. Each node's header has its family's colour, and so does the family's
block in the add dialog.

**Feedback.** A loop that hands each frame's picture back to the start of a chain, so the next
frame builds on it: trails, echoes, patterns that grow. A `tex.feedback` closes the loop through
its `previous` input. See [tex.feedback](reference/nodes.md#texfeedback) and the `feedback`
example.

**Helpers window.** The window for setting up a show: displays and projectors, test patterns,
blend and colour match, resolution, saved state, OSC and audio. Right-click on empty canvas and
choose **Helpers**. See [The Helpers window](helpers.md).

**Linear light.** Picture values in penumbra are amounts of light: 0.5 is half the light of 1,
and a fade to 0.5 takes away half the light. The eye sees about 0.2 as a mid grey, so numbers
from a paint program or a Shadertoy shader look pale until they are converted
([Shaders](shaders.md#from-shadertoy-and-glsl)). Hex colours in palettes are converted for you.
Values above 1 are allowed and stay brighter than white until the output.

**Lock.** A locked node holds its outputs and stops cooking, whatever happens upstream; its
inputs may even be removed. Use it for calibration results and anything slow to make again.
Locked outputs survive a restart. See [What the editor shows](concepts.md#what-the-editor-shows).

**Mapper.** `tex.mapper` puts the content onto one projector: four corners to drag, a mesh to
bend, a soft edge where projectors overlap. Its output is that projector's picture. See [Mapping
without a camera](mapping-without-camera.md).

**MCP.** The Model Context Protocol, how Claude Code and other AI clients talk to tools.
`penumbra-cli mcp` lets them drive penumbra and see its pictures. See [Control](control.md#mcp-server).

**Node reference.** A param that names another node, such as `tex.select`'s `node`: the node
uses that node's output without a wire. It reaches across components, and it is drawn as a dashed
line. See [Paths](concepts.md#paths).

**Object.** In a scan, whatever is not flat: a pumpkin, a vase, a person. `scan.scene` and
`scan.objects` find objects as masks, and content drawn in the camera's picture lands on them.
See [Scanning](scanning.md).

**Param.** A setting of a node, shown as a row in the param panel: a number, a colour, a choice,
a file. The circled "i" before its name explains it. See [Params](concepts.md#params).

**Piece.** One part of a show, such as the intro or a scene: a chain of nodes that makes one
picture. A `chan.sequence` plays pieces one after another, a `tex.switch` picks one by a key or a
cue. See [Shows](concepts.md#shows).

**Port.** One of the sockets of a component, as a node inside it: `tex.in`, `tex.out`,
`chan.in` and so on for every kind. What is wired to the component outside comes out of its
`in` node inside. See [Component ports](reference/nodes.md#component-ports).

**Premultiplied alpha.** penumbra stores a picture's colour already multiplied by its opacity: a
half transparent white is `0.5, 0.5, 0.5, 0.5`. For you this matters in shaders: return
`vec4f(colour * alpha, alpha)`. Colour params are the other way (straight alpha), as a colour
picker shows them. Fully transparent pixels are black.

**Projector.** An `out.projector` node: it shows its picture fullscreen on a display. Its
`display` names which one. Each projector has colour correction and test patterns of its own.
See [Helpers](helpers.md#outputs).

**Region.** Faces of a scan grouped into one part of the scene, the way a person sees it: one
crate front instead of its panels and rib strips. `scan.regions` makes them, and you can merge
and name them by clicking in the viewer. Region names survive a rescan. See
[Regions](scanning.md#51-regions-scanregions).

**RON.** The text format of `project.ron` and component files (Rusty Object Notation). One node
per line, sorted, so a project can be read, edited by hand and kept in git. The app picks up an
edit to the file while it runs. See [The project directory](concepts.md#the-project-directory).

**Scan.** A camera films each projector's stripe patterns, and penumbra learns which camera pixel
each projector pixel lands on. From that it finds faces and objects, blends projectors and puts
content on things. One scan takes about 20 seconds per projector in a dark room. See
[Scanning](scanning.md).

**Scene.** Two meanings. In scanning, the folder `scan.scene` writes: the faces and objects of
one scan, which `scan.regions`, `scan.face_template` and the content read. In 3D, the geometry,
materials, lights and camera that a `tex.render` draws.

**Show clock.** The time every animation reads: frames at 60 a second, starting at 0 when a
project opens. The status bar pauses and runs it. See [Time](concepts.md#time).

**Show mode.** A lock for a performance: no edits, no saving, no project switches, while OSC,
DMX, keys and cameras keep driving the show. When the GPU or the editor fails, the app starts
again by itself. The padlock in the status bar turns it on. See [Show mode](getting-started.md#show-mode).

**State.** What a node builds up over time: a feedback loop's last frame, a cache's frames, a
learned background. It is lost on restart unless **keep node state across restarts** is on in
the Helpers window. R starts the selected nodes over. See [State](helpers.md#state).

**Surface.** The picture that the `tex.mapper` nodes fed by one node share between them. Each
mapper shows a `region` of it, and mappers of one surface blend where their regions overlap.
`tex.shapes` draws its masks in the surface's coordinates.

**Thumbnail.** The small live picture in a node's body. Thumbnails cook with the time left in a
frame, after the outputs. The editor's right-click menu sets them to always live, only when free
or never, for a large network. See [What the editor shows](concepts.md#what-the-editor-shows).

**Viewer.** The large picture at the right: the selected node's output, or the node pinned with
`pin`. Some nodes are edited right there: a mapper's corners, a `tex.shapes`' shapes, a scan's
regions. See [The viewer](getting-started.md#the-viewer).

**Wire.** A connection from one node's output to another's input of the same kind. It carries
the whole output, and its colour is the kind's. Wires may not form a loop, except into a
feedback input. See [Nodes, families and data kinds](concepts.md#nodes-families-and-data-kinds).
