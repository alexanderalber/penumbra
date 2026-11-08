"""penumbra's side of the script node, inside the embedded Python.

Holds one module per script node, builds the `cx` object a script's `cook(cx)` sees, and turns
JSON requests from penumbra into calls and the results back into JSON. Runs on penumbra's
Python thread only. Not importable by scripts; nothing here is a public API except what `Cx`
and `Chans` offer.
"""

import io
import json
import math
import sys
import threading
import traceback
import types

# Embedded interpreters have no argv; some libraries expect one.
if not hasattr(sys, "argv"):
    sys.argv = ["penumbra"]


class Timeout(BaseException):
    """Raised into a script by penumbra's watchdog when it runs longer than its node allows.

    A BaseException, so `except Exception` in a script does not swallow it."""


# key -> (module, cook function, file label)
_modules = {}
# key -> generation of the newest load seen
_generations = {}

# Longest printed text returned per call; the rest is cut from the front.
_MAX_PRINTED = 4000


def thread_id():
    return threading.get_ident()


def info():
    return json.dumps(
        {
            "version": sys.version.split()[0],
            "full_version": sys.version,
            "prefix": sys.prefix,
            # Called on the thread that runs every request: the watchdog's target.
            "thread": threading.get_ident(),
        }
    )


class Chans(dict):
    """The channels of one chan input: name -> list of samples (floats), the last sample being
    the value now. `rate` is the sample rate in Hz."""

    def __init__(self, rate, items):
        super().__init__(items)
        self.rate = rate

    def value(self, name=None):
        """The value now of channel `name`, or of the first channel; 0.0 when there is none."""
        if name is None:
            samples = next(iter(self.values()), None)
        else:
            samples = self.get(name)
        if not samples:
            return 0.0
        return samples[-1]


class Cx:
    """What `cook(cx)` sees. Inputs: `chans[0]`, `chans[1]` (Chans or None), `tables[0]`,
    `tables[1]` (lists of rows of strings, or None), `pictures[0]`, `pictures[1]` (numpy arrays
    of shape (height, width, 4), float32, or None), `params` (the values of PARAMS), `rate` (show
    frames per second), `time` (seconds) and `frame` (show frame number). Outputs: `out`
    (channel name -> number or list of numbers), `out_rate` (samples per second of `out`; None
    for the show rate), `table` (list of rows), `picture` (an array of shape (height, width),
    (height, width, 3) or (height, width, 4), uint8 or float), `mesh` (a dict of arrays:
    `positions`, optionally `indices`, `normals`, `uvs`). `warn(text)` shows a warning on the
    node."""

    __slots__ = (
        "chans",
        "tables",
        "pictures",
        "params",
        "rate",
        "_time",
        "_frame",
        "out",
        "out_rate",
        "table",
        "picture",
        "mesh",
        "_warnings",
    )

    def __init__(self, req, blob):
        self.chans = [_chans(c) for c in req.get("chans", [])]
        self.tables = [None if t is None else [list(r) for r in t] for t in req.get("tables", [])]
        self.pictures = [_picture_in(p, blob) for p in req.get("pictures", [])]
        self.params = dict(req.get("params", {}))
        self.rate = req.get("rate", 60.0)
        self._time = req.get("time")
        self._frame = req.get("frame")
        self.out = {}
        self.out_rate = None
        self.table = []
        self.picture = None
        self.mesh = None
        self._warnings = []

    @property
    def time(self):
        return self._time

    @property
    def frame(self):
        return self._frame

    def warn(self, text):
        self._warnings.append(str(text))


def _chans(c):
    if c is None:
        return None
    return Chans(c["rate"], zip(c["names"], c["samples"]))


# Caps, as penumbra checks them again (crates/script/src/protocol.rs).
_MAX_PICTURE_SIDE = 8192
_MAX_PICTURE_PIXELS = 1 << 23
_MAX_MESH_VERTICES = 1 << 20
_MAX_MESH_TRIANGLES = 1 << 21


class NumpyMissing(Exception):
    pass


def _numpy_hint():
    exe = "python" if not sys.prefix else sys.prefix.rstrip("\\/") + ("\\python.exe" if sys.platform == "win32" else "/bin/python3")
    return (
        "numpy is not installed in the Python penumbra uses (%s); pictures and meshes need it. "
        "Install it with: \"%s\" -m pip install numpy" % (sys.prefix, exe)
    )


