"""Dump a TouchDesigner project (.toe or .tox) as readable text, for porting it by hand.

TouchDesigner saves a project as one binary file. Its own tool `toeexpand` (in TouchDesigner's
`bin` folder) turns that file into a folder of small text files, one set per operator. This
script runs toeexpand on a copy of the project, unpacks the components saved "load on demand"
(`.lod` files), and writes one text file that lists every operator: its type, its inputs, the
params that differ from their defaults, expressions, exports, and the text of DATs (GLSL,
Python, tables).

Usage:
    python td_dump.py PROJECT.toe            expand a copy and dump it
    python td_dump.py PROJECT.toe.dir        dump a folder toeexpand already made
    options:
      -o FILE          where the dump goes (default: dump.txt in the work folder)
      --work DIR       folder for the copy and the expanded files
                       (default: %TEMP%\\td_dump\\<name>)
      --all            also show the insides of TouchDesigner's palette components and
                       cameras and lights, and every texture sampling param
      --text N         longest DAT text shown in full, in characters (default 4000)
      --toeexpand EXE  toeexpand.exe to use (default: found as described below)

toeexpand is looked for in this order: --toeexpand, the TOEEXPAND environment variable,
PATH, the install folder TouchDesigner writes to the registry, and
C:\\Program Files\\Derivative\\TouchDesigner*\\bin. Without arguments the script prints this
text and which toeexpand it found.

Standard library only; Python 3.8 or newer.
"""
import argparse
import glob
import os
import shutil
import struct
import subprocess
import sys
import tempfile

# Top-level folders that hold TouchDesigner's own state, not the project's network.
TD_INTERNAL = {"local", "perform", "sys", "ui"}
# Params that only record the editor's state.
NOISE_PARAMS = {"pageindex", "pageindex1", "pageindex2", "pageindex3", "opviewer"}
# How a node samples the textures it reads (map edges, filtering): saved for every map slot
# whether used or not, so left out unless --all.
SAMPLING_SUFFIXES = ("extendu", "extendv", "extendw", "anisotropy", "mapfilter", "texfilter")
# Components whose insides are only the shape TouchDesigner draws for them in the viewport.
GIZMO_COMPS = {"COMP:cam", "COMP:light", "COMP:ambient", "COMP:environment"}
# Node flags that change what a node does.
FLAGS_SHOWN = {"display", "render", "bypass", "lock", "export", "activate"}
FILE_EXTENSIONS = (
    ".png", ".jpg", ".jpeg", ".tif", ".tiff", ".exr", ".hdr", ".bmp", ".gif", ".tga",
    ".mov", ".mp4", ".avi", ".mkv", ".webm", ".m4v", ".wav", ".mp3", ".aif", ".aiff",
    ".flac", ".ogg", ".obj", ".fbx", ".abc", ".usd", ".usdz", ".glb", ".gltf", ".svg",
    ".csv", ".txt", ".json", ".xml", ".py", ".glsl", ".frag", ".vert", ".tox", ".ttf",
    ".otf", ".mid", ".dmx",
)


# ---- finding and running toeexpand -------------------------------------------------------

def registry_install_dirs():
    try:
        import winreg
    except ImportError:
        return []
    dirs = []
    for view in (0, getattr(winreg, "KEY_WOW64_64KEY", 0)):
        try:
            key = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Derivative\TouchDesigner",
                                 0, winreg.KEY_READ | view)
            value, _ = winreg.QueryValueEx(key, "Path")
            winreg.CloseKey(key)
            if value and value not in dirs:
                dirs.append(value)
        except OSError:
            pass
    return dirs


def find_toeexpand(explicit=None):
    candidates = []
    if explicit:
        candidates.append(explicit)
    if os.environ.get("TOEEXPAND"):
        candidates.append(os.environ["TOEEXPAND"])
    on_path = shutil.which("toeexpand")
    if on_path:
        candidates.append(on_path)
    for d in registry_install_dirs():
        candidates.append(os.path.join(d, "bin", "toeexpand.exe"))
    pattern = os.path.join(os.environ.get("ProgramFiles", r"C:\Program Files"),
                           "Derivative", "TouchDesigner*", "bin", "toeexpand.exe")
    candidates.extend(sorted(glob.glob(pattern), reverse=True))
    for c in candidates:
        if os.path.isfile(c):
            return c
    return None


