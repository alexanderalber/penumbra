"""Writes ceiling_lights' three models next to this file, each hanging down -z from the origin
(the point on the ceiling): lamp.obj (a rod and a shade, drawn as lines), bulb.obj (the bulb in
the shade's mouth, a small ball) and rays.obj (short rays of light falling from the mouth, each
a thin prism that the wireframe draws as one line). Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The rod: its length and half its thickness. The shade: its height, the radius at its top and
# at its mouth, and its sides. The bulb's radius; the rays' count, length and thickness.
# Metres at size 1.
ROD = 0.55
THICK = 0.006
SHADE = 0.18
TOP = 0.05
MOUTH = 0.2
SIDES = 8
BULB = 0.095
RAYS = 10
REACH = 0.3
THIN = 0.003


def write(name, lines):
    (HERE / name).write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def lamp():
    lines = [
        f"# A lamp for ceiling_lights: a rod {ROD} m down from the origin, then a shade of {SIDES} sides",
        f"# {SHADE} m tall opening to a radius of {MOUTH} m. Hangs down -z. Written by lamp.py.",
    ]
    # The rod: a thin square prism, which the wireframe draws as one line.
    for z in (0.0, -ROD):
        for x, y in [(-THICK, -THICK), (THICK, -THICK), (THICK, THICK), (-THICK, THICK)]:
            lines.append(f"v {x} {y} {z}")
    faces = []
    for s in range(4):
        a = s + 1
        b = (s + 1) % 4 + 1
        faces.append(f"f {a} {b} {b + 4} {a + 4}")
    # The shade: a ring at its top and one at its mouth.
    base = 8
    for z, r in [(-ROD, TOP), (-ROD - SHADE, MOUTH)]:
        for s in range(SIDES):
            a = 2 * math.pi * (s + 0.5) / SIDES
            lines.append(f"v {r * math.cos(a):.4f} {r * math.sin(a):.4f} {z:.4f}")
    for s in range(SIDES):
        a = base + s + 1
        b = base + (s + 1) % SIDES + 1
        faces.append(f"f {a} {b} {b + SIDES} {a + SIDES}")
    write("lamp.obj", lines + faces)


def ball(name, radius, rings, sides, what):
    z0 = -ROD - SHADE + 0.02
    lines = [
        f"# {what} for ceiling_lights: a ball of radius {radius} m in the shade's mouth,",
        f"# {ROD + SHADE - 0.02:.2f} m below the origin. Written by lamp.py.",
        f"v 0 0 {z0 + radius:.4f}",
    ]
    for i in range(1, rings):
        t = math.pi * i / rings
        for s in range(sides):
            a = 2 * math.pi * s / sides
            lines.append(f"v {radius * math.sin(t) * math.cos(a):.4f} {radius * math.sin(t) * math.sin(a):.4f} "
                         f"{z0 + radius * math.cos(t):.4f}")
    lines.append(f"v 0 0 {z0 - radius:.4f}")
    bottom = 2 + (rings - 1) * sides
    for s in range(sides):
        lines.append(f"f 1 {2 + (s + 1) % sides} {2 + s}")
    for i in range(rings - 2):
        for s in range(sides):
            a = 2 + i * sides + s
            b = 2 + i * sides + (s + 1) % sides
            lines.append(f"f {a} {b} {b + sides} {a + sides}")
    last = 2 + (rings - 2) * sides
    for s in range(sides):
        lines.append(f"f {bottom} {last + s} {last + (s + 1) % sides}")
    write(name, lines)


def rays():
    mouth = -ROD - SHADE
    lines = [
        f"# Light falling from a lamp of ceiling_lights: {RAYS} rays from the shade's mouth, {mouth:.2f} m below",
        f"# the origin, {REACH} m down and spreading. Written by lamp.py.",
    ]
    faces = []
    for s in range(RAYS):
        a = 2 * math.pi * (s + 0.25) / RAYS
        c, d = math.cos(a), math.sin(a)
        for r, z in [(0.8 * MOUTH, mouth), (1.6 * MOUTH, mouth - REACH)]:
            for dr, dt in [(THIN, 0.0), (-0.5 * THIN, THIN), (-0.5 * THIN, -THIN)]:
                lines.append(f"v {(r + dr) * c - dt * d:.4f} {(r + dr) * d + dt * c:.4f} {z:.4f}")
        o = 6 * s + 1
        for k in range(3):
            m = (k + 1) % 3
            faces.append(f"f {o + k} {o + m} {o + 3 + m} {o + 3 + k}")
    write("rays.obj", lines + faces)


def main():
    lamp()
    ball("bulb.obj", BULB, 4, 8, "A bulb")
    rays()
    print("wrote lamp.obj, bulb.obj and rays.obj")


if __name__ == "__main__":
    main()