def _numpy():
    try:
        import numpy
    except ImportError:
        raise NumpyMissing(_numpy_hint()) from None
    return numpy


def _picture_in(p, blob):
    """An input picture: a writable float32 array of shape (height, width, 4) over its part of
    the request's bytes."""
    if p is None:
        return None
    np = _numpy()
    w, h, offset = p["width"], p["height"], p["offset"]
    a = np.frombuffer(blob, dtype="<f4", count=w * h * 4, offset=offset)
    return a.reshape(h, w, 4)


def _out_picture(cx, parts):
    """`cx.picture` as JSON describing it, its samples appended to `parts`; None when unset."""
    a = cx.picture
    if a is None:
        return None
    np = _numpy()
    a = np.asarray(a)
    if a.ndim == 2:
        channels = 1
    elif a.ndim == 3 and a.shape[2] in (1, 3, 4):
        channels = a.shape[2]
    else:
        raise ValueError(
            "cx.picture: shape %s; give (height, width), (height, width, 3) or (height, width, 4)"
            % (tuple(a.shape),)
        )
    h, w = a.shape[0], a.shape[1]
    if h == 0 or w == 0:
        raise ValueError("cx.picture: shape %s is empty" % (tuple(a.shape),))
    if w > _MAX_PICTURE_SIDE or h > _MAX_PICTURE_SIDE or w * h > _MAX_PICTURE_PIXELS:
        raise ValueError(
            "cx.picture: %d x %d pixels is too large (at most %d on a side and %d pixels)"
            % (w, h, _MAX_PICTURE_SIDE, _MAX_PICTURE_PIXELS)
        )
    if a.dtype == np.uint8:
        dtype = "u8"
    elif a.dtype == np.bool_:
        a = a.astype(np.uint8) * np.uint8(255)
        dtype = "u8"
    elif a.dtype.kind == "f":
        a = a.astype("<f4", copy=False)
        dtype = "f32"
    else:
        raise ValueError("cx.picture: dtype %s; give uint8 (0 to 255) or float (0 to 1)" % a.dtype)
    parts.append(np.ascontiguousarray(a).tobytes())
    return {"width": w, "height": h, "channels": channels, "dtype": dtype, "part": len(parts) - 1}


def _mesh_array(np, m, key, dtype, columns, rows=None):
    v = m.get(key)
    if v is None:
        return None
    try:
        a = np.asarray(v, dtype=dtype)
    except (TypeError, ValueError) as e:
        raise ValueError("cx.mesh[%r]: %s" % (key, e)) from None
    if columns is not None and (a.ndim != 2 or a.shape[1] != columns):
        raise ValueError("cx.mesh[%r]: shape %s; give (n, %d)" % (key, tuple(a.shape), columns))
    if rows is not None and a.shape[0] != rows:
        raise ValueError(
            "cx.mesh[%r]: %d rows for %d positions; give one per vertex" % (key, a.shape[0], rows)
        )
    return a


_MESH_KEYS = ("positions", "indices", "normals", "uvs")