def expand(project, work, toeexpand):
    os.makedirs(work, exist_ok=True)
    name = os.path.basename(project)
    copy = os.path.join(work, name)
    shutil.copy2(project, copy)
    out_dir = copy + ".dir"
    if os.path.isdir(out_dir):
        shutil.rmtree(out_dir)
    # toeexpand writes NAME.dir and NAME.toc beside its input and exits with 1 even when it
    # worked, so success is judged by the folder it leaves.
    result = subprocess.run([toeexpand, name], cwd=work, capture_output=True, text=True)
    if not os.path.isdir(out_dir):
        raise RuntimeError(f"toeexpand made no folder.\n{result.stdout}\n{result.stderr}")
    return out_dir


# ---- load-on-demand components -----------------------------------------------------------

def lod_entries(b):
    """A .lod holds the component's files. Byte 0x34, u8 name length, u32 big-endian size,
    the NUL-padded name, the content: one file. Bytes 0x36 0x34, u8 name length, name: a nested
    component starts, its files follow until a byte 0x35."""
    i, path = 0, []
    while i < len(b):
        tag = b[i]
        if tag == 0x36 and i + 2 < len(b) and b[i + 1] == 0x34:
            n = b[i + 2]
            path.append(b[i + 3:i + 3 + n].rstrip(b"\0").decode("utf-8", "replace"))
            i += 3 + n
            continue
        if tag == 0x35:
            if path:
                path.pop()
            i += 1
            continue
        if tag != 0x34:
            raise ValueError(f"unknown tag {tag:#x} at byte {i}")
        n = b[i + 1]
        (size,) = struct.unpack_from(">I", b, i + 2)
        j = i + 6
        name = b[j:j + n].rstrip(b"\0").decode("utf-8", "replace")
        j += n
        yield "/".join(path + [name]), b[j:j + size]
        i = j + size


def unpack_lods(root):
    notes = []
    for d, _, files in os.walk(root):
        for f in files:
            if not f.endswith(".lod"):
                continue
            lod = os.path.join(d, f)
            out = lod[:-4]
            if os.path.isdir(out):
                continue
            try:
                with open(lod, "rb") as fh:
                    data = fh.read()
                count = 0
                for name, content in lod_entries(data):
                    if name in ("", ".build", ".toc"):
                        continue
                    dest = os.path.normpath(os.path.join(out, name))
                    if not dest.startswith(os.path.normpath(out) + os.sep):
                        raise ValueError(f"entry leaves the folder: {name}")
                    os.makedirs(os.path.dirname(dest), exist_ok=True)
                    with open(dest, "wb") as fh:
                        fh.write(content)
                    count += 1
                notes.append(f"unpacked {os.path.relpath(lod, root)}: {count} files")
            except (ValueError, IndexError, struct.error) as e:
                # Some of TouchDesigner's own components use another layout.
                notes.append(f"could not unpack {os.path.relpath(lod, root)}: {e}")
    return notes


# ---- reading one operator ----------------------------------------------------------------

