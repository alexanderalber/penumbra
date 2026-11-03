"""Writes the models of stage_ascent: storey.obj, slab.obj and post.obj, next to this file.

A storey is a ring-shaped floor along the tower's wall, open in the middle: from radius 4.5 to
6 around the z axis, 48 sectors. storey.obj holds its edges as two rings of 48 quads, 5 cm above
and 5 cm below the floor, so the wireframe draws them on top and underneath alike. slab.obj is
the floor itself, the same quads at height 0, once facing up and once facing down: the stage
draws it black, so a storey hides what lies behind it. post.obj is a thin prism one unit long
along y, centred on the origin: scaled to two centimetres across, its three long edges draw one
line. Run it with no arguments.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
SIDES = 48
OUTER = 6.0
INNER = 4.5
EDGE = 0.05


def write(name, head, verts, faces):
    lines = [f"# {line}" for line in head]
    lines += [f"v {x:.5f} {y:.5f} {z:.5f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces]
    (HERE / name).write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def ring(z, base):
    """Vertices and upward faces of one ring of quads at height z, numbered from base."""
    verts = []
    for i in range(SIDES):
        a = 2.0 * math.pi * i / SIDES
        s, c = math.sin(a), math.cos(a)
        verts += [(INNER * s, INNER * c, z), (OUTER * s, OUTER * c, z)]
    faces = []
    for i in range(SIDES):
        j = (i + 1) % SIDES
        # Inner corner, the next inner one, then outwards: seen from above this runs
        # counter-clockwise, so the face looks up.
        faces.append([base + 2 * i, base + 2 * j, base + 2 * j + 1, base + 2 * i + 1])
    return verts, faces


def main():
    top, top_f = ring(EDGE, 0)
    bottom, bottom_f = ring(-EDGE, len(top))
    write("storey.obj", [
        "A storey of stage_ascent: the edges of a ring-shaped floor from radius 4.5 to 6 around",
        "the z axis, 48 sectors, as two rings of quads 5 cm above and below the floor, z up.",
        "The wireframe draws their outlines: two circles each and the lines between the sectors.",
    ], top + bottom, top_f + bottom_f)

    slab, up = ring(0.0, 0)
    down = [list(reversed(f)) for f in up]
    write("slab.obj", [
        "The floor of a storey of stage_ascent: a ring from radius 4.5 to 6 around the z axis,",
        "48 quads at height 0, once facing up and once facing down, z up. The stage draws it",
        "black, so it hides what lies behind it.",
    ], slab, up + down)

    h = math.sqrt(3.0) / 2.0
    tri = [(0.0, 2.0 * h / 3.0), (-0.5, -h / 3.0), (0.5, -h / 3.0)]
    seg = [(x, -0.5, z) for x, z in tri] + [(x, 0.5, z) for x, z in tri]
    write("post.obj", [
        "A post of stage_ascent: a thin triangular prism one unit long along y, centred on the",
        "origin, z up, one unit across. Scaled to two centimetres across, the wireframe draws its",
        "three long edges as one line.",
    ], seg, [[0, 1, 4, 3], [1, 2, 5, 4], [2, 0, 3, 5]])


if __name__ == "__main__":
    main()
