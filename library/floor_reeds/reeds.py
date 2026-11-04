"""Writes floor_reeds' model next to this file: reeds.obj, a clump of reeds standing on z = 0, z up,
about one unit tall. Run it with no arguments.

Each blade is a chain of thin quads from its foot to its tip, bending outwards from the clump's
middle; the wireframe draws each chain as one line. Two blades carry a seed head, a short wider
quad near the top. A corner's v is how far up its blade it lies (0 at the foot, 1 at the tip),
for a ramp that darkens the blade and lights its tip.
"""

import math
import random
from pathlib import Path

HERE = Path(__file__).resolve().parent

BLADES = 9
SEGMENTS = 4
# A blade's width at its foot, in units; it narrows to a third at the tip.
WIDTH = 0.012
# Heights of the blades (units), and how far a tip bends out, as a share of its height.
LOW, HIGH = 0.55, 1.0
BEND_LOW, BEND_HIGH = 0.08, 0.32
# The seed heads: blades that carry one, where it starts (share of the height), length, width.
HEADS = (2, 6)
HEAD_AT, HEAD_LEN, HEAD_W = 0.78, 0.13, 0.03


def main():
    rnd = random.Random(7)
    vs, vts, fs = [], [], []

    def quad(a, b, c, d):
        base = len(vs)
        for (p, v) in (a, b, c, d):
            vs.append(p)
            vts.append(v)
        fs.append((base, base + 1, base + 2, base + 3))

    for i in range(BLADES):
        turn = 2 * math.pi * (i + 0.5 * rnd.random()) / BLADES
        foot = 0.06 * rnd.random()
        h = rnd.uniform(LOW, HIGH)
        bend = rnd.uniform(BEND_LOW, BEND_HIGH) * h
        cx, cy = math.cos(turn), math.sin(turn)
        # The blade's width lies across its bend.
        wx, wy = -cy, cx

        def at(s):
            r = foot + bend * s * s
            return (r * cx, r * cy, h * s * (1 - 0.15 * bend * s))

        for k in range(SEGMENTS):
            s0, s1 = k / SEGMENTS, (k + 1) / SEGMENTS
            p0, p1 = at(s0), at(s1)
            w0 = 0.5 * WIDTH * (1 - 0.66 * s0)
            w1 = 0.5 * WIDTH * (1 - 0.66 * s1)
            quad(((p0[0] - wx * w0, p0[1] - wy * w0, p0[2]), s0), ((p0[0] + wx * w0, p0[1] + wy * w0, p0[2]), s0),
                 ((p1[0] + wx * w1, p1[1] + wy * w1, p1[2]), s1), ((p1[0] - wx * w1, p1[1] - wy * w1, p1[2]), s1))
        if i in HEADS:
            s0, s1 = HEAD_AT, HEAD_AT + HEAD_LEN
            p0, p1 = at(s0), at(min(s1, 1.0))
            w = 0.5 * HEAD_W
            quad(((p0[0] - wx * w, p0[1] - wy * w, p0[2]), 1.0), ((p0[0] + wx * w, p0[1] + wy * w, p0[2]), 1.0),
                 ((p1[0] + wx * w, p1[1] + wy * w, p1[2]), 1.0), ((p1[0] - wx * w, p1[1] - wy * w, p1[2]), 1.0))

    lines = ["# floor_reeds: a clump of reeds, thin quads; v is how far up its blade a corner lies"]
    lines += [f"v {x:.4f} {y:.4f} {z:.4f}" for (x, y, z) in vs]
    lines += [f"vt 0.5 {v:.4f}" for v in vts]
    lines += ["f " + " ".join(f"{i + 1}/{i + 1}" for i in f) for f in fs]
    (HERE / "reeds.obj").write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")
    print(f"wrote reeds.obj: {BLADES} blades, {len(fs)} quads")


if __name__ == "__main__":
    main()
