"""Writes floor_gates' models next to this file. Run it with no arguments.

gate_body.obj: a gate in a box one unit wide (x), deep (y) and high (z), z up, its front facing
+y: two pillars with a capital each, a lintel across them and a stepped crest on the lintel. The
placement shader (gates.wgsl) scales it so the opening is as wide as the lane's free path plus
the camera's. Its base lies a little below z = 0, so the water line cuts it. Each corner's u is
its light from the band behind the gate, looked up in a ramp strip at the bottom of the texture
(face.wgsl, v 0.97 to 1).

gate_face.obj: the carved fronts: the lintel's (u 0 to 1, v 0 to 0.16 of the texture) and the
two pillars' (u 0 to 0.125 the left one, 0.125 to 0.25 the right one, v 0.18 to 0.96).
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

RAMP_V = 0.985
BASE = -0.05
# x of a pillar's inner and outer face, of the lintel's end; z of the capital, the lintel, the crest.
PX = (0.398, 0.475)
LX = 0.5
ZC = 0.76
ZL = (0.8, 0.91)
CREST = [(0.3, 0.955), (0.15, 1.0)]
HY = 0.5
LINTEL_V = (0.0, 0.16)
PILLAR_V = (0.18, 0.96)
BAND = (0.0, -1.0, 0.35)


def h3(i, j, k, s=0):
    n = (i * 73856093) ^ (j * 19349663) ^ (k * 83492791) ^ (s * 2654435761)
    n &= 0xFFFFFFFF
    n = ((n >> 13) ^ n) * 1274126177 & 0xFFFFFFFF
    return (((n >> 16) ^ n) & 0xFFFF) / 65535.0


def noise(x, y, z, s=0):
    i, j, k = math.floor(x), math.floor(y), math.floor(z)
    fx, fy, fz = x - i, y - j, z - k
    sx, sy, sz = [t * t * (3 - 2 * t) for t in (fx, fy, fz)]

    def lerp(a, b, t):
        return a + (b - a) * t
    c = [[[h3(i + a, j + b, k + cc, s) for cc in (0, 1)] for b in (0, 1)] for a in (0, 1)]
    return lerp(lerp(lerp(c[0][0][0], c[0][0][1], sz), lerp(c[0][1][0], c[0][1][1], sz), sy),
                lerp(lerp(c[1][0][0], c[1][0][1], sz), lerp(c[1][1][0], c[1][1][1], sz), sy), sx) * 2 - 1


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def norm(a):
    ln = math.sqrt(dot(a, a)) or 1.0
    return (a[0] / ln, a[1] / ln, a[2] / ln)


def smooth(e0, e1, x):
    t = min(max((x - e0) / (e1 - e0), 0.0), 1.0)
    return t * t * (3 - 2 * t)


def clamp01(x):
    return min(max(x, 0.0), 1.0)


def jitter(p, seed, amp=0.004):
    return (p[0] + amp * noise(p[0] * 9, p[1] * 9, p[2] * 9, seed),
            p[1] + amp * noise(p[0] * 9 + 3, p[1] * 9, p[2] * 9, seed),
            p[2] + amp * noise(p[0] * 9, p[1] * 9 + 5, p[2] * 9, seed))


class Mesh:
    def __init__(self, comment):
        self.comment = comment
        self.v, self.t, self.f = [], [], []

    def face(self, pts, uvs):
        bv, bt = len(self.v), len(self.t)
        self.v += list(pts)
        self.t += list(uvs)
        self.f.append([(bv + i, bt + i) for i in range(len(pts))])

    def write(self, name):
        out = [f"# floor_gates: {self.comment}"]
        out += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in self.v]
        # The loader turns v upside down: the texture's top row is v 1 in the file.
        out += [f"vt {u:.4f} {1.0 - w:.4f}" for u, w in self.t]
        out += ["f " + " ".join(f"{a + 1}/{b + 1}" for a, b in f) for f in self.f]
        (HERE / name).write_text("\n".join(out) + "\n", encoding="ascii", newline="\n")


def light(p, n):
    """Rims where the band's light from behind meets the edges the camera sees: the pillars'
    inner back corners, the top faces at the back, the lintel's lower back edge."""
    back = smooth(-0.1, 0.5, -p[1])
    corner = smooth(0.2, 0.5, -p[1])
    up = max(n[2], 0.0)
    inner = abs(p[0]) < 0.44
    side = abs(n[0]) > 0.5
    rim = 0.0
    if side:
        rim = (0.5 if inner else 0.35) * corner ** 3 * (0.4 + 0.6 * clamp01(p[2]))
    return rim + 0.55 * up * back + 0.3 * max(0.0, dot(n, norm(BAND))) ** 4


