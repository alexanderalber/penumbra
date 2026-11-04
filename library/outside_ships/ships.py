"""Writes the models of outside_ships next to this file: for each of three hulls a, b and c,
hull_<x>.obj (the edges), fill_<x>.obj (the same parts a little smaller, drawn black) and
glow_<x>.obj (the engines' mouths, drawn lit), and the running lights of all three into
ships.wgsl, between the lines `// lights begin` and `// lights end`.

Every model is one unit long along y, the bow at +y, z up, centred on the origin; the element
scales it to the ship's length. The hulls are made up: plain boxes, tapered blocks and
eight-sided barrels.

- a, a hauler: a thin spine with cargo blocks along both sides, a tapered bow, a wide engine
  block and three engines at the stern, fins above and below.
- b, a wedge: a long hull narrowing to the bow, a bridge on its back, two engine pods beside the
  stern, a fin on top.
- c, a ring ship: a barrel hull with a pointed bow, a ring of blocks around it on four spokes,
  four engines at the stern.

Run it with no arguments.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
# How much smaller the black parts are than their edges, so the edges draw over them.
SHRINK = 0.97
SIDES = 8


class Model:
    def __init__(self):
        self.parts = []  # each a (vertices, faces) pair, faces counter-clockwise from outside
        self.mouths = []  # engine mouths: (vertices, faces)

    def add(self, verts, faces):
        self.parts.append((verts, faces))

    def block(self, x, y0, y1, z, w0, h0, w1=None, h1=None):
        """A tapered block along y from y0 to y1, w0 by h0 across at y0 and w1 by h1 at y1."""
        w1 = w0 if w1 is None else w1
        h1 = h0 if h1 is None else h1
        v = []
        for y, w, h in ((y0, w0, h0), (y1, w1, h1)):
            v += [(x - w / 2, y, z - h / 2), (x + w / 2, y, z - h / 2), (x + w / 2, y, z + h / 2), (x - w / 2, y, z + h / 2)]
        f = [[0, 3, 2, 1], [4, 5, 6, 7]]
        for k in range(4):
            k1 = (k + 1) % 4
            f.append([k, k1, k1 + 4, k + 4])
        self.add(v, f)

    def box(self, x, y, z, sx, sy, sz):
        self.block(x, y - sy / 2, y + sy / 2, z, sx, sz)

    def barrel(self, x, z, y0, y1, r0, r1=None, mouth=False):
        """An eight-sided barrel along y from y0 (radius r0) to y1 (radius r1); `mouth` puts a lit
        disc on its end at y0."""
        r1 = r0 if r1 is None else r1
        v = []
        for y, r in ((y0, r0), (y1, r1)):
            for i in range(SIDES):
                a = 2.0 * math.pi * (i + 0.5) / SIDES
                v.append((x + r * math.cos(a), y, z + r * math.sin(a)))
        f = [list(range(SIDES)), list(range(2 * SIDES - 1, SIDES - 1, -1))]
        for i in range(SIDES):
            j = (i + 1) % SIDES
            f.append([i, i + SIDES, j + SIDES, j])
        self.add(v, f)
        if mouth:
            d = []
            for i in range(SIDES):
                a = 2.0 * math.pi * (i + 0.5) / SIDES
                d.append((x + 0.8 * r0 * math.cos(a), y0 - 0.002, z + 0.8 * r0 * math.sin(a)))
            self.mouths.append((d, [list(range(SIDES)), list(range(SIDES - 1, -1, -1))]))


def hauler():
    m = Model()
    m.block(0.0, -0.40, 0.32, 0.0, 0.035, 0.035)
    m.block(0.0, 0.32, 0.50, 0.0, 0.07, 0.06, 0.02, 0.02)
    m.box(0.0, 0.26, 0.04, 0.05, 0.06, 0.04)
    for i in range(6):
        y = 0.20 - 0.095 * i
        for side in (-1, 1):
            m.box(side * 0.055, y, 0.0, 0.07, 0.08, 0.06 + 0.012 * (i % 2))
    m.block(0.0, -0.46, -0.38, 0.0, 0.16, 0.08)
    for x in (-0.05, 0.0, 0.05):
        m.barrel(x, 0.0, -0.50, -0.46, 0.022, 0.018, mouth=True)
    m.block(0.0, -0.44, -0.30, 0.07, 0.008, 0.06, 0.008, 0.02)
    m.block(0.0, -0.44, -0.30, -0.07, 0.008, 0.06, 0.008, 0.02)
    lights = [(0.0, 0.50, 0.0, "strobe"), (-0.09, 0.20, 0.0, "port"), (0.09, 0.20, 0.0, "starboard"),
              (-0.09, -0.27, 0.0, "port"), (0.09, -0.27, 0.0, "starboard"), (0.0, 0.26, 0.065, "beacon"),
              (0.0, -0.30, 0.10, "beacon"), (-0.08, -0.42, 0.0, "amber"), (0.08, -0.42, 0.0, "amber"),
              (0.0, -0.10, 0.04, "strobe")]
    return m, lights


def wedge():
    m = Model()
    m.block(0.0, -0.42, 0.50, 0.0, 0.20, 0.07, 0.015, 0.012)
    m.block(0.0, -0.30, -0.05, 0.05, 0.08, 0.03, 0.04, 0.02)
    m.box(0.0, -0.22, 0.08, 0.05, 0.06, 0.03)
    for side in (-1, 1):
        m.barrel(side * 0.12, -0.01, -0.50, -0.16, 0.032, 0.026, mouth=True)
        m.box(side * 0.085, -0.30, -0.01, 0.04, 0.10, 0.015)
    m.barrel(0.0, 0.0, -0.46, -0.42, 0.03, 0.03, mouth=True)
    m.block(0.0, -0.40, -0.10, 0.11, 0.006, 0.07, 0.006, 0.01)
    lights = [(0.0, 0.50, 0.0, "strobe"), (-0.155, -0.30, -0.01, "port"), (0.155, -0.30, -0.01, "starboard"),
              (-0.06, 0.12, 0.0, "port"), (0.06, 0.12, 0.0, "starboard"), (0.0, -0.22, 0.10, "beacon"),
              (0.0, -0.38, 0.15, "beacon"), (-0.12, -0.50, -0.01, "amber"), (0.12, -0.50, -0.01, "amber"),
              (0.0, 0.25, 0.02, "strobe")]
    return m, lights


def ring_ship():
    m = Model()
    m.barrel(0.0, 0.0, -0.40, 0.36, 0.045)
    m.barrel(0.0, 0.0, 0.36, 0.50, 0.045, 0.008)
    m.box(0.0, 0.22, 0.05, 0.04, 0.10, 0.03)
    n = 16
    r = 0.19
    for i in range(n):
        a = 2.0 * math.pi * i / n
        # A block of the ring, turned to face out: a tapered block along y cannot turn, so the
        # ring's blocks stand upright beside and above the hull as short boxes.
        x, z = r * math.cos(a), r * math.sin(a)
        size = 2.0 * math.pi * r / n * 0.8
        m.box(x, -0.08, z, size if abs(math.sin(a)) > 0.7 else 0.035, 0.07, size if abs(math.sin(a)) <= 0.7 else 0.035)
    for x, z in ((0.0, 1.0), (0.0, -1.0), (1.0, 0.0), (-1.0, 0.0)):
        if x == 0.0:
            m.box(0.0, -0.08, z * (0.045 + r) / 2, 0.012, 0.03, r - 0.045)
        else:
            m.box(x * (0.045 + r) / 2, -0.08, 0.0, r - 0.045, 0.03, 0.012)
    for x, z in ((-0.03, -0.03), (0.03, -0.03), (-0.03, 0.03), (0.03, 0.03)):
        m.barrel(x, z, -0.50, -0.40, 0.022, 0.026, mouth=True)
    lights = [(0.0, 0.50, 0.0, "strobe"), (-0.215, -0.08, 0.0, "port"), (0.215, -0.08, 0.0, "starboard"),
              (0.0, -0.08, 0.215, "beacon"), (0.0, -0.08, -0.215, "beacon"), (0.0, 0.22, 0.07, "strobe"),
              (-0.05, 0.10, 0.0, "port"), (0.05, 0.10, 0.0, "starboard"), (-0.06, -0.45, 0.0, "amber"),
              (0.06, -0.45, 0.0, "amber")]
    return m, lights


def shrunk(verts, k):
    c = [sum(p[i] for p in verts) / len(verts) for i in range(3)]
    return [tuple(c[i] + k * (p[i] - c[i]) for i in range(3)) for p in verts]


def write(name, head, parts):
    lines = [f"# {line}" for line in head]
    base = 0
    faces = []
    for verts, fs in parts:
        lines += [f"v {x:.5f} {y:.5f} {z:.5f}" for x, y, z in verts]
        faces += ["f " + " ".join(str(base + i + 1) for i in f) for f in fs]
        base += len(verts)
    (HERE / name).write_bytes(("\n".join(lines + faces) + "\n").encode("ascii"))


KINDS = {"strobe": 0, "port": 1, "starboard": 2, "beacon": 3, "amber": 4}


def main():
    table = []
    for key, (make, what) in {"a": (hauler, "hauler"), "b": (wedge, "wedge"), "c": (ring_ship, "ring ship")}.items():
        m, lights = make()
        write(f"hull_{key}.obj", [f"Hull {key} of outside_ships, a {what}: its edges. One unit long along y, bow at +y, z up."], m.parts)
        write(f"fill_{key}.obj", [f"Hull {key} of outside_ships: its parts {SHRINK} as large, drawn black."],
              [(shrunk(v, SHRINK), f) for v, f in m.parts])
        write(f"glow_{key}.obj", [f"Hull {key} of outside_ships: the mouths of its engines, drawn lit."], m.mouths)
        table.append(lights)
    count = len(table[0])
    assert all(len(t) == count for t in table)
    body = [f"const LIGHTS: u32 = {count}u;",
            "// Per hull, per light: x, y, z in the model's units and the kind (0 strobe, 1 port, 2",
            "// starboard, 3 beacon, 4 amber).",
            f"const LIGHT_AT = array<vec4f, {3 * count}>("]
    rows = []
    for lights in table:
        for x, y, z, kind in lights:
            rows.append(f"    vec4f({x:.3f}, {y:.3f}, {z:.3f}, {KINDS[kind]}.0),")
    body += rows + [");"]
    wgsl = HERE / "ships.wgsl"
    text = wgsl.read_text(encoding="utf-8")
    start = text.index("// lights begin\n") + len("// lights begin\n")
    end = text.index("// lights end")
    wgsl.write_bytes((text[:start] + "\n".join(body) + "\n" + text[end:]).encode("utf-8"))


if __name__ == "__main__":
    main()
