"""Writes ceiling_shafts' models next to this file. rays_0.obj, rays_1.obj and rays_2.obj are
the top, middle and lowest third of a cone of light one unit long down -z, radius 0.3 at the
origin and 1 at its far end: 16 rays, each a thin prism that the wireframe draws as one line,
and around the lowest third the ring where the light meets the floor. hole.obj is a square one
unit across in the plane z = 0, the opening in the ceiling; mote.obj a speck of dust, an
octahedron one unit across. Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The cone: rays around it, its radius at the top as a share of the radius at its foot, the
# rays' thickness in the same unit and the parts along its length.
SIDES = 16
NARROW = 0.3
THICK = 0.003
BANDS = 3


def write(name, lines):
    (HERE / name).write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def rays(band):
    t0, t1 = band / BANDS, (band + 1) / BANDS
    lines = [
        f"# Part {band} of {BANDS} of a cone of light for ceiling_shafts, from {t0:.3f} to {t1:.3f} of its length:",
        f"# {SIDES} rays, each a thin prism. The cone's narrow end (radius {NARROW}) is on the origin, its foot",
        "# (radius 1) one unit down -z." + (" A ring where the light meets the floor." if band == BANDS - 1 else ""),
        "# Written by shafts.py.",
    ]
    faces = []
    count = 0
    for s in range(SIDES):
        a = 2 * math.pi * s / SIDES
        c, d = math.cos(a), math.sin(a)
        for t in (t0, t1):
            r = NARROW + (1 - NARROW) * t
            # Three corners around the ray: out, and to both sides along the ring.
            for dr, dt in [(THICK, 0.0), (-0.5 * THICK, THICK), (-0.5 * THICK, -THICK)]:
                x = (r + dr) * c - dt * d
                y = (r + dr) * d + dt * c
                lines.append(f"v {x:.4f} {y:.4f} {-t:.4f}")
        o = count + 1
        for k in range(3):
            m = (k + 1) % 3
            faces.append(f"f {o + k} {o + m} {o + 3 + m} {o + 3 + k}")
        count += 6
    if band == BANDS - 1:
        ring = []
        for s in range(SIDES):
            a = 2 * math.pi * s / SIDES
            lines.append(f"v {math.cos(a):.4f} {math.sin(a):.4f} -1")
            count += 1
            ring.append(str(count))
        faces.append("f " + " ".join(ring))
    write(f"rays_{band}.obj", lines + faces)


def hole():
    write("hole.obj", [
        "# The opening of a light shaft for ceiling_shafts: a square one unit across in the plane",
        "# z = 0, centred on the origin. Written by shafts.py.",
        "v -0.5 -0.5 0",
        "v 0.5 -0.5 0",
        "v 0.5 0.5 0",
        "v -0.5 0.5 0",
        "f 1 2 3 4",
    ])


def mote():
    lines = [
        "# A mote of dust for ceiling_shafts: an octahedron one unit across, centred on the origin.",
        "# Written by shafts.py.",
    ]
    for p in [(0.5, 0, 0), (-0.5, 0, 0), (0, 0.5, 0), (0, -0.5, 0), (0, 0, 0.5), (0, 0, -0.5)]:
        lines.append("v {} {} {}".format(*p))
    for f in [(1, 3, 5), (3, 2, 5), (2, 4, 5), (4, 1, 5), (3, 1, 6), (2, 3, 6), (4, 2, 6), (1, 4, 6)]:
        lines.append("f {} {} {}".format(*f))
    write("mote.obj", lines)


def main():
    for band in range(BANDS):
        rays(band)
    hole()
    mote()
    print("wrote rays_0.obj to rays_2.obj, hole.obj and mote.obj")


if __name__ == "__main__":
    main()
