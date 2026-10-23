## What it is for

`tex.select` brings in the picture of another node without a wire: you name the node in `node`,
and its output comes out here. The editor draws the reference as a dashed line.

That helps where a wire would be awkward or impossible:

- **Across components.** Wires cross a component's edge only through its ports. A
  `tex.select` inside a component can reach any node of the project by path, without adding a
  port for it.
- **Far across the network.** One picture used in many places (the camera, a mask) need not be
  wired to every one of them.
- **A picture to choose by name.** `node` is a param like any other, so it can be changed from
  the control channel to show another node's picture.

## Naming the node

`node` is read from where the `tex.select` sits: `x` is the node `x` beside it, `../x` one level
up, `sub/x` inside the component `sub`, and `/a/b` the full path from the top. Paths relative to
the select keep working when its component is moved or saved to the library.

`output` picks which output of that node, counted from 0, for nodes with more than one:
`tex.render`'s `data` is 1.

A select that points at a `tex.null` at the end of a chain stays valid however that chain
changes.
