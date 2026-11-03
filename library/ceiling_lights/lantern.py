"""Writes ceiling_lights' three models next to this file, each hanging down -z from the origin
(the point on the ceiling). lantern.obj is a paper lantern: a short string, then a body of 8
ribs and 3 rings round an ellipsoid, a cap ring at its top and bottom and a short tassel.
core.obj is the light inside: a smaller shell of 6 ribs. halo.obj is the soft light around a
lit lantern: a larger shell of 8 ribs, turned half a rib against the body. Every line is a thin
prism, which the wireframe draws as one line. Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
NL = "\n"

# Metres at size 1: the string, the body's half width and half height, its ribs and rings, the
# steps a rib or ring is drawn with, the openings' radius as a share of the half width, the
# tassel, and half a line's thickness.
STRING = 0.32
WIDE = 0.2
TALL = 0.27
RIBS = 8
RINGS = 3
STEPS = 12
OPEN = 0.42
TASSEL = 0.12
THIN = 0.004


class Model:
    def __init__(self, comment):
        self.lines = [f"# {c}" for c in comment]
        self.faces = []
        self.count = 0

    def prism(self, a, b):
        """A thin triangular prism from point a to point b."""
        d = [b[i] - a[i] for i in range(3)]
        length = math.sqrt(sum(x * x for x in d)) or 1.0
        f = [x / length for x in d]
        # Two directions across the line.
        helper = (0.0, 0.0, 1.0) if abs(f[2]) < 0.9 else (1.0, 0.0, 0.0)
        u = cross(f, helper)
        n = math.sqrt(sum(x * x for x in u)) or 1.0
        u = [x / n for x in u]
        w = cross(f, u)
        o = self.count + 1
        for p in (a, b):
            for k in range(3):
                t = 2 * math.pi * k / 3
                c, s = math.cos(t) * THIN, math.sin(t) * THIN
                q = [p[i] + c * u[i] + s * w[i] for i in range(3)]
                self.lines.append(f"v {q[0]:.4f} {q[1]:.4f} {q[2]:.4f}")
        self.count += 6
        for k in range(3):
            m = (k + 1) % 3
            self.faces.append(f"f {o + k} {o + m} {o + 3 + m} {o + 3 + k}")

    def polyline(self, points, closed=False):
        pairs = list(zip(points, points[1:]))
        if closed:
            pairs.append((points[-1], points[0]))
        for a, b in pairs:
            self.prism(a, b)

    def write(self, name):
        (HERE / name).write_text(NL.join(self.lines + self.faces) + NL, encoding="utf-8", newline=NL)


def cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


def shell(model, middle, wide, tall, ribs, rings, turn):
    """Ribs from the top opening to the bottom one round an ellipsoid centred `middle` metres
    down, and `rings` rings between."""
    top = math.asin(OPEN)
    for r in range(ribs):
        a = 2 * math.pi * (r + turn) / ribs
        points = []
        for s in range(STEPS + 1):
            t = top + (math.pi - 2 * top) * s / STEPS
            points.append((wide * math.sin(t) * math.cos(a), wide * math.sin(t) * math.sin(a),
                           -middle + tall * math.cos(t)))
        model.polyline(points)
    for k in range(rings):
        t = math.pi * (k + 1) / (rings + 1)
        model.polyline(ring(wide * math.sin(t), -middle + tall * math.cos(t)), closed=True)


def ring(radius, z, sides=16):
    return [(radius * math.cos(2 * math.pi * s / sides), radius * math.sin(2 * math.pi * s / sides), z)
            for s in range(sides)]


def main():
    middle = STRING + TALL
    lantern = Model([
        f"A paper lantern for ceiling_lights: a string {STRING} m down from the origin, then a body of",
        f"{RIBS} ribs and {RINGS} rings round an ellipsoid {2 * WIDE} m wide and {2 * TALL} m tall, cap rings and a",
        f"tassel of {TASSEL} m. Hangs down -z. Written by lantern.py.",
    ])
    lantern.prism((0.0, 0.0, 0.0), (0.0, 0.0, -STRING))
    shell(lantern, middle, WIDE, TALL, RIBS, RINGS, 0.0)
    cap = WIDE * OPEN
    for z in (-STRING, -middle - TALL):
        lantern.polyline(ring(cap, z, 8), closed=True)
    lantern.prism((0.0, 0.0, -middle - TALL), (0.0, 0.0, -middle - TALL - TASSEL))
    lantern.write("lantern.obj")

    core = Model([
        "The light inside a lantern of ceiling_lights: a shell of 6 ribs, 0.5 of the body's size,",
        "round its middle. Written by lantern.py.",
    ])
    for share in (0.5,):
        shell(core, middle, WIDE * share, TALL * share, 6, 1, 0.25)
    core.write("core.obj")

    halo = Model([
        "The soft light round a lit lantern of ceiling_lights: a shell of 8 ribs 1.3 times the body's",
        "size, turned half a rib against it. Written by lantern.py.",
    ])
    shell(halo, middle, WIDE * 1.3, TALL * 1.18, RIBS, 0, 0.5)
    halo.write("halo.obj")
    print("wrote lantern.obj, core.obj and halo.obj")


if __name__ == "__main__":
    main()
