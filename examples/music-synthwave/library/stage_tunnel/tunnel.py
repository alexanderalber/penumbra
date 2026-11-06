"""Writes the models of stage_tunnel: wall.obj and segment.obj, next to this file.

wall.obj is one square of the tube's wall, 1 by 1 in the xy plane around the origin, z up, with
faces on both sides: the stage draws it black, a piece between two rings, turned along the
tube's sheet, so the wall hides what lies outside the tube from either side. segment.obj is a
thin prism one unit long along y, centred on the origin: scaled to a centimetre across, its
three long edges draw one line. The rings, the long lines and the chevrons are all segments.
Run it with no arguments.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent


def write(name, head, verts, faces):
    lines = [f"# {line}" for line in head]
    lines += [f"v {x:.5f} {y:.5f} {z:.5f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces]
    (HERE / name).write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def main():
    wall = [(-0.5, -0.5, 0.0), (0.5, -0.5, 0.0), (0.5, 0.5, 0.0), (-0.5, 0.5, 0.0)]
    write("wall.obj", [
        "A piece of the wall of stage_tunnel: a square 1 by 1 in the xy plane, z up, faces on",
        "both sides. The stage draws it black, so it hides what lies outside the tube.",
    ], wall, [[0, 1, 2, 3], [0, 3, 2, 1]])

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
