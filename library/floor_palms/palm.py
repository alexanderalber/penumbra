"""Writes floor_palms' two models next to this file: trunk.obj (a palm trunk 2.6 m tall,
leaning towards +y, in rings) and frond.obj (one frond along +y, arching up and drooping, its
leaflets as cross lines). Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The trunk: height, lean of the top towards +y, radius at the foot and at the top (metres),
# rings up the trunk and sides around it.
HEIGHT = 2.6
LEAN = 0.45
FOOT = 0.13
TOP = 0.08
RINGS = 11
SIDES = 6

# A frond: length (metres), steps along it, half its width at the widest, how high its middle
# rib stands over its edges (the fold), how far it rises and how far its tip droops.
LENGTH = 1.7
STEPS = 9
WIDTH = 0.32
FOLD = 0.08
RISE = 0.55
DROOP = 1.25


def trunk():
    lines = [
        "# A palm trunk for floor_palms: 2.6 m tall, its foot on the origin, z up, the top leaning",
        f"# {LEAN} m towards +y, {RINGS} rings of {SIDES} sides. Written by palm.py.",
    ]
    for r in range(RINGS + 1):
        t = r / RINGS
        z = HEIGHT * t
        # The lean grows with the square of the height: straight at the foot, bending at the top.
        cy = LEAN * t * t
        rad = FOOT + (TOP - FOOT) * t
        for s in range(SIDES):
            a = 2 * math.pi * (s + 0.5 * (r % 2)) / SIDES
            lines.append(f"v {rad * math.cos(a):.4f} {cy + rad * math.sin(a):.4f} {z:.4f}")
    for r in range(RINGS):
        for s in range(SIDES):
            a = r * SIDES + s + 1
            b = r * SIDES + (s + 1) % SIDES + 1
            c = b + SIDES
            d = a + SIDES
            lines.append(f"f {a} {b} {c} {d}")
    (HERE / "trunk.obj").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def frond():
    lines = [
        "# A palm frond for floor_palms: it starts at the origin and runs along +y for 1.7 m, z up,",
        "# arching up and drooping to its tip. A middle rib and an edge on each side, joined by cross",
        "# lines for the leaflets. Written by palm.py.",
    ]
    for i in range(STEPS + 1):
        t = i / STEPS
        y = LENGTH * t
        z = RISE * t - DROOP * t * t
        w = WIDTH * math.sin(math.pi * min(t * 1.15, 1.0)) ** 0.7
        lines.append(f"v {-w:.4f} {y:.4f} {z - FOLD * w / WIDTH:.4f}")
        lines.append(f"v 0 {y:.4f} {z:.4f}")
        lines.append(f"v {w:.4f} {y:.4f} {z - FOLD * w / WIDTH:.4f}")
    for i in range(STEPS):
        a = 3 * i + 1
        n = a + 3
        lines.append(f"f {a} {a + 1} {n + 1} {n}")
        lines.append(f"f {a + 1} {a + 2} {n + 2} {n + 1}")
    (HERE / "frond.obj").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def main():
    trunk()
    frond()
    print("wrote trunk.obj and frond.obj")


if __name__ == "__main__":
    main()
