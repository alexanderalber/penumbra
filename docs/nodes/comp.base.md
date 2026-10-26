## What it is for

A network grows fast: a few scenes, a mapping, some audio analysis, and the editor is full. A
component packs part of the network into a single node. From outside it looks like any other
node, with inputs, outputs and a few params; inside it holds its own nodes, and you go in to
work on them.

That is good for three things:

- **Order.** Each scene of a show, the mapping of each projector, the audio analysis: one
  component each, and the top level shows how they connect.
- **Reuse.** A component can go into the library and be added to any project, as a copy.
- **A simple surface.** Only the params you expose show on the component. The scene's speed and
  colour are on the outside; the twenty nodes that make it are tucked away.

## Making one

Select nodes and press Ctrl+G. penumbra puts them into a new component and adds a port wherever
a wire crossed the edge of the selection. I or a double-click goes in, U goes back up. The node
menu's "Dissolve" undoes the grouping.

## Ports

Wires enter and leave a component only through ports, which are nodes inside it. Each data kind
has a pair: `tex.in` and `tex.out`, `chan.in` and `chan.out`, and the same for `table`, `geo`,
`mat`, `light` and `view`. The `.in` nodes are the component's inputs, the `.out` nodes its
outputs. What is wired to an input outside comes out of the matching `.in` node inside; what is wired
into a `.out` node inside comes out of the component. Bindings and `tex.select` are not wires:
they may reach in and out of components by path.

## Exposed params

`expose` lists inner params to show on the component, one per line: `noise1.speed`, or with a
name of its own, `speed = noise1.speed`. Changing the exposed param changes the inner one, and
you can bind it like any param.

More in the Components section of the concepts guide.