def read_text(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def block(text, name):
    """Lines between `name`, `{` and `}` in a .n or .network file."""
    lines = text.splitlines()
    for k, line in enumerate(lines):
        if line.strip() == name and k + 1 < len(lines) and lines[k + 1].strip() == "{":
            out = []
            for l in lines[k + 2:]:
                if l.strip() == "}":
                    return out
                out.append(l)
    return []


def parse_flags(n_text):
    for line in n_text.splitlines():
        if line.startswith("flags"):
            toks = line.split("=", 1)[-1].split()
            pairs = dict(zip(toks[0::2], toks[1::2]))
            return [k for k, v in pairs.items() if k in FLAGS_SHOWN and v == "on"]
    return []


def split_first(rest):
    """The first token of a param value (quoted or bare) and what follows it."""
    rest = rest.strip()
    if rest.startswith('"'):
        end = rest.find('"', 1)
        while end > 0 and rest[end - 1] == "\\":
            end = rest.find('"', end + 1)
        if end > 0:
            return rest[:end + 1], rest[end + 1:].strip()
    parts = rest.split(None, 1)
    return (parts[0], parts[1] if len(parts) > 1 else "") if parts else ("", "")


def parse_param(line):
    """One .parm line: name, a bit field, the constant value, maybe an expression.

    Bits as observed: 1 the param follows its expression (the stored value is then only the
    constant it falls back to, not what the expression gave), 16 an expression is stored,
    2 a CHOP export drives the param, 64 a custom param."""
    parts = line.split(None, 2)
    if len(parts) < 2 or not parts[1].lstrip("-").isdigit():
        return None
    name, bits = parts[0], int(parts[1])
    rest = parts[2] if len(parts) > 2 else ""
    if bits & 16:
        value, expr = split_first(rest)
    else:
        value, expr = rest.strip(), ""
    return name, bits, value, expr


def format_param(name, bits, value, expr):
    if expr and bits & 1:
        s = f"{name} = `{expr}`   (expression; constant when off: {value})"
    elif expr:
        s = f"{name} = {value}   (expression stored but off: `{expr}`)"
    else:
        s = f"{name} = {value}"
    if bits & 2:
        s += "   (driven by a CHOP export)"
    return s


def decode_table(path):
    """A .table or .text: a line with a number, `*`, four big-endian u32 (1, rows, cols, and
    one more), then per cell u32 type, u32 length, the bytes. A .text is one cell. Returns
    rows of strings, or None when the layout differs."""
    try:
        with open(path, "rb") as f:
            b = f.read()
        i = b.index(b"*") + 1
        if not b[:i - 1].strip().isdigit():
            return None
        _, rows, cols, _ = struct.unpack(">IIII", b[i:i + 16])
        i += 16
        if rows * cols > 1_000_000:
            return None
        out = []
        for _ in range(rows):
            row = []
            for _ in range(cols):
                _, n = struct.unpack(">II", b[i:i + 8])
                i += 8
                row.append(b[i:i + n].decode("utf-8", "replace"))
                i += n
            out.append(row)
        return out
    except (ValueError, struct.error, OSError):
        return None


def is_palette_component(dirpath):
    return os.path.isfile(os.path.join(dirpath, "docsHelper.n"))


def count_nodes(dirpath):
    return sum(1 for _, _, fs in os.walk(dirpath) for f in fs if f.endswith(".n"))


# ---- the dump ----------------------------------------------------------------------------

class Dump:
    def __init__(self, root, show_all, max_text):
        self.root = root
        self.show_all = show_all
        self.max_text = max_text
        self.lines = []
        self.types = {}
        self.texts = []
        self.files = set()
        self.expressions = 0
        self.palettes = []

    def out(self, s=""):
        self.lines.append(s)

    def node(self, dirpath, name, path, depth):
        pad = "  " * depth
        base = os.path.join(dirpath, name)
        n_text = read_text(base + ".n")
        typ = n_text.splitlines()[0].strip() if n_text.strip() else "?"
        self.types[typ] = self.types.get(typ, 0) + 1
        flags = parse_flags(n_text)
        head = f"{pad}{path} [{typ}]"
        if flags:
            head += "  " + " ".join(flags)
        self.out(head)
        sub = pad + "    "

        inputs = [" ".join(l.split()) for l in block(n_text, "inputs") if l.strip()]
        if inputs:
            self.out(f"{sub}in: " + ", ".join(inputs))
        comp_in = [l.strip() for l in block(read_text(base + ".network"), "compinputs")]
        for k in range(0, len(comp_in) - 2, 3):
            src = " ".join(comp_in[k].split())
            self.out(f"{sub}in: {src} -> inside {comp_in[k + 1]} ({comp_in[k + 2]})")
        exports = [l.strip() for l in block(n_text, "exports") if l.strip()]
        if exports:
            self.out(f"{sub}params driven by exports from: " + ", ".join(exports))

        for line in read_text(base + ".parm").splitlines():
            p = parse_param(line)
            if not p or p[0] in NOISE_PARAMS:
                continue
            if not self.show_all and p[0].endswith(SAMPLING_SUFFIXES):
                continue
            if p[3] and p[1] & 1:
                self.expressions += 1
            value = p[2].strip('"')
            if value.lower().endswith(FILE_EXTENSIONS):
                self.files.add(value)
            self.out(sub + format_param(*p)[:400])

        custom = []
        for line in read_text(base + ".cparm").splitlines():
            parts = line.split(None, 2)
            if len(parts) == 3 and parts[0].lstrip("-").isdigit():
                label, _ = split_first(parts[2])
                custom.append(f"{parts[1]} {label}")
        if custom:
            self.out(f"{sub}custom params: " + ", ".join(custom))

        text = ""
        if os.path.isfile(base + ".text"):
            cells = decode_table(base + ".text")
            text = "\n".join(c for row in cells for c in row) if cells else read_text(base + ".text")
        if text.strip():
            self.texts.append(f"{path} ({len(text.splitlines())} lines)")
            body = text if len(text) <= self.max_text else text[:self.max_text] + (
                f"\n... [{len(text)} characters, cut; the full text is {base}.text]")
            self.out(f"{sub}text:")
            for l in body.splitlines():
                self.out(f"{sub}  | {l}")
        if os.path.isfile(base + ".table"):
            rows = decode_table(base + ".table")
            if rows is None:
                self.out(f"{sub}table: (not decoded) {base}.table")
            else:
                self.out(f"{sub}table ({len(rows)} rows):")
                for row in rows[:60]:
                    self.out(f"{sub}  | " + "\t".join(row))
                if len(rows) > 60:
                    self.out(f"{sub}  | ... {len(rows) - 60} more rows")
        for ext in (".data", ".chop"):
            # TouchDesigner also saves a cache file for some operators that are not locked.
            if "lock" in flags and os.path.isfile(base + ext):
                size = os.path.getsize(base + ext)
                self.out(f"{sub}locked {ext[1:]} ({size} bytes): {base}{ext}")

        if os.path.isdir(base):
            if typ in GIZMO_COMPS and not self.show_all:
                self.out(f"{sub}(the viewport gizmo inside is not shown)")
            elif is_palette_component(base) and not self.show_all:
                count = count_nodes(base)
                self.palettes.append(f"{path} ({count} operators)")
                self.out(f"{sub}palette component: {count} operators inside, not shown "
                         "(--all shows them)")
            else:
                self.folder(base, path, depth + 1)

    def folder(self, dirpath, path, depth):
        names = sorted({f[:-2] for f in os.listdir(dirpath) if f.endswith(".n")}, key=str.lower)
        for name in names:
            self.node(dirpath, name, f"{path}/{name}", depth)

    def run(self):
        top = sorted({f[:-2] for f in os.listdir(self.root) if f.endswith(".n")}, key=str.lower)
        for name in top:
            if name in TD_INTERNAL:
                continue
            self.node(self.root, name, "/" + name, 0)
        time_parm = os.path.join(self.root, "local", "time.parm")
        if os.path.isfile(time_parm):
            self.out("")
            self.out("/local/time (frame rate, tempo, timeline)")
            for line in read_text(time_parm).splitlines():
                p = parse_param(line)
                if p and p[0] not in NOISE_PARAMS and p[0] != "clone":
                    self.out("    " + format_param(*p))

    def summary(self):
        s = []
        build = read_text(os.path.join(self.root, ".build")).strip().splitlines()
        if build:
            s.append("saved by TouchDesigner " + "; ".join(build[:3]))
        total = sum(self.types.values())
        s.append(f"{total} operators shown, {self.expressions} params following an expression")
        s.append("operator types:")
        for typ, n in sorted(self.types.items(), key=lambda kv: (-kv[1], kv[0])):
            s.append(f"  {n:5d}  {typ}")
        if self.texts:
            s.append("DATs with text (shaders, scripts, notes):")
            s.extend("  " + t for t in self.texts)
        if self.files:
            s.append("files named in params:")
            s.extend("  " + f for f in sorted(self.files))
        else:
            s.append("files named in params: none")
        if self.palettes:
            s.append("palette components (insides not shown):")
            s.extend("  " + p for p in self.palettes)
        return s


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    ap = argparse.ArgumentParser(add_help=True, description="Dump a TouchDesigner project as text.")
    ap.add_argument("project", nargs="?")
    ap.add_argument("-o", "--out")
    ap.add_argument("--work")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--text", type=int, default=4000)
    ap.add_argument("--toeexpand")
    args = ap.parse_args()

    if not args.project:
        print(__doc__)
        exe = find_toeexpand(args.toeexpand)
        print("toeexpand:", exe or "not found; install TouchDesigner or pass --toeexpand")
        return 0

    project = os.path.abspath(args.project)
    notes = []
    if os.path.isdir(project):
        root = project
        work = os.path.dirname(project)
    elif os.path.isfile(project):
        exe = find_toeexpand(args.toeexpand)
        if not exe:
            print("toeexpand not found. It comes with TouchDesigner (its bin folder); "
                  "pass its path with --toeexpand.", file=sys.stderr)
            return 1
        stem = os.path.splitext(os.path.basename(project))[0]
        work = os.path.abspath(args.work) if args.work else os.path.join(
            tempfile.gettempdir(), "td_dump", stem)
        try:
            root = expand(project, work, exe)
        except (OSError, RuntimeError) as e:
            print(f"expanding failed: {e}", file=sys.stderr)
            return 1
        notes.append(f"expanded with {exe}")
    else:
        print(f"no such file or folder: {project}", file=sys.stderr)
        return 1

    notes.extend(unpack_lods(root))
    d = Dump(root, args.all, args.text)
    d.run()
    out = os.path.abspath(args.out) if args.out else os.path.join(work, "dump.txt")
    summary = d.summary()
    with open(out, "w", encoding="utf-8") as f:
        f.write(f"# {project}\n")
        for line in notes + summary:
            f.write(f"# {line}\n")
        f.write("\n")
        f.write("\n".join(d.lines) + "\n")
    for line in notes + summary:
        print(line)
    print(f"\nexpanded files: {root}")
    print(f"dump: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
