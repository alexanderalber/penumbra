"""Writes rock.obj and shard.obj for mid_debris. Run with no arguments."""
import math, random
from pathlib import Path
OUT = Path(__file__).resolve().parent
t = (1 + 5 ** 0.5) / 2
V = [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0), (0, -1, t), (0, 1, t), (0, -1, -t), (0, 1, -t),
     (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]
F = [(0, 11, 5), (0, 5, 1), (0, 1, 7), (0, 7, 10), (0, 10, 11), (1, 5, 9), (5, 11, 4), (11, 10, 2),
     (10, 7, 6), (7, 1, 8), (3, 9, 4), (3, 4, 2), (3, 2, 6), (3, 6, 8), (3, 8, 9), (4, 9, 5),
     (2, 4, 11), (6, 2, 10), (8, 6, 7), (9, 8, 1)]
V = [tuple(c / math.sqrt(sum(x * x for x in v)) for c in v) for v in V]
# Plain icosahedron faces: facets read as a rock in a wireframe, a subdivided one as a ball.
F2 = F
rng = random.Random(11)
bumps = [(tuple(rng.uniform(-1, 1) for _ in range(3)), rng.uniform(0.1, 0.25)) for _ in range(7)]
P = []
for v in V:
    r = 1.0
    for d, a in bumps:
        n = math.sqrt(sum(x * x for x in d))
        r -= a * max(0.0, sum(v[i] * d[i] / n for i in range(3))) ** 3
    r *= rng.uniform(0.75, 1.15)
    P.append((0.5 * v[0] * r * 1.1, 0.5 * v[1] * r * 0.95, 0.5 * v[2] * r * 0.8))
lines = ["# A low-poly rock about one unit across, centred on the origin, z up: an icosahedron with",
         "# its corners pushed in and out, squashed a little. mid_debris scales and turns one per rock.",
         "# Written by a script from a fixed seed."]
lines += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in P]
lines += [f"f {a + 1} {b + 1} {c + 1}" for a, b, c in F2]
(OUT / "rock.obj").write_text("\n".join(lines) + "\n", encoding="utf-8")
shard = ["# A shard of a burst rock: a thin, uneven tetrahedron about one unit long along y,",
         "# centred on the origin, z up.",
         "v 0.0000 0.5000 0.0000", "v -0.3000 -0.4000 -0.1500", "v 0.3500 -0.3000 -0.2000", "v 0.0500 -0.3500 0.3000",
         "f 1 2 3", "f 1 3 4", "f 1 4 2", "f 2 4 3"]
(OUT / "shard.obj").write_text("\n".join(shard) + "\n", encoding="utf-8")
