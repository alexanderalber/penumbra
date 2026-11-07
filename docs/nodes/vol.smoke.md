## Smoke with several emitters

Wire a `vol.emitter` into each `emitter` input, 64 emitters at most over all inputs. Inputs are
applied in order, and a later emitter acts on what the earlier ones left where they overlap. The
shipped example `smoke` has three, one of them moving: its `position` x is bound to a `chan.lfo`.

Keep an emitter at least 1 m from the open box edge behind it. Closer than that, air comes in
through the edge as a straight jet and leaves straight streaks. An emitter that moves and holds a
lot of `fuel` leaves thin streaks where it has been. Low `fuel` with a `correction_rate` of 2 keeps the rim clean, `advection: smooth` removes the streaks.

## Emitters from a table

Any table can be an emitter input, one emitter per row. Its first row names the columns. `x` and `y`
are required, every other column is optional:

| column | meaning when the column is missing |
|---|---|
| `x`, `y` | required: the emitter's centre, in the metres of the box |
| `z` | 0 |
| `type` | `sphere`; `box`; also 0 and 1 |
| `size_x`, `size_y`, `size_z` | 0.5 m each, for a box |
| `radius` | 0.3 m, for a sphere |
| `vx`, `vy`, `vz` | 0, 0.8 and 0 m/s |
| `smoke`, `temp`, `fuel`, `correction_rate` | 0.5, 2, 1, 0.5 |

A row with a cell that is not a finite number is left out. A `table.text` with commas between its
cells needs `format` set to `csv`. With its default, `lines`, every row is one cell.

## Driving emitters from a tracker

`tex.blob_track` gives `x` and `y` as shares of the picture: `x` 0 is the left edge, `y` 0 the top.
Emitters want metres, so put a `script.python` between them: `blobs` into `table0`, the node's
`table` output into `emitter0`, `cook` set to `every frame`.

```python
BOX_CENTER = (0.0, 0.0)
BOX_SIZE = (8.0, 4.5)

def cook(cx):
    cx.table.append(["x", "y", "radius", "vy", "smoke", "temp", "fuel"])
    rows = cx.tables[0] or []
    if len(rows) < 2:
        return
    ix, iy = rows[0].index("x"), rows[0].index("y")
    for row in rows[1:]:
        x = BOX_CENTER[0] + (float(row[ix]) - 0.5) * BOX_SIZE[0]
        y = BOX_CENTER[1] + (0.5 - float(row[iy])) * BOX_SIZE[1]
        y = max(y, BOX_CENTER[1] - BOX_SIZE[1] / 2 + 1.0)  # 1 m above the lower edge
        cx.table.append([x, y, 0.25, 1.0, 1.5, 2.5, 2.0])
```

Set `BOX_CENTER` and `BOX_SIZE` to the `center` and `size` of `vol.smoke`. The formula puts the
picture's edges on the box's edges along x and y.
