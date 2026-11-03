"""Writes the models of stage_tunnel: ring.obj, band.obj and segment.obj, next to this file.

ring.obj is one polygon of 48 corners, a circle of radius 1 in the xz plane around the origin:
the wireframe draws its outline as a ring. band.obj is the tube's wall between two rings, 48
quads around the y axis at radius 1, from y = 0 to y = 1, facing inwards. segment.obj is a thin
prism one unit long along y, centred on the origin: scaled to a centimetre across, its three
long edges draw one line. Run it with no arguments.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
SIDES = 48


def write(name, head, verts, faces):
    lines = [f"# {line}" for line in head]
    lines += [f"v {x:.5f} {y:.5f} {z:.5f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces]
    (HERE / name).write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def corner(i):
    a = 2.0 * math.pi * i / SIDES
    return math.sin(a), -math.cos(a)


def main():
    ring = [(x, 0.0, z) for x, z in (corner(i) for i in range(SIDES))]
    write("ring.obj", [
        "A ring of stage_tunnel: one polygon of 48 corners, radius 1 in the xz plane, z up.",
        "The wireframe draws only the polygon's outline.",
    ], ring, [list(range(SIDES))])

    band = []
    for i in range(SIDES):
        x, z = corner(i)
        band += [(x, 0.0, z), (x, 1.0, z)]
    faces = []
    for i in range(SIDES):
        j = (i + 1) % SIDES
        # Wound so the face looks inwards, towards the camera inside the tube.
        faces.append([2 * i, 2 * j, 2 * j + 1, 2 * i + 1])
    write("band.obj", [
        "The wall of stage_tunnel between two rings: 48 quads around the y axis at radius 1,",
        "from y = 0 to y = 1, z up, facing inwards. The stage draws it black, so it hides what",
        "lies outside the tube.",
    ], band, faces)

    h = math.sqrt(3.0) / 2.0
    tri = [(0.0, 2.0 * h / 3.0), (-0.5, -h / 3.0), (0.5, -h / 3.0)]
    seg = [(x, -0.5, z) for x, z in tri] + [(x, 0.5, z) for x, z in tri]
    write("segment.obj", [
        "A line of stage_tunnel: a thin triangular prism one unit long along y, centred on the",
        "origin, z up, one unit across. Scaled to a centimetre across, the wireframe draws its",
        "three long edges as one line.",
    ], seg, [[0, 1, 4, 3], [1, 2, 5, 4], [2, 0, 3, 5]])


if __name__ == "__main__":
    main()