SPLIT = 4


def lerp4(pts, s, t):
    """The point at (s, t) of a quad's corners 0 1 2 3."""
    a = [pts[0][k] + (pts[1][k] - pts[0][k]) * s for k in range(3)]
    b = [pts[3][k] + (pts[2][k] - pts[3][k]) * s for k in range(3)]
    return tuple(a[k] + (b[k] - a[k]) * t for k in range(3))


def box(body, face, x0, x1, y0, y1, z0, z1, seed, front=None):
    def P(x, y, z):
        return jitter((x, y, z), seed)
    c = (0.5 * (x0 + x1), 0.5 * (y0 + y1), 0.5 * (z0 + z1))
    quads = [
        ([P(x0, y1, z0), P(x1, y1, z0), P(x1, y1, z1), P(x0, y1, z1)], True),
        ([P(x1, y0, z0), P(x0, y0, z0), P(x0, y0, z1), P(x1, y0, z1)], False),
        ([P(x0, y0, z0), P(x0, y1, z0), P(x0, y1, z1), P(x0, y0, z1)], False),
        ([P(x1, y1, z0), P(x1, y0, z0), P(x1, y0, z1), P(x1, y1, z1)], False),
        ([P(x0, y1, z1), P(x1, y1, z1), P(x1, y0, z1), P(x0, y0, z1)], False),
        ([P(x0, y0, z0), P(x1, y0, z0), P(x1, y1, z0), P(x0, y1, z0)], False),
    ]
    for pts, is_front in quads:
        if is_front and front:
            face.face(pts, [front(x0, z0), front(x1, z0), front(x1, z1), front(x0, z1)])
            continue
        a = sub(pts[1], pts[0])
        b = sub(pts[2], pts[0])
        n = norm(cross(a, b))
        if dot(n, sub(pts[0], c)) < 0.0:
            n = (-n[0], -n[1], -n[2])
        # In small pieces, so the light can change across a face: a rim stays narrow.
        for i in range(SPLIT):
            for j in range(SPLIT):
                q = [lerp4(pts, (i + di) / SPLIT, (j + dj) / SPLIT) for di, dj in ((0, 0), (1, 0), (1, 1), (0, 1))]
                body.face(q, [(clamp01(light(p, n)) * 0.98 + 0.01, RAMP_V) for p in q])


def write():
    body = Mesh("a gate, its body; u is the light from the band")
    face = Mesh("a gate, the carved fronts of its lintel and pillars")
    for side, (cell, seed) in ((-1, (0.0, 3)), (1, (0.125, 4))):
        x0, x1 = (-PX[1], -PX[0]) if side < 0 else (PX[0], PX[1])

        def pillar_uv(x, z, x0=x0, x1=x1, cell=cell):
            lx = (x - x0) / (x1 - x0)
            ly = 1.0 - (z - BASE) / (ZC - BASE)
            return (cell + 0.125 * (0.01 + 0.98 * clamp01(lx)), PILLAR_V[0] + (PILLAR_V[1] - PILLAR_V[0]) * clamp01(ly))
        box(body, face, x0, x1, -HY, HY, BASE, ZC, seed, pillar_uv)
        # the capital: a little wider and deeper than the pillar
        box(body, face, x0 - 0.008, x1 + 0.008, -HY * 1.06, HY * 1.06, ZC, ZL[0], seed + 10)

    def lintel_uv(x, z):
        lx = 0.5 + x / (2 * LX)
        ly = 1.0 - (z - ZL[0]) / (ZL[1] - ZL[0])
        return (0.002 + 0.996 * clamp01(lx), LINTEL_V[0] + (LINTEL_V[1] - LINTEL_V[0]) * clamp01(ly))
    box(body, face, -LX, LX, -HY * 1.1, HY * 1.1, ZL[0], ZL[1], 7, lintel_uv)
    z = ZL[1]
    for i, (hx, zt) in enumerate(CREST):
        box(body, face, -hx, hx, -HY * (0.85 - 0.15 * i), HY * (0.85 - 0.15 * i), z, zt, 8 + i)
        z = zt
    body.write("gate_body.obj")
    face.write("gate_face.obj")


if __name__ == "__main__":
    write()
    print("wrote gate_body.obj, gate_face.obj")
