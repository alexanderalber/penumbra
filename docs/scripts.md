# Scripts

`script.python` runs a Python function each time the node cooks: channels and tables in,
channels and a table out. It is meant for logic (counting, state machines, sorting a table,
turning OSC into something useful), not for pixels; pictures are the job of `tex` nodes and
[shaders](shaders.md).

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
a call costs a few hundredths of a millisecond plus the script itself.