def _out_mesh(cx, parts):
    """`cx.mesh` as JSON describing it, its arrays appended to `parts`; None when unset."""
    m = cx.mesh
    if m is None:
        return None
    if not isinstance(m, dict):
        raise ValueError("cx.mesh: give a dict with `positions`, optionally `indices`, `normals`, `uvs`")
    unknown = [k for k in m if k not in _MESH_KEYS]
    if unknown:
        raise ValueError("cx.mesh: unknown key %r; the keys are %s" % (unknown[0], ", ".join(_MESH_KEYS)))
    if m.get("positions") is None:
        raise ValueError("cx.mesh: `positions` is missing")
    np = _numpy()
    pos = _mesh_array(np, m, "positions", "<f4", 3)
    n = pos.shape[0]
    if n == 0:
        raise ValueError("cx.mesh['positions']: no vertices")
    if n > _MAX_MESH_VERTICES:
        raise ValueError("cx.mesh: %d vertices is too many (at most %d)" % (n, _MAX_MESH_VERTICES))
    if not np.isfinite(pos).all():
        raise ValueError("cx.mesh['positions']: a position is NaN or infinite")
    out = {"vertices": n}
    parts.append(np.ascontiguousarray(pos).tobytes())
    out["positions"] = len(parts) - 1
    idx = m.get("indices")
    if idx is not None:
        idx = np.asarray(idx)
        if idx.dtype.kind not in "iu":
            raise ValueError("cx.mesh['indices']: dtype %s; give integers" % idx.dtype)
        if not (idx.ndim == 2 and idx.shape[1] == 3) and not (idx.ndim == 1 and idx.size % 3 == 0):
            raise ValueError("cx.mesh['indices']: shape %s; give (m, 3)" % (tuple(idx.shape),))
        if idx.size == 0:
            raise ValueError("cx.mesh['indices']: no triangles")
        if idx.size // 3 > _MAX_MESH_TRIANGLES:
            raise ValueError(
                "cx.mesh: %d triangles is too many (at most %d)" % (idx.size // 3, _MAX_MESH_TRIANGLES)
            )
        lo, hi = int(idx.min()), int(idx.max())
        if lo < 0:
            raise ValueError("cx.mesh['indices']: index %d is negative" % lo)
        if hi >= n:
            raise ValueError("cx.mesh['indices']: index %d is past the last vertex (%d)" % (hi, n - 1))
        parts.append(np.ascontiguousarray(idx, dtype="<u4").tobytes())
        out["indices"] = len(parts) - 1
    elif n % 3:
        raise ValueError(
            "cx.mesh: %d vertices without indices; every three vertices make a triangle, so give a multiple of 3" % n
        )
    for key, columns in (("normals", 3), ("uvs", 2)):
        a = _mesh_array(np, m, key, "<f4", columns, rows=n)
        if a is not None:
            parts.append(np.ascontiguousarray(a).tobytes())
            out[key] = len(parts) - 1
    return out


def _uses_time(code):
    """Whether any code in the module names `time` or `frame` (as `cx.time`, or the `time`
    module): such a node recooks every frame. Static, like the shader node's check."""
    names = set()
    stack = [code]
    while stack:
        c = stack.pop()
        names.update(c.co_names)
        stack.extend(k for k in c.co_consts if isinstance(k, types.CodeType))
    return "time" in names or "frame" in names


def _param(name, spec):
    def kind_of(v):
        if isinstance(v, bool):
            return "bool"
        if isinstance(v, int):
            return "int"
        if isinstance(v, float):
            return "float"
        if isinstance(v, str):
            return "text"
        return None

    where = "PARAMS[%r]" % (name,)
    if not isinstance(name, str):
        raise ValueError("%s: names are strings" % where)
    if isinstance(spec, (list, tuple)) and spec and all(isinstance(s, str) for s in spec):
        return {"name": name, "kind": "choice", "default": spec[0], "choices": list(spec)}
    if isinstance(spec, tuple):
        if len(spec) != 3 or any(kind_of(v) not in ("int", "float") for v in spec):
            raise ValueError("%s: a tuple is (default, min, max), all numbers" % where)
        kind = "int" if all(kind_of(v) == "int" for v in spec) else "float"
        default = spec[0] if kind == "int" else float(spec[0])
        return {"name": name, "kind": kind, "default": default, "min": float(spec[1]), "max": float(spec[2])}
    kind = kind_of(spec)
    if kind is None:
        raise ValueError(
            "%s: use a bool, int, float, str, a list of str (a menu) or (default, min, max)" % where
        )
    return {"name": name, "kind": kind, "default": spec}


def _describe(e, label):
    """One line naming the script's line where the error happened, then the error."""
    if isinstance(e, NumpyMissing):
        return str(e)
    if isinstance(e, ImportError) and getattr(e, "name", None) == "numpy":
        return _numpy_hint()
    if isinstance(e, SyntaxError) and e.filename == label:
        return "%s, line %s: SyntaxError: %s" % (label, e.lineno, e.msg)
    frames = [f for f in traceback.extract_tb(e.__traceback__) if f.filename == label]
    where = ""
    if frames:
        where = "%s, line %s, in %s: " % (label, frames[-1].lineno, frames[-1].name)
    text = str(e)
    return "%s%s%s" % (where, type(e).__name__, ": " + text if text else "")


def _number(v):
    f = float(v)
    # Non-finite numbers become 0, as everywhere in penumbra's channels.
    return f if math.isfinite(f) else 0.0


def _out_chans(cx):
    names = []
    samples = []
    for name, v in cx.out.items():
        names.append(str(name))
        if isinstance(v, (int, float)):
            samples.append([_number(v)])
        else:
            samples.append([_number(x) for x in v])
    lengths = {len(s) for s in samples if len(s) != 1}
    if len(lengths) > 1:
        raise ValueError("cx.out: lists of different lengths (%s); all channels share one" % sorted(lengths))
    n = lengths.pop() if lengths else 1
    if n == 0:
        raise ValueError("cx.out: a channel needs at least one sample")
    # A single number goes with lists of n samples as n equal samples.
    samples = [s * n if len(s) == 1 else s for s in samples]
    rate = None if cx.out_rate is None else _number(cx.out_rate)
    return names, samples, rate


def _cell(c):
    return "" if c is None else str(c)


def _out_table(cx):
    rows = []
    for r in cx.table:
        if isinstance(r, str):
            rows.append([r])
        else:
            rows.append([_cell(c) for c in r])
    return rows


class _Capture:
    """Collects what a script prints."""

    def __enter__(self):
        self.buf = io.StringIO()
        self.saved = (sys.stdout, sys.stderr)
        sys.stdout = sys.stderr = self.buf
        return self

    def __exit__(self, *exc):
        sys.stdout, sys.stderr = self.saved
        return False

    def text(self):
        t = self.buf.getvalue()
        return t[-_MAX_PRINTED:]


def _load(req):
    key = req["key"]
    label = req["file"]
    generation = req.get("generation", 0)
    if generation < _generations.get(key, -1):
        return {"ok": False, "error": "superseded by a newer version"}
    _generations[key] = generation
    folder = req.get("folder")
    if folder and folder not in sys.path:
        sys.path.insert(0, folder)
    with _Capture() as cap:
        try:
            code = compile(req["source"], label, "exec")
            module = types.ModuleType("penumbra_script_%d" % key)
            module.__file__ = label
            exec(code, module.__dict__)
            cook = module.__dict__.get("cook")
            if not callable(cook):
                raise ValueError("the script defines no function cook(cx)")
            spec = module.__dict__.get("PARAMS", {})
            if not isinstance(spec, dict):
                raise ValueError("PARAMS must be a dict")
            params = [_param(n, s) for n, s in spec.items()]
        except Timeout:
            return {"ok": False, "timeout": True, "error": "loading took too long", "printed": cap.text()}
        except BaseException as e:
            return {"ok": False, "error": _describe(e, label), "printed": cap.text()}
    _modules[key] = (module, cook, label)
    return {"ok": True, "params": params, "uses_time": _uses_time(code), "printed": cap.text()}


def _call(req, blob, parts):
    entry = _modules.get(req["key"])
    if entry is None:
        return {"ok": False, "missing": True, "error": "the script is not loaded"}
    _module, cook, label = entry
    with _Capture() as cap:
        try:
            cx = Cx(req, blob)
            cook(cx)
            names, samples, rate = _out_chans(cx)
            table = _out_table(cx)
            picture = _out_picture(cx, parts) if req.get("want_picture") else None
            mesh = _out_mesh(cx, parts) if req.get("want_mesh") else None
        except Timeout:
            del parts[:]
            return {"ok": False, "timeout": True, "error": "stopped", "printed": cap.text()}
        except BaseException as e:
            del parts[:]
            return {"ok": False, "error": _describe(e, label), "printed": cap.text()}
    return {
        "ok": True,
        "names": names,
        "samples": samples,
        "rate": rate,
        "table": table,
        "picture": picture,
        "mesh": mesh,
        "warnings": cx._warnings,
        "printed": cap.text(),
    }


def handle(text, blob=None):
    """One request from penumbra, as JSON text, and the binary data it refers to (a bytearray
    or None); returns the reply as JSON text, or a tuple of it and a list of bytes (the
    picture's samples, the mesh's arrays) the reply names by index."""
    try:
        req = json.loads(text)
        for key in req.get("drops", ()):
            _modules.pop(key, None)
            _generations.pop(key, None)
        op = req.get("op")
        parts = []
        if op == "call":
            reply = _call(req, blob, parts)
        elif op == "load":
            reply = _load(req)
        elif op == "ping":
            reply = {"ok": True, "modules": len(_modules)}
        else:
            reply = {"ok": False, "error": "unknown request %r" % (op,)}
        if parts:
            return (json.dumps(reply), parts)
        return json.dumps(reply)

    except Timeout:
        return json.dumps({"ok": False, "timeout": True, "error": "stopped"})
    except BaseException as e:
        return json.dumps({"ok": False, "error": "penumbra bootstrap: %s: %s" % (type(e).__name__, e)})
