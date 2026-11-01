## What it is for

A `tex.null` does nothing to the picture, and that is the point: it is a fixed place in the
network that stays put while everything around it changes.

- **A stable end of a chain.** Bindings and `tex.select` refer to a node by its
  path. Point them at a null at the end of a chain, and you can rebuild, insert or delete
  nodes before it without touching anything that refers to it.
- **A place to look.** Pin the viewer to a null at an interesting point of a chain, and it keeps
  showing that point while you work elsewhere.
- **A tidy network.** One null where a picture fans out to several nodes keeps the wires
  readable, and its name says what the picture is (`content`, `mask`, `camera_clean`).

The input is passed on as it is, without a copy, so a null costs nothing.

Every family that runs on wires has its null: `chan.null`, `audio.null`, `table.null`,
`geo.null`, `mat.null`, `light.null` and `view.null`. Each passes its input on without a copy.
A `chan.null` is the usual thing to bind params to: rebuild the chain that makes the numbers,
and the bindings stay. A null shows in the viewer what the node before it shows: the picture of
the geometry or the material, the scope of the sound, the lights or the camera as lines.
