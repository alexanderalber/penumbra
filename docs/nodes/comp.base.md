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

The quickest way is the menu. Inside the component, the link button of a param has a section
named after the component:

- **Put on component:** the component gets a param of that name, kind, range and value, and
  the inner param follows it. Other nodes inside can follow it too. Use it for a value several
  nodes share, such as a preset or a size.
- **Show on component:** the inner param gets a row on the component. The value and the range
  stay with the inner node. Use it for a value only that node reads.
- **follow preset:** the param follows the component's `preset`.

The component's rows say at their right end who follows them ("used by step", a click lists
them) or which inner param they set (`step.feed`). "unused" marks a param nothing follows yet.

Behind the menu is the `params` text ("edit params" under the rows), one line per param. TouchDesigner calls
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
  and bind it like any param; a value it does not take is refused at once. Inner nodes follow
  it through a binding to `parent.preset` (TouchDesigner: `parent().par.Preset`). They take the
  value whole, a choice by name.
- **`@group`** puts the params after it into a fold of the panel. `@group` alone ends the fold.
- **`@expose`** shows an inner param on the component: `@expose noise1.speed`, or with a name of
  its own, `@expose speed = noise1.speed`. Changing the exposed param changes the inner one,
  and you can bind it like any param. The inner node keeps the value and the range.
- **`@reset`** names params whose change starts the component over: `@reset preset seed
  width height`. More under "Starting over" below.
- **Comments:** lines starting with `#`, and `//` lines without an `@`.
- **Mistakes:** a line that declares nothing is listed right below the text with its line
  number and the reason; the rest still works. Each name may appear once. `params`, `shelf`, `about` and
  `reset` are taken.

Files written before `params` existed listed the exposed params in a param `expose`. They load
as `@expose` lines and are saved in the new form.

## Inner nodes follow

Bind an inner param to the component's param: `node.bind {param: "/rd/step.preset", from:
"parent.preset"}`, written `binds: {"preset": "parent.preset"}` in the file. `parent` is the
component the node sits in, whatever it is called, so a copy from the library follows its own
component. The follower's row shows `parent.preset`; a click on it goes to the component. When a
follower cannot take a value (a choice it lacks), the component's panel lists the warning at the
top. Every follower takes the new value in the same frame, also when the component's
param is bound to a dropdown or an LFO outside.

More in the Components section of the concepts guide.

## Starting over

A simulation inside a component keeps its last frame in a feedback loop. A new preset then
works on the old picture. Two things start the loop over from its seed:

- **`reset`:** while it is on, every node inside that remembers earlier frames starts over in
  each frame. These are `tex.feedback`, `chan.feedback`, `tex.fluid`, `vol.smoke`, trails,
  clocks and the other nodes the `reset` button acts on, in nested components too. Held on,
  the simulation stays at its start; it runs again when `reset` goes off. Bind it to a key or
  any channel, as TouchDesigner users bind a pulse parameter. The `reset` button in the panel's
  header does the same once.
- **`@reset preset seed`:** when the value of one of these params changes, the component starts
  over once. A value set by hand, by undo or through a binding counts. The new start already
  uses the new value, in the same frame. Only `@param` names may stand there; for an exposed
  param, make it an `@param` that the inner param follows ("Put on component").

Nodes that only follow the clock, such as an LFO or a shader that reads `time`, do not start
over. A locked node keeps what it holds.

**Key 0 starts the simulation over:** add a `chan.keyboard_in` with `keys` at `0`, and bind the
component's `reset` to its `key_0`:

```text
node.create {path: "/keys", kind: "chan.keyboard_in", params: {keys: "0"}}
node.bind {param: "/sim.reset", from: "/keys", channel: "key_0"}
```

A tap restarts the simulation; held, it waits at the start. This also works while the show is
locked: a key is no edit.

**Restart when it has died:** measure the picture inside with `chan.from_tex`, compare with
`chan.math` (`below` a mark), pass that through a `chan.feedback`, and bind `reset` to it. The
feedback node hands the measurement on a frame later, so the binding makes no loop. The
component starts over one frame after the measurement passes the mark.

