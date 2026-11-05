"""Writes floor_cenote's models next to this file. Run it with no arguments.

All in a box of radius 1, z up, scaled by the placement shader (cenote.wgsl) to the ring's radius
and height. A face's u is its light, looked up in stone.wgsl's ramp.

ring.obj: the rock ring seen from outside and from above: broken blocks of different heights, its
foot a little below z = 0, so the water line cuts it. A thin strip along the outer top edge
catches the band's light.
inner.obj: the ring's inner wall, brightest at the water: the pool lights it from below.
pool.obj: the pool's disc, just above the water line, u and v the disc's place in the picture.
hot.obj: a flat quad for one hot spot in the pool. shaft.obj: an upright quad for the light that
falls in, facing the camera as it flies along +y.
"""

import math
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent

SEGMENTS = 144
FOOT = -0.2
POOL_Z = 0.06
POOL_R = 0.905


class Mesh:
    def __init__(self, title):
        self.lines = [f"# {title}"]
        self.vs = 0
        self.body = []

    def face(self, points, uvs):
        ids = []
        for (x, y, z), (u, v) in zip(points, uvs):
            self.lines.append(f"v {x:.5f} {y:.5f} {z:.5f}")
            self.lines.append(f"vt {u:.5f} {v:.5f}")
            self.vs += 1
            ids.append(self.vs)
        self.body.append("f " + " ".join(f"{i}/{i}" for i in ids))

    def write(self, name):
        (HERE / name).write_text("\n".join(self.lines + self.body) + "\n", encoding="ascii", newline="\n")


def blocks(rng):
    """Height and radial offset per segment: runs of segments share a block, so the top is stepped."""
    out = []
    while len(out) < SEGMENTS:
        run = rng.randint(4, 11)
        height = rng.uniform(0.52, 1.0)
        # Now and then a block has broken away, so the ring is not a perfect wall.
        if rng.random() < 0.09:
            height = rng.uniform(0.22, 0.36)
        off = rng.uniform(-0.016, 0.016)
        out += [(height, off)] * run
    return out[:SEGMENTS]


def at(angle, r, z):
    return (r * math.cos(angle), r * math.sin(angle), z)


def write():
    rng = random.Random(11)
    ring = Mesh("a rock ring, outside and top; u is the light")
    inner = Mesh("a rock ring, the inner wall; u is the light from the pool")
    pool = Mesh("the pool's disc")
    parts = blocks(rng)
    for i in range(SEGMENTS):
        a0 = 2 * math.pi * i / SEGMENTS
        a1 = 2 * math.pi * (i + 1) / SEGMENTS
        h, off = parts[i]
        # The band stands far ahead (+y): the far side's tops catch more of it.
        far = 0.5 + 0.5 * math.sin(0.5 * (a0 + a1))
        grain = rng.random()
        v = (i + 0.5) / SEGMENTS

        def strip(r0, z0, r1, z1, light):
            ring.face([at(a0, r0 + off, z0), at(a1, r0 + off, z0), at(a1, r1 + off, z1), at(a0, r1 + off, z1)],
                      [(light, v)] * 4)

        strip(1.0, FOOT, 0.992, 0.3 * h, 0.003 + 0.003 * grain)
        strip(0.992, 0.3 * h, 0.972, 0.94 * h, 0.005 + 0.006 * grain)
        strip(0.972, 0.94 * h, 0.968, h, 0.16 + 0.14 * grain)
        strip(0.968, h, 0.905, 0.96 * h, 0.006 + 0.008 * grain + 0.01 * far)
        # A block's side, where the next block stands lower or higher.
        h2 = parts[(i + 1) % SEGMENTS][0]
        if h2 != h:
            low, high = min(h, h2), max(h, h2)
            ring.face([at(a1, 0.972, low), at(a1, 0.905, low), at(a1, 0.905, high), at(a1, 0.972, high)],
                      [(0.006 + 0.008 * grain, v)] * 4)
        layers = 8
        for k in range(layers):
            f0 = k / layers
            f1 = (k + 1) / layers
            z0 = POOL_Z + (0.96 * h - POOL_Z) * f0
            z1 = POOL_Z + (0.96 * h - POOL_Z) * f1
            r0 = 0.895 + 0.01 * f0 + off
            r1 = 0.895 + 0.01 * f1 + off
            light = (1.0 - f0) ** 2 * 0.9 + 0.02
            inner.face([at(a0, r0, z0), at(a1, r0, z0), at(a1, r1, z1), at(a0, r1, z1)], [(min(light, 1.0), v)] * 4)
    # The pool: a fan, uv the disc's place in the picture.
    for i in range(SEGMENTS):
        a0 = 2 * math.pi * i / SEGMENTS
        a1 = 2 * math.pi * (i + 1) / SEGMENTS
        ps = [(0.0, 0.0, POOL_Z), at(a0, POOL_R + 0.02, POOL_Z), at(a1, POOL_R + 0.02, POOL_Z)]
        pool.face(ps, [(0.5 + 0.5 * x, 0.5 - 0.5 * y) for x, y, _ in ps])
    ring.write("ring.obj")
    inner.write("inner.obj")
    pool.write("pool.obj")
    hot = Mesh("a hot spot's quad, 1 wide, flat on the water")
    hot.face([(-0.5, -0.5, 0.0), (0.5, -0.5, 0.0), (0.5, 0.5, 0.0), (-0.5, 0.5, 0.0)],
             [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)])
    hot.write("hot.obj")
    shaft = Mesh("the shaft's quad, 1 wide and 1 high, its foot at z = 0, facing -y")
    shaft.face([(-0.5, 0.0, 0.0), (0.5, 0.0, 0.0), (0.5, 0.0, 1.0), (-0.5, 0.0, 1.0)],
               [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)])
    shaft.write("shaft.obj")


if __name__ == "__main__":
    write()
    print("wrote ring.obj, inner.obj, pool.obj, hot.obj, shaft.obj")
