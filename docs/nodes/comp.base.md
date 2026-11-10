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

## When a node inside fails

A component turns red when a node inside it is in error, at any depth: a red outline and a red
"!" beside its name. A shader that does not compile three levels down shows on every component
around it. The tooltip names the first such node by path, with its error, and says how many
more there are. Warnings inside do not mark the component. An input port nothing is wired to
does not either: the component shows that as an empty socket.

`node.info` of a component lists the nodes in error inside it as `errors_inside`, each with its
path and error. `node.list` gives a component the paths, and `node.list {"recursive": false,
"problems": true}` finds a failing component at the top level.

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
them) or which inner param they set (`step.feed`). A param nothing follows can still have a
job through the text, and the row says so:

- "writes 12": a choice with presets, whose entries write 12 params. The hover names them.
- "restart rule": a bool that an `@reset ... if` or `unless` line asks.
- "restarts": a param on an `@reset` line.
- "unused": neither a follower nor such a job. The param changes nothing.

`node.info` says the same in each row of `instance_params`: `followed_by`, `writes`,
`restart_condition`, `restarts` or `unused`.

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

A preset is a set of values for some of the component's params, named by an entry of a choice.
Choosing the entry writes those values into the params. After that the values are ordinary
values: tune a slider, and the tuned value is saved with the project. TouchDesigner's palette
has the presetManager for this job.

```text
@param preset choice(coral|maze) = coral -- the pattern
@param speed float = 1 0..4
@expose feed = step.feed
@expose kill = step.kill
@preset preset=* feed=0.0545 kill=0.062
@preset preset=maze feed=0.029 kill=0.057 -- a labyrinth
```

- **A line** reads `@preset <choice>=<entry> <name>=<value> ... -- doc`. The first pair names
  the choice and the entry; the pairs after it are values. Several lines for one entry add up.
  The doc after `--` shows on the entry in the choice's box and in its help. `@preset` lines
  may stand anywhere in the text, and take no `#` comment.
- **The `*` line** lists the params the presets write, each with the value every entry starts
  from. Choosing `maze` writes the `*` values, then maze's own. A param on no `*` line is never
  written: `speed` above stays where you put it. An entry without lines of its own gets the `*`
  values.
- **Values** read like an `@param` default: `0.03`, `true`, `[1, 0.5, 0, 1]`, an entry of a
  choice by name, a palette by name or colours. A value runs up to the next `name=`, so a vector
  may hold spaces. A preset may write the `@param` and `@expose` names of the text, but not its
  own choice.
- **Choosing** writes the values in the same edit, and one undo takes all of them back. This
  holds for the panel, `node.set`, `node.batch` and OSC. Choosing the chosen entry again writes
  its values again. Opening a project writes nothing, and neither does a hand edit of
  `project.ron`.
- **The panel** marks the choice's row "writes 2" at its right end when nothing inside
  follows the choice itself; its hover names the params. A bool that only an `@reset` line asks
  (`morph` below) says "restart rule".
  The panel shows a quiet line under the choice while params differ from the chosen entry:
  `feed, kill changed · back`. "back" writes the entry's values again, as one undo step. More
  than three names end in a count, such as `feed, kill, steps +2`. `node.info` lists them all
  under `changed`.
- **Reset** of a param a preset writes goes to the chosen entry's value: the row's reset
  button, or `null` in `node.set`.
- **`node.set` with several names** sets the choice first: `node.set {path: "/rd", params:
  {preset: "maze", feed: 0.03}}` gives maze with `feed` at 0.03.
- **A second choice with presets:** `outside` may have its own `@preset outside=* ...` lines.
  The main presets may write `outside`, and the values of the entry they pick follow. This
  reaches one level: the presets of `outside` cannot write a third choice that has presets. Two
  choices may not write the same param.
- **A bound choice:** bind the choice to a key, a channel or a `chan.dropdown`. When the
  binding gives another entry, the component writes that entry and its values one frame later.
  That frame still runs the old entry, so no frame mixes the two. The write makes no undo step,
  works while the show is locked, and is saved with the project. A dropdown names the entry; a
  number picks it by place, 0 for the first. A new binding writes its entry at once. After that
  only a change of the binding writes. So after opening a project, or after a choice by hand,
  the stored entry stays in effect until the binding changes. The row then shows that entry and
  says `binding says maze`.
- **A bound param among the values:** a preset writes its stored value, and the binding decides
  the value in effect. `node.info` lists such params under `bound`.
- **Starting over** on a new preset: `@reset preset`, or `@reset preset unless morph`. See
  "Starting over" below.
- **Mistakes** cost their line or their value and show below the text. Examples: an entry the
  choice lacks, a name on an entry line but on no `*` line, a name given twice, a value its
  `@param` does not take. An inner param (a shader's) is asked only when the entry is chosen:
  a value it refuses is skipped, and the edit's reply names it under `left_out`. The panel lists
  such values under the rows, `node.info` under `problems`.
- **Before saving a component** to the library, choose its entry once. Otherwise its params
  hold their declared defaults, and the component opens with them listed as changed.

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

