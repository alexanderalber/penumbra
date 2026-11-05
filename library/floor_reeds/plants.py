"""Writes floor_reeds' models next to this file: one line mesh per plant kind (`<kind>_lines.obj`)
and, for the kinds with a body, a fill mesh (`<kind>_fill.obj`). Run it with no arguments.

All models stand on z = 0, z up, about one unit tall. A line mesh is a set of very thin quads: the
wireframe material draws each quad's outline, and a quad this thin reads as one line. A corner's u
picks the ramp (0.25 the jade ramp, 0.75 the copper ramp), its v says how far along the blade or
leaf it lies (0 at the foot, 1 at the tip). The ramps light only the top of v, so a part that
should stay dark ends below that.
"""

import math
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent
JADE, COPPER = 0.25, 0.75
# Half a line's width at the foot and at the tip. Wider quads would show as open boxes close up.
LINE = 0.0008


def add(a, b):
    return (a[0] + b[0], a[1] + b[1], a[2] + b[2])


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def mul(a, s):
    return (a[0] * s, a[1] * s, a[2] * s)


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def norm(a):
    length = math.sqrt(sum(x * x for x in a)) or 1.0
    return (a[0] / length, a[1] / length, a[2] / length)


def rot_about(v, axis, angle):
    k = norm(axis)
    c, s = math.cos(angle), math.sin(angle)
    kv = cross(k, v)
    kd = k[0] * v[0] + k[1] * v[1] + k[2] * v[2]
    return tuple(v[i] * c + kv[i] * s + k[i] * kd * (1 - c) for i in range(3))


class Mesh:
    def __init__(self):
        self.v, self.t, self.f = [], [], []

    def poly(self, points, uvs):
        base = len(self.v)
        self.v += list(points)
        self.t += list(uvs)
        self.f.append(list(range(base, base + len(points))))

    def obj(self, comment):
        out = [f"# floor_reeds: {comment}"]
        out += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in self.v]
        out += [f"vt {u:.4f} {w:.4f}" for u, w in self.t]
        out += ["f " + " ".join(f"{i + 1}/{i + 1}" for i in f) for f in self.f]
        return "\n".join(out) + "\n"


def line(m, points, vs, u=JADE, across=None):
    """One line along `points`, thin quads whose width lies across `across` (default: sideways)."""
    for i in range(len(points) - 1):
        a, b = points[i], points[i + 1]
        d = across or side_of(sub(b, a))
        m.poly(
            [sub(a, mul(d, LINE)), add(a, mul(d, LINE)), add(b, mul(d, LINE)), sub(b, mul(d, LINE))],
            [(u, vs[i]), (u, vs[i]), (u, vs[i + 1]), (u, vs[i + 1])],
        )


def side_of(d):
    p = (-d[1], d[0], 0.0)
    if abs(p[0]) + abs(p[1]) < 1e-6:
        p = (1.0, 0.0, 0.0)
    return norm(p)


def blade(m, foot, out, h, bend, u=JADE, seg=4, v0=0.0, v1=1.0):
    """A blade rising `h`, its tip bent out by `bend`. Returns its points."""
    points, vs = [], []
    for k in range(seg + 1):
        s = k / seg
        r = bend * s * s
        points.append((foot[0] + out[0] * r, foot[1] + out[1] * r, foot[2] + h * s * (1 - 0.15 * bend * s)))
        vs.append(v0 + (v1 - v0) * s)
    line(m, points, vs, u)
    return points


def path(foot, out, length, e0, e1, n, p=1.6):
    """A centre line: its elevation runs from e0 to e1 degrees along the horizontal direction `out`."""
    points = [foot]
    q = foot
    for k in range(n):
        s = (k + 0.5) / n
        e = math.radians(e0 + (e1 - e0) * s**p)
        q = add(q, mul((out[0] * math.cos(e), out[1] * math.cos(e), math.sin(e)), length / n))
        points.append(q)
    return points


