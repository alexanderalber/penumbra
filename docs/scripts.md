# Scripts

`script.python` runs a Python function each time the node cooks. Channels, tables and pictures
go in; channels, a table, a picture and a mesh come out. It is meant for logic (counting, state
machines, sorting a table, turning OSC into something useful) and for pictures and meshes that
numpy computes well: a height field, a data plot, a mesh from a formula. Per-pixel effects are
faster as `tex` nodes and [shaders](shaders.md).

## Python

The node needs Python 3.12 or newer, installed separately; nothing else in penumbra does.
penumbra looks for it in this order:

1. `PENUMBRA_PYTHON`: a Python directory, or a file in one.
2. The first directory on `PATH` that holds a usable Python.
3. The newest Python in the installer's default places (`%LOCALAPPDATA%\Programs\Python\Python3*`,
   `%ProgramFiles%\Python3*`).

Python loads only when the first script node cooks. Without a usable Python the node shows an
error saying what is missing; the rest of the app runs. `script.info` on the control channel
says which Python was found, or why none.

## A script

The script is the node's `code` param, or a `.py` file in the project named by `file` (which
then takes precedence and reloads when saved). It defines `cook(cx)`:

```python
PARAMS = {"gain": (1.0, 0.0, 2.0)}

def cook(cx):
    a = cx.chans[0]
    cx.out["value"] = (a.value() if a else 0.0) * cx.params["gain"]
```

What `cx` holds:

| | |
|---|---|
| `cx.chans[0]`, `cx.chans[1]` | the inputs `chan0`, `chan1`: a dict of channel name to a list of samples (the last is the value now), with `.rate` and `.value(name=None)`; `None` when unconnected |
| `cx.tables[0]`, `cx.tables[1]` | the inputs `table0`, `table1`: lists of rows of strings; `None` when unconnected |
| `cx.params` | the values of the params declared in `PARAMS` |
| `cx.time`, `cx.frame`, `cx.rate` | show time in seconds, frame number, frames per second |
| `cx.out[name] = ...` | a channel on the `out` output: a number, or a list of samples |
| `cx.out_rate` | the sample rate of `out` (default: the show rate) |
| `cx.table.append([...])` | a row on the `table` output; cells become strings, `None` empty |
| `cx.warn(text)` | a warning on the node |

Pictures and meshes are below.

Module globals keep their values between calls, so a script can count or remember; they start
over when the script reloads. The script's folder (the project's, for `code`) is on
`sys.path`, so it can import modules beside it. `print` goes to the app's log.

## Params

`PARAMS` declares params that appear in the panel under "from the script", set, bound, saved
and undone like any other:

```python
PARAMS = {
    "gain": 1.0,                 # a float
    "speed": (2.0, 0.0, 10.0),   # default, slider min, slider max
    "count": 3,                  # an int
    "on": True,                  # a bool
    "label": "x",                # text
    "mode": ["a", "b"],          # a menu; the first entry is the default
}
```

## Pictures and meshes

Pictures and meshes are numpy arrays. numpy must be installed in the Python penumbra uses
(`script.info` names it): `python -m pip install numpy` with that Python. Without numpy, a
script that uses them shows an error that says so; scripts without arrays still run.

**Out.** `cx.picture = array` puts a picture on the `picture` output. `cx.mesh = dict(...)` puts
a mesh on the `mesh` output. Both outputs keep the last picture or mesh until the script sets
another, so a script may update them only now and then. While nothing reads an output, the
array the script sets for it is ignored: not checked, not turned into a picture or mesh. The
script itself still runs.

| | |
|---|---|
| `cx.picture` | shape `(height, width)` (grey), `(height, width, 3)` (RGB) or `(height, width, 4)` (RGBA); row 0 is the top. `uint8` is 0 to 255, floats are 0 to 1 (above 1 is brighter than white) |
| `cx.mesh["positions"]` | shape `(n, 3)`: x, y, z in metres, z up |
| `cx.mesh["indices"]` | optional, shape `(m, 3)`, integers: three vertices per triangle. Without it, every three vertices make a triangle |
| `cx.mesh["normals"]` | optional, shape `(n, 3)`. Without it, vertices shared by several triangles get the average of their normals (smooth), others the triangle's own (flat) |
| `cx.mesh["uvs"]` | optional, shape `(n, 2)`: texture coordinates, 0 to 1, origin at the top left |

**In.** A picture wired to `tex0` or `tex1` arrives as `cx.pictures[0]` or `cx.pictures[1]`: a
float32 array of shape `(height, width, 4)`, RGBA. The array is read-only; `p.copy()` gives one
to change and put out again. The node reads a picture back from the GPU when the node before it
made a new one. For 512 by 512 pixels that adds about 4 milliseconds per new picture, most of it
waiting for the GPU to draw it. An unchanged picture costs nothing. Inputs are limited to 2048
by 2048 pixels: shrink a larger picture with `tex.resolution` or `tex.fit` first.

**Colour.** The node's `values` param says what the numbers mean. With `color` (the default),
`uint8` is sRGB like a PNG, floats are linear light, and alpha is straight (not multiplied in).
Pictures in come as linear floats with straight alpha; where alpha is 0, their colour is 0.
With `data`, every number is taken as stored, for maps such as heights, positions or masks, and
colour where alpha is 0 stays.

**Limits.** A picture out has at most 8192 pixels on a side and 8.4 million pixels (4K UHD
fits). A mesh has at most 1 048 576 vertices and 2 097 152 triangles. A wrong shape or type, a
NaN position or an index past the last vertex is an error on the node.

A picture of rings and a wavy sheet:

```python
import numpy as np

y, x = np.mgrid[0:256, 0:256] / 256 - 0.5
u, v = np.meshgrid(np.linspace(0, 1, 50), np.linspace(0, 1, 50))
i = np.arange(50 * 50).reshape(50, 50)
a, b, c, d = i[:-1, :-1], i[:-1, 1:], i[1:, :-1], i[1:, 1:]
cells = np.concatenate([np.stack([a, c, b], -1), np.stack([b, c, d], -1)]).reshape(-1, 3)

def cook(cx):
    rings = np.sin(40 * np.hypot(x, y) - 3 * cx.time) * 0.5 + 0.5
    cx.picture = (rings * 255).astype(np.uint8)
    z = 0.1 * np.sin(8 * u + cx.time)
    cx.mesh = dict(positions=np.stack([u - 0.5, 0.5 - v, z], -1).reshape(-1, 3),
                   indices=cells, uvs=np.stack([u, v], -1).reshape(-1, 2))
```

Wire `picture` into a viewer or a material's `texture`, and `mesh` into `geo.assign_material`
and `tex.render`. The [python-pictures](../examples/python-pictures/README.md) example does
both.

## When it runs

The `cook` param decides: `on change` runs the script when an input or a param changes,
`every frame` runs it every frame, `auto` (the default) runs it every frame if the code
mentions `time` or `frame` anywhere (also `import time`) and on change otherwise. A script that
cooks on change sees `cx.time` as `None`.

## Errors and limits

An error shows on the node, naming the script's line. A version that fails to load leaves the
last good one running. A call that runs longer than `timeout` (100 ms by default) is stopped,
and the node with it, until the code or the timeout changes.

There is no sandbox: a script can do anything Python can, including taking the app down
(`os._exit`, a crash in a C extension). A script stuck in C code or in `time.sleep` cannot be
stopped and holds up every other script until it returns. All scripts share one Python thread;
a call costs a few hundredths of a millisecond plus the script itself. A 512 by 512 picture
or a mesh of 10 000 vertices adds about 2 milliseconds per call.

