"""Writes octahedron.obj, torus.obj and icosahedron.obj for mid_shapes. Run with no arguments."""
import math
from pathlib import Path

OUT = Path(__file__).resolve().parent


def write(name, about, verts, faces):
    lines = [f"# {line}" for line in about]
    lines += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces]
    (OUT / name).write_text("\n".join(lines) + "\n", encoding="utf-8")


# An octahedron one unit across, corners on the axes.
octa_v = [(0.5, 0, 0), (-0.5, 0, 0), (0, 0.5, 0), (0, -0.5, 0), (0, 0, 0.5), (0, 0, -0.5)]
octa_f = [(0, 2, 4), (2, 1, 4), (1, 3, 4), (3, 0, 4), (2, 0, 5), (1, 2, 5), (3, 1, 5), (0, 3, 5)]
write("octahedron.obj", ["An octahedron one unit across, centred on the origin, z up. Written by shapes.py."], octa_v, octa_f)

# An icosahedron one unit across.
t = (1 + 5 ** 0.5) / 2
ico = [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0), (0, -1, t), (0, 1, t), (0, -1, -t), (0, 1, -t),
       (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]
ico_f = [(0, 11, 5), (0, 5, 1), (0, 1, 7), (0, 7, 10), (0, 10, 11), (1, 5, 9), (5, 11, 4), (11, 10, 2),
         (10, 7, 6), (7, 1, 8), (3, 9, 4), (3, 4, 2), (3, 2, 6), (3, 6, 8), (3, 8, 9), (4, 9, 5),
         (2, 4, 11), (6, 2, 10), (8, 6, 7), (9, 8, 1)]
n = math.sqrt(1 + t * t)
ico_v = [tuple(0.5 * c / n for c in v) for v in ico]
write("icosahedron.obj", ["An icosahedron one unit across, centred on the origin, z up. Written by shapes.py."], ico_v, ico_f)

# A torus one unit across, its hole along y: 16 steps around the ring, 6 around the tube, quads.
RING, TUBE, R, r = 16, 6, 0.36, 0.14
tor_v, tor_f = [], []
for i in range(RING):
    a = 2 * math.pi * i / RING
    for j in range(TUBE):
        b = 2 * math.pi * j / TUBE
        d = R + r * math.cos(b)
        tor_v.append((d * math.cos(a), r * math.sin(b), d * math.sin(a)))
for i in range(RING):
    for j in range(TUBE):
        a, b = i * TUBE + j, i * TUBE + (j + 1) % TUBE
        c, e = ((i + 1) % RING) * TUBE + (j + 1) % TUBE, ((i + 1) % RING) * TUBE + j
        tor_f.append((a, e, c, b))
write("torus.obj", ["A torus one unit across, centred on the origin, its hole along y (the way it flies): 16 by 6 quads.",
                    "Written by shapes.py."], tor_v, tor_f)