def leaf(fill, lines, foot, out, length, e0, e1, width, n=6, twist=0.0, u=JADE, shape=None, tip_jade=False):
    """A broad leaf: a black strip, its two edges as lines."""
    c = path(foot, out, length, e0, e1, n)
    shape = shape or (lambda s: 1.0 - s)
    left, right, normals, vs = [], [], [], []
    for k, p in enumerate(c):
        s = k / n
        along = norm(sub(c[min(k + 1, n)], c[max(k - 1, 0)]))
        across = rot_about(side_of(out), along, twist * s)
        half = 0.5 * width * shape(s)
        left.append(sub(p, mul(across, half)))
        right.append(add(p, mul(across, half)))
        normals.append(norm(cross(along, across)))
        vs.append(s)
    for k in range(n):
        fill.poly([left[k], right[k], right[k + 1], left[k + 1]], [(0.5, 0.0)] * 4)
    # The lines' width lies across the leaf's face, so half of each stands in front of the strip,
    # from whichever side one looks.
    for edge in (left, right):
        for k in range(n):
            line(lines, edge[k : k + 2], vs[k : k + 2], u, across=normals[k])
    if tip_jade:
        k = n - 2
        line(lines, c[k:], [0.9, 0.95, 1.0], JADE, across=normals[k])


def prism(fill, low, high, r_low, r_high, sides=6):
    """A closed black prism from `low` to `high`."""

    def ring(c, r):
        return [
            (c[0] + r * math.cos(2 * math.pi * j / sides), c[1] + r * math.sin(2 * math.pi * j / sides), c[2])
            for j in range(sides)
        ]

    a, b = ring(low, r_low), ring(high, r_high)
    for j in range(sides):
        fill.poly([a[j], a[(j + 1) % sides], b[(j + 1) % sides], b[j]], [(0.5, 0.0)] * 4)
    fill.poly(list(reversed(a)), [(0.5, 0.0)] * sides)
    fill.poly(b, [(0.5, 0.0)] * sides)


def reeds():
    """Nine blades; two of them end in a small plume of short blades."""
    rnd = random.Random(7)
    lines = Mesh()
    for i in range(9):
        turn = 2 * math.pi * (i + 0.5 * rnd.random()) / 9
        out = (math.cos(turn), math.sin(turn), 0.0)
        h = rnd.uniform(0.55, 1.0)
        foot = mul(out, 0.06 * rnd.random())
        points = blade(lines, foot, out, h, rnd.uniform(0.08, 0.32) * h)
        if i in (2, 6):
            top = points[-1]
            for j in range(5):
                a = 2 * math.pi * j / 5 + rnd.random()
                spray = (math.cos(a), math.sin(a), 0.0)
                blade(lines, top, spray, rnd.uniform(0.05, 0.09), rnd.uniform(0.02, 0.04), seg=2, v0=0.86, v1=1.0)
    return lines, None


def cattails():
    """Eight blades and three stalks with a dark club head."""
    rnd = random.Random(11)
    fill, lines = Mesh(), Mesh()
    for i in range(8):
        t = 2 * math.pi * (i + 0.5 * rnd.random()) / 8
        out = (math.cos(t), math.sin(t), 0.0)
        h = rnd.uniform(0.7, 1.05)
        blade(lines, mul(out, 0.03 * rnd.random()), out, h, rnd.uniform(0.05, 0.22) * h)
    for i in range(3):
        t = 2 * math.pi * (i / 3 + 0.1 * rnd.random())
        out = (math.cos(t), math.sin(t), 0.0)
        h = rnd.uniform(0.85, 1.0)
        low = h * rnd.uniform(0.66, 0.72)
        high = low + rnd.uniform(0.15, 0.19)
        at_low = (0.03 * out[0], 0.03 * out[1], low)
        at_high = (0.035 * out[0], 0.035 * out[1], high)
        line(lines, [(0.0, 0.0, 0.0), at_low, at_high, (0.04 * out[0], 0.04 * out[1], h)], [0.0, 0.7, 0.85, 1.0])
        r = rnd.uniform(0.009, 0.012)
        prism(fill, at_low, at_high, r * 0.85, r, sides=7)
    return lines, fill


def sedge():
    """Fourteen broad arching leaves."""
    rnd = random.Random(23)
    fill, lines = Mesh(), Mesh()
    for i in range(14):
        t = 2 * math.pi * (i + 0.6 * rnd.random()) / 14
        out = (math.cos(t), math.sin(t), 0.0)
        leaf(
            fill,
            lines,
            mul(out, 0.02),
            out,
            rnd.uniform(0.8, 1.25),
            rnd.uniform(70, 86),
            rnd.uniform(-25, 30),
            rnd.uniform(0.035, 0.05),
            twist=rnd.uniform(-0.8, 0.8),
            shape=lambda s: (1 - s) ** 0.8 * (0.6 + 0.4 * (1 - s)),
        )
    return lines, fill


