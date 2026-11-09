## A row of numbers per entry of a choice

A simulation's looks often need a few plain numbers each: the steps per frame for a
`tex.shader`'s `iterations`, a hold time for a `chan.clock`. Write them as a table with one row
per entry and the entry's name in the first cell. Bind the `chan.from_table`'s `key` to the
choice. The node then gives that row's numbers as channels, named by the header row.

- **By name.** The key matches the first cell, case and surrounding spaces aside. The rows may
  stand in any order, and an entry added to the choice moves no other row.
- **Same frame.** A new entry reaches every param bound to the channels in the frame of the
  change. Nothing runs on the GPU.
- **A missing row** gives the first row's numbers, and the node shows a warning that lists the
  keys the table has.
- The first column holds words only, so it makes no channel.

## Example

Inside a component `growth` with `@param look choice(window|snowflake|coral)` and
`@param speed float = 1 0..4`:

```
"/growth/look_table": (kind: "table.text", params: {"format": "csv", "text": "look,steps,hold\nwindow,12,5\nsnowflake,32,20\ncoral,16,5"}),
"/growth/look_values": (kind: "chan.from_table", inputs: {"in": "look_table"}, binds: {"key": "parent.look"}),
"/growth/steps": (kind: "chan.math", params: {"op": "multiply"}, inputs: {"a": "look_values"}, binds: {"b": "parent.speed"}),
"/growth/step": (kind: "tex.shader", ..., binds: {"iterations": "steps:steps"}),
"/growth/full_hold": (kind: "chan.clock", params: {"mode": "once"}, binds: {"length": "look_values:hold"}),
```

The same by command: `node.bind {param: "/growth/look_values.key", from: "parent.look"}`.
Outside a component, bind `key` to a `chan.dropdown`: a text param bound to a dropdown takes
the entry's name.
