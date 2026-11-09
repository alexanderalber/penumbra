## What it is for

A network grows fast: a few scenes, a mapping, some audio analysis, and the editor is full. A
component packs part of the network into a single node. From outside it looks like any other
node, with inputs, outputs and a few params; inside it holds its own nodes, and you go in to
work on them.

That is good for three things:

- **Order.** Each scene of a show, the mapping of each projector, the audio analysis: one
  component each, and the top level shows how they connect.
- **Reuse.** A component can go into the library and be added to any project, as a copy.
- **A simple surface.** Only the params you declare or expose show on the component. The scene's speed and
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

## Params of its own

The `params` text (under more) gives the component params, one per line. TouchDesigner calls
these custom parameters. A line uses the grammar of a shader header, and the `//` in front may
stay when you paste one:

```text
@param preset choice(coral|maze|worms) = coral -- the pattern
@param feed float = 0.055 0..0.1
@group look
@param glow float = 1 0..4
@expose speed = noise1.speed
```

- **`@param`** makes a param the component holds: `name kind = default min..max -- doc`. Kinds
  are `float`, `int`, `bool`, `vec2`, `vec3`, `vec4`, `color`, `palette` and `choice(a|b)`. Set
  and bind it like any param; a value it does not take is refused at once. Inner nodes do not
  follow it yet.
- **`@group`** puts the params after it into a fold of the panel. `@group` alone ends the fold.
- **`@expose`** shows an inner param on the component: `@expose noise1.speed`, or with a name of
  its own, `@expose speed = noise1.speed`. Changing the exposed param changes the inner one,
  and you can bind it like any param. The inner node keeps the value and the range.
- **Comments:** lines starting with `#`, and `//` lines without an `@`.
- **Mistakes:** a line that declares nothing is listed below the rows with its line number and
  the reason; the rest still works. Each name may appear once. `params`, `shelf`, `about` and
  `reset` are taken.

Files written before `params` existed listed the exposed params in a param `expose`. They load
as `@expose` lines and are saved in the new form.

More in the Components section of the concepts guide.