def tufts():
    """Twenty-four short copper blades, low at the water."""
    rnd = random.Random(31)
    lines = Mesh()
    for i in range(24):
        t = 2 * math.pi * (i + 0.7 * rnd.random()) / 24
        out = (math.cos(t), math.sin(t), 0.0)
        h = rnd.uniform(0.45, 1.0)
        blade(lines, mul(out, 0.05 * rnd.random()), out, h, rnd.uniform(0.25, 0.75) * h, COPPER, seg=3)
    return lines, None


def papyrus():
    """Four stalks, each with a round head of fine straight rays. The rays stay dark: their v ends
    below the ramp's light, and only one ray in six reaches the jade."""
    rnd = random.Random(53)
    lines = Mesh()
    for i in range(4):
        t = 2 * math.pi * (i / 4 + 0.15 * rnd.random())
        out = (math.cos(t), math.sin(t), 0.0)
        h = rnd.uniform(0.62, 0.85)
        lean = rnd.uniform(0.03, 0.12)
        top = (out[0] * lean, out[1] * lean, h)
        line(lines, [(0.0, 0.0, 0.0), mul(top, 0.5), top], [0.0, 0.3, 0.6])
        for j in range(18):
            a = 2 * math.pi * (j + 0.5 * rnd.random()) / 18
            d = (math.cos(a), math.sin(a), 0.0)
            ray = path(top, d, rnd.uniform(0.12, 0.19), rnd.uniform(10, 70), rnd.uniform(5, 45), 2)
            end = 1.0 if j % 6 == 0 else 0.7
            line(lines, ray, [0.6, 0.65, end])
    return lines, None


def agave():
    """A rosette of eighteen pointed black leaves with copper edges and jade tips."""
    rnd = random.Random(61)
    fill, lines = Mesh(), Mesh()
    for i in range(18):
        ring = i % 3
        t = 2 * math.pi * (i / 18) + ring * 0.35 + 0.1 * rnd.random()
        out = (math.cos(t), math.sin(t), 0.0)
        e = [25, 48, 70][ring] + rnd.uniform(-8, 8)
        leaf(
            fill,
            lines,
            mul(out, 0.03),
            out,
            [0.85, 0.75, 0.6][ring] * rnd.uniform(0.85, 1.1),
            e,
            e - rnd.uniform(5, 20),
            0.15 * [1, 0.9, 0.75][ring],
            n=4,
            u=COPPER,
            shape=lambda s: min(1.0, 0.75 + s) * (1 - s) ** 1.1,
            tip_jade=True,
        )
    return lines, fill


def mast():
    """The tall flower stalk an agave rarely carries: black, leaning a little, candelabra branches
    with a dark cluster at each end."""
    rnd = random.Random(67)
    fill, lines = Mesh(), Mesh()
    height = 2.6
    lean = math.tan(math.radians(4.0))
    x = lambda z: lean * z

    prism(fill, (x(0.2), 0, 0.2), (x(height * 0.5), 0, height * 0.5), 0.016, 0.012, sides=5)
    prism(fill, (x(height * 0.5), 0, height * 0.5), (x(height), 0, height), 0.012, 0.006, sides=5)
    for k in range(6):
        z = height * (0.6 + 0.065 * k)
        sign = 1 if k % 2 else -1
        a = rnd.uniform(-0.5, 0.5)
        d = (sign * math.cos(a), math.sin(a), 0.0)
        branch = path((x(z), 0, z), d, 0.22 - 0.025 * k, 15, 50, 2)
        line(lines, branch, [0.85, 0.92, 1.0])
        tip = branch[-1]
        prism(fill, add(tip, (0, 0, -0.01)), add(tip, (0, 0, 0.04)), 0.02, 0.03, sides=6)
        prism(fill, branch[0], branch[1], 0.006, 0.005, sides=4)
        prism(fill, branch[1], branch[2], 0.005, 0.004, sides=4)
    return lines, fill


KINDS = {
    "reeds": reeds,
    "cattails": cattails,
    "sedge": sedge,
    "papyrus": papyrus,
    "agave": agave,
    "mast": mast,
    "tufts": tufts,
}


def main():
    for name, make in KINDS.items():
        lines, fill = make()
        (HERE / f"{name}_lines.obj").write_text(lines.obj(f"{name}, lines"), encoding="ascii", newline="\n")
        quads = len(lines.f)
        if fill is not None:
            (HERE / f"{name}_fill.obj").write_text(fill.obj(f"{name}, body"), encoding="ascii", newline="\n")
        print(f"{name}: {quads} line quads, {len(fill.f) if fill else 0} body faces")


if __name__ == "__main__":
    main()
