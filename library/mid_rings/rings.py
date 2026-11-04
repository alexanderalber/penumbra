"""Writes the models of mid_rings: ring.obj and ring_fill.obj, next to this file.

A ring is a broad band around the z axis, cut into 12 segments with gaps between them, at
radius 1: from 0.85 to 1 across, from -0.5 to 0.5 in z. The element scales it to its radius and
its height. ring.obj holds the edges: the top and the bottom of each segment as quads, a hair
outside the band, so the wireframe draws their outlines over the black band. ring_fill.obj is
the band itself, closed boxes, which the element draws black, so a ring hides what lies behind
it. Run it with no arguments.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
SEGMENTS = 12
# The share of a segment's place the segment fills; the rest is the gap to the next one.
FILL = 0.8
# Pieces a segment is made of along the ring, so it curves.
PIECES = 3
INNER = 0.85
OUTER = 1.0
# How far the edges lie outside the band, in the model's units.
EDGE_RADIAL = 0.004
EDGE_Z = 0.04


def write(name, head, verts, faces):
    lines = [f"# {line}" for line in head]
    lines += [f"v {x:.5f} {y:.5f} {z:.5f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces]
    (HERE / name).write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def angles(s):
    """The angles along segment s, from its start to its end."""
    place = 2.0 * math.pi / SEGMENTS
    a0 = place * s
    return [a0 + place * FILL * i / PIECES for i in range(PIECES + 1)]


def at(r, a, z):
    return (r * math.sin(a), r * math.cos(a), z)


def edges():
    verts, faces = [], []
    for s in range(SEGMENTS):
        for z in (0.5 + EDGE_Z, -0.5 - EDGE_Z):
            base = len(verts)
            for a in angles(s):
                verts += [at(INNER - EDGE_RADIAL, a, z), at(OUTER + EDGE_RADIAL, a, z)]
            for i in range(PIECES):
                faces.append([base + 2 * i, base + 2 * i + 2, base + 2 * i + 3, base + 2 * i + 1])
    return verts, faces


def fill():
    verts, faces = [], []
    for s in range(SEGMENTS):
        base = len(verts)
        # Per angle: inner top, outer top, outer bottom, inner bottom.
        for a in angles(s):
            verts += [at(INNER, a, 0.5), at(OUTER, a, 0.5), at(OUTER, a, -0.5), at(INNER, a, -0.5)]
        for i in range(PIECES):
            p, q = base + 4 * i, base + 4 * i + 4
            for k in range(4):
                k1 = (k + 1) % 4
                faces.append([p + k, q + k, q + k1, p + k1])
        last = base + 4 * PIECES
        faces.append([base, base + 1, base + 2, base + 3])
        faces.append([last + 3, last + 2, last + 1, last])
    return verts, faces


def main():
    v, f = edges()
    write("ring.obj", [
        "A ring of mid_rings: the edges of a band around the z axis at radius 1, 0.85 to 1 across,",
        "z from -0.5 to 0.5, cut into 12 segments, as quads on the top and the bottom of each",
        "segment, a hair outside the band. The wireframe draws their outlines.",
    ], v, f)
    v, f = fill()
    write("ring_fill.obj", [
        "The band of a ring of mid_rings: 12 closed segments around the z axis at radius 1, 0.85",
        "to 1 across, z from -0.5 to 0.5. The element draws it black, so it hides what lies behind.",
    ], v, f)


if __name__ == "__main__":
    main()
