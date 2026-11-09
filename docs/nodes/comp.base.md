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
  width height`, or with a condition, `@reset preset unless morph`. More under "Starting over"
  below.
- **`@preset`** gives the values that choosing an entry of a choice writes: `@preset
  preset=maze feed=0.029 kill=0.057`. More under "Presets" below.
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
param is bound to a dropdown or an LFO outside. A bound choice with presets is the exception:
its followers take the new entry one frame later, together with the entry's values (see
"Presets").

More in the Components section of the concepts guide.

## Presets

A preset is a set of values for some of the component's params, chosen by its name in a choice.
Choosing an entry writes its values into the params. After that the values are yours: tune a
slider, and the tuned value is saved with the project. TouchDesigner's palette has the
presetManager for this job.

```text
@param preset choice(coral|maze) = coral -- the pattern
@param speed float = 1 0..4
@expose feed = step.feed
@expose kill = step.kill
@preset preset=* feed=0.0545 kill=0.062
@preset preset=maze feed=0.029 kill=0.057 -- a labyrinth
```

- **The `*` line** lists the params the presets write, each with the value every entry starts
  from. Choosing `maze` writes the `*` values, then maze's own. A preset never writes a param on
  no `*` line: `speed` above stays where you put it.
- **Entry lines** give one entry's own values. Several lines for one entry add up. The text
  after `--` describes the entry.
- **Values** read like an `@param` default: `0.03`, `true`, `[1, 0.5, 0, 1]`, an entry of a
  choice by name, a palette by name or colours. A preset may write `@param` and `@expose` names.
- **Choosing** writes the values in the same edit, and one undo takes all of them back. This
  holds for the panel, `node.set`, `node.batch` and OSC. Choosing the entry that is chosen
  already writes its values again: back to the preset. Opening a project writes nothing, and
  neither does a hand edit of `project.ron`.
- **A bound choice:** bind the choice to a key, a channel or a `chan.dropdown`. When the
  binding gives another entry, the component writes that entry and its values one frame later.
  That frame still runs the old entry with the old values, so no frame mixes the two. The write
  makes no undo step, also works while the show is locked, and is saved with the project. A
  dropdown names the entry; a number picks it by place, 0 for the first. Only a change of the
  binding writes: opening a project writes nothing, and a choice set by hand stays until the
  binding gives another entry. A binding made anew writes its entry at once when it differs.
- **`node.set` with several names** sets the choice first: `node.set {path: "/rd", params:
  {preset: "maze", feed: 0.03}}` gives maze with `feed` at 0.03.
- **Reset** of a param a preset writes goes to the chosen entry's value: the row's reset
  button, or `null` in `node.set`.
- **A second choice with presets:** `outside` may have its own `@preset outside=* ...` lines.
  The main presets may write `outside`, and the values of the entry they pick follow. This
  reaches one level: the presets of `outside` cannot write a third choice that has presets.
- **Mistakes** cost their line or their value and show below the text. Examples: an entry the
  choice lacks, a name on an entry line but on no `*` line, a name given twice, a value its
  `@param` does not take, a param that the presets of two choices write. An inner param may
  refuse a value when the entry is chosen. That value is skipped, and the edit's reply names it
  under `left_out`.
- **A bound param among the values:** a preset writes its stored value, and the binding still
  decides the value in effect. `node.info` lists such params under `bound`.
- **`node.info`** lists `component.presets`: the entries, the params the presets own (`owns`),
  the chosen entry, `changed` (owned params whose value differs from the chosen entry's),
  `problems` (values an inner param would refuse now) and `bound`.

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
- **`@reset preset unless morph`, `@reset seed if fresh`:** the line counts only while the
  bool `@param` after `unless` is off, or the one after `if` is on. Turning the bool on or off
  starts nothing. One condition per line; write two lines for two.
- **A new preset decides alone:** when a choice with presets changes, only its own `@reset`
  line decides whether the component starts over. The params the preset writes in the same
  edit do not start it over on their own, even when an `@reset` line names them. Changed by
  hand, they start it over as usual. Choosing the chosen entry again is no change of the
  choice: when it brings back a value on an `@reset` line, that value starts it over.

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

