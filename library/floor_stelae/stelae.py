"""Writes floor_stelae's models next to this file. Run it with no arguments.

Four stones, each in a box one unit wide (x), deep (y) and high (z), z up, its front facing +y.
The placement shader (stelae.wgsl) scales each to metres and mirrors it on the left of the lane,
so the +x side always faces the band on the horizon: the light baked into the stones comes from
behind (-y) and from +x.

- a: a weathered monolith, lofted from rounded rings with noise, tapering to an irregular
  rounded top.
- c: a slender stone with a slanted broken top and low relief panels on its front.
- g: monolith a broken at 0.74 of its height, leaning and sunk 0.2 into the water, with its
  broken-off top lying in the water beside it.
- e: an upright disc on a stepped pedestal.

Each stone is split into a body (`*_body.obj`) and its carved front (`*_face*.obj`), drawn by
separate geo.tex_instance nodes with separate light, so the swell can lift the carving more than
the rims. Both read one texture (face.wgsl, 2048 x 2048): the body's u is its baked light,
looked up in a ramp strip at the bottom of the texture; a front's uvs lie in its cell:

    u 0    to 0.25   cell of monolith a (and g), glyph 1
    u 0.25 to 0.5    the same, glyph 2
    u 0.5  to 0.625  the relief of stone c
    u 0.625 to 1     the disc of stone e, v 0 to 0.375
    v 0.97 to 1      the light ramp: u is the light

c_rim.obj: thin strips along c's band-side back corner and the back of its top, taken from c's
own vertices, so the rim lines (mat.wireframe) lie on the silhouette.
e_ring1.obj to e_ring4.obj: e's front in four rings from the middle out, so the swell can fill
it from the middle.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

RAMP_V = 0.985
CELL_A = (0.0, 0.25)
CELL_A2 = (0.25, 0.5)
CELL_C = (0.5, 0.625)
CELL_E = (0.625, 1.0)
CELL_V = 0.96
E_V = 0.375
# The rings that split e's front: in the flat stone between two carved circles.
E_SPLIT = [0.225, 0.52, 0.76]
# Where the light comes from in a stone's own frame: behind it and from +x, a little from above.
BAND = (0.45, -1.0, 0.3)


# --- small vector and noise helpers -----------------------------------------------------------

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


def fbm(x, y, z, s=0, octaves=3):
    a, f, t = 1.0, 1.0, 0.0
    for o in range(octaves):
        t += a * noise(x * f, y * f, z * f, s + o * 17)
        a *= 0.5
        f *= 2.03
    return t


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def norm(a):
    ln = math.sqrt(sum(x * x for x in a)) or 1.0
    return (a[0] / ln, a[1] / ln, a[2] / ln)


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def smooth(e0, e1, x):
    t = min(max((x - e0) / (e1 - e0), 0.0), 1.0)
    return t * t * (3 - 2 * t)


def clamp01(x):
    return min(max(x, 0.0), 1.0)


def normal(pts):
    n = (0.0, 0.0, 0.0)
    for i in range(1, len(pts) - 1):
        c = cross(sub(pts[i], pts[0]), sub(pts[i + 1], pts[0]))
        n = (n[0] + c[0], n[1] + c[1], n[2] + c[2])
    return norm(n)


def rotate(p, rx=0.0, ry=0.0, rz=0.0):
    """Turns p about x, then y, then z, in degrees."""
    x, y, z = p
    a = math.radians(rx)
    y, z = y * math.cos(a) - z * math.sin(a), y * math.sin(a) + z * math.cos(a)
    a = math.radians(ry)
    x, z = x * math.cos(a) + z * math.sin(a), -x * math.sin(a) + z * math.cos(a)
    a = math.radians(rz)
    x, y = x * math.cos(a) - y * math.sin(a), x * math.sin(a) + y * math.cos(a)
    return (x, y, z)


class Mesh:
    def __init__(self, comment):
        self.comment = comment
        self.v, self.t, self.f = [], [], []

    def face(self, pts, uvs):
        bv, bt = len(self.v), len(self.t)
        self.v += list(pts)
        self.t += list(uvs)
        self.f.append([(bv + i, bt + i) for i in range(len(pts))])

    def lit(self, pts, light, centre=None):
        """A face of the body: each corner's u is its light from the band, looked up in the ramp.
        With `centre`, the face's normal is turned to point away from it."""
        n = normal(pts)
        if centre is not None and dot(n, sub(pts[0], centre)) < 0.0:
            n = (-n[0], -n[1], -n[2])
        self.face(pts, [(clamp01(light(p, n)) * 0.98 + 0.01, RAMP_V) for p in pts])

    def write(self, name):
        out = [f"# floor_stelae: {self.comment}"]
        out += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in self.v]
        # The loader turns v upside down: the texture's top row is v 1 in the file.
        out += [f"vt {u:.4f} {1.0 - w:.4f}" for u, w in self.t]
        out += ["f " + " ".join(f"{a + 1}/{b + 1}" for a, b in f) for f in self.f]
        (HERE / name).write_text("\n".join(out) + "\n", encoding="ascii", newline="\n")


def band_light(n):
    return max(0.0, dot(n, norm(BAND)))


# --- lofts ------------------------------------------------------------------------------------

def squircle_rings(zs, half, e, amp, seed, front_test, top=None, n_around=36):
    """Rings of a rounded box from the foot up. half(z) -> (hx, hy); amp(front) the noise.
    Returns rings of points and, per point, whether it lies on the carved front."""
    rings, tags = [], []
    for i, z in enumerate(zs):
        hx, hy = half(max(z, 0.0))
        ring, tg = [], []
        for j in range(n_around):
            a = 2 * math.pi * j / n_around
            c, s = math.cos(a), math.sin(a)
            x = hx * math.copysign(abs(c) ** (2 / e), c)
            y = hy * math.copysign(abs(s) ** (2 / e), s)
            front = front_test(s, z)
            d = fbm(x * 3.1 + seed, y * 3.1, z * 2.3, seed)
            x += c * amp(front) * d
            y += s * amp(front) * d
            zt = top(i, x, y, z) if top else z
            ring.append((x, y, zt))
            tg.append(front)
        rings.append(ring)
        tags.append(tg)
    return rings, tags


def loft_quads(rings, tags, cap=True):
    """The loft's quads as (points, on the front) and its top cap's triangles."""
    n = len(rings[0])
    quads = []
    for i in range(len(rings) - 1):
        a, b = rings[i], rings[i + 1]
        for j in range(n):
            k = (j + 1) % n
            front = tags[i][j] and tags[i][k] and tags[i + 1][j] and tags[i + 1][k]
            quads.append(([a[j], a[k], b[k], b[j]], front))
    if cap:
        top = rings[-1]
        c = tuple(sum(p[q] for p in top) / n for q in range(3))
        for j in range(n):
            quads.append(([top[j], top[(j + 1) % n], c], False))
    return quads


def bottom_cap(ring):
    n = len(ring)
    c = tuple(sum(p[q] for p in ring) / n for q in range(3))
    return [([ring[(j + 1) % n], ring[j], c], False) for j in range(n)]


def front_uv(cell, p, width, top):
    """A point of the front in its cell: x across the front's width, z down from `top`."""
    lx = clamp01(0.5 + p[0] / width)
    ly = clamp01(1.0 - p[2] / top)
    return (cell[0] + (cell[1] - cell[0]) * (0.004 + 0.992 * lx), ly * CELL_V)


# --- a: the weathered monolith ----------------------------------------------------------------

A_ZS = [-0.05, 0.08, 0.2, 0.35, 0.5, 0.62, 0.74, 0.84, 0.9, 0.95, 0.985, 1.0]
A_SEED = 14


def a_half(z, seed=A_SEED):
    hx = 0.5 - 0.07 * z + 0.02 * fbm(0.3, z * 3, seed, seed)
    hy = 0.5 - 0.1 * z
    if z > 0.84:
        t = (z - 0.84) / 0.16
        s = math.sqrt(max(1 - t * t, 0.0)) * 0.85 + 0.15 * (1 - t)
        hx *= max(s, 0.05)
        hy *= max(0.55 + 0.45 * s, 0.05)
    return hx, hy


def a_light(p, n):
    """Rims where the band's light from behind meets the edges the camera sees: the back corner
    of the side towards the lane (-x), the front corner of the band's side (+x), the back of the
    top, and facets turned to the band."""
    back = smooth(-0.05, 0.45, -p[1])
    front = smooth(0.2, 0.48, p[1])
    xm = smooth(0.25, 0.48, -p[0])
    xp = smooth(0.25, 0.48, p[0])
    up = max(n[2], 0.0)
    return (0.75 * (xm * back) ** 2 + 0.6 * (xp * front) ** 2 + 0.45 * up * up * back
            + 0.6 * band_light(n) ** 4)


def a_rings(seed, zs=A_ZS, top_cut=None):
    def top(i, x, y, z):
        if i == len(zs) - 1:
            if top_cut:
                return top_cut(x, y, z)
            return z - 0.08 * (0.5 + 0.5 * fbm(x * 4, y * 4, 1.0, seed + 5))
        return z
    return squircle_rings(zs, lambda z: a_half(z, seed), 4.5, lambda f: 0.012 if f else 0.08, seed,
                          lambda s, z: s > 0.62 and z < 0.9, top)


def write_a():
    rings, tags = a_rings(A_SEED)
    quads = loft_quads(rings, tags)
    body = Mesh("monolith a, its body; u is the light from the band")
    faces = [Mesh("monolith a, its front with glyph 1"), Mesh("monolith a, its front with glyph 2")]
    for pts, front in quads:
        if front:
            for m, cell in zip(faces, (CELL_A, CELL_A2)):
                m.face(pts, [front_uv(cell, p, 0.86, 0.9) for p in pts])
        else:
            body.lit(pts, a_light)
    body.write("a_body.obj")
    faces[0].write("a_face1.obj")
    faces[1].write("a_face2.obj")


# --- c: the slender stone -----------------------------------------------------------------------

C_ZS = [-0.05, 0.1, 0.25, 0.4, 0.55, 0.7, 0.82, 0.92, 0.97, 1.0]
C_SEED = 19
C_N = 44


def c_half(z):
    return 0.5 - 0.13 * z, 0.5 - 0.12 * z


def c_top(i, x, y, z):
    if i == len(C_ZS) - 1:
        return 0.94 + 0.06 * (0.5 + x) + 0.02 * fbm(x * 6, y * 6, 2.0, C_SEED)
    if i == len(C_ZS) - 2:
        return min(z, 0.92 + 0.05 * (0.5 + x))
    return z


def c_light(p, n):
    back = smooth(0.2, 0.47, -p[1])
    front = smooth(0.2, 0.47, p[1])
    xm = smooth(0.3, 0.45, -p[0])
    xp = smooth(0.3, 0.45, p[0])
    top = max(n[2], 0.0) * smooth(-0.2, 0.4, -p[1])
    return 0.6 * (xm * back) ** 2 + 0.5 * (xp * front) ** 2 + 0.6 * top + 0.4 * band_light(n) ** 4


def write_c():
    rings, tags = squircle_rings(C_ZS, c_half, 7.0, lambda f: 0.006 if f else 0.025, C_SEED,
                                 lambda s, z: s > 0.55 and z < 0.95, c_top, n_around=C_N)
    body = Mesh("stone c, its body; u is the light from the band")
    face = Mesh("stone c, its front with the relief")
    for pts, front in loft_quads(rings, tags):
        if front:
            face.face(pts, [front_uv(CELL_C, p, 0.84, 0.95) for p in pts])
        else:
            body.lit(pts, c_light)
    body.write("c_body.obj")
    face.write("c_face.obj")
    # The rims: the vertex of each ring nearest a corner the camera sees on the silhouette (the
    # back corner towards the lane, -x, and the front corner of the band's side, +x), pushed out a
    # hair from the stone's axis, and the top ring's back half.
    rim = Mesh("stone c, its rim lines on the edges the band lights")
    out = 1.012

    def nearest(angle):
        return min(range(C_N), key=lambda j: abs(((2 * math.pi * j / C_N) - angle + math.pi) % (2 * math.pi) - math.pi))

    def pushed(p, i):
        return (p[0] * out, p[1] * out, p[2] + (0.002 if i == len(rings) - 1 else 0.0))

    def strip(path, gain):
        for a, b in zip(path, path[1:]):
            d = 0.003
            la, lb = gain * (0.45 + 0.55 * clamp01(a[2])), gain * (0.45 + 0.55 * clamp01(b[2]))
            rim.face([a, b, (b[0], b[1] + d, b[2]), (a[0], a[1] + d, a[2])], [(la, 0.5), (lb, 0.5), (lb, 0.5), (la, 0.5)])

    back = nearest(1.25 * math.pi)
    strip([pushed(rings[i][back], i) for i in range(len(rings))], 0.9)
    front = nearest(0.25 * math.pi)
    strip([pushed(rings[i][front], i) for i in range(len(rings) - 1)], 0.75)
    top = rings[-1]
    path = [pushed(top[back], len(rings) - 1)]
    k = back
    for _ in range(C_N // 2):
        k = (k + 1) % C_N
        if top[k][1] > -0.05:
            break
        path.append(pushed(top[k], len(rings) - 1))
    strip(path, 1.0)
    rim.write("c_rim.obj")


# --- g: the broken monolith -------------------------------------------------------------------

G_SEED = 35
G_LEAN = (-9.0, 16.0, 0.0)
G_SINK = -0.2
# g's size in metres at the default height, width and depth: its fragment is built in metres.
G_SIZE = (1.0, 0.55, 2.8)


def g_break(seed):
    def cut(x, y, z):
        return z - 0.1 * (0.5 + 0.5 * fbm(x * 6, y * 6, 3.0, seed + 9)) - 0.12 * (x + 0.5)
    return cut


def write_g():
    zs = [-0.05, 0.1, 0.25, 0.4, 0.55, 0.66, 0.74]
    rings, tags = a_rings(G_SEED, zs, g_break(G_SEED))
    quads = loft_quads(rings, tags)

    def place(p):
        x, y, z = rotate(p, *G_LEAN)
        return (x, y, z + G_SINK)
    body = Mesh("stone g, its leaning trunk and the fragment; u is the light from the band")
    faces = [Mesh("stone g, its front with glyph 1"), Mesh("stone g, its front with glyph 2")]
    for pts, front in quads:
        moved = [place(p) for p in pts]
        if front:
            for m, cell in zip(faces, (CELL_A, CELL_A2)):
                m.face(moved, [front_uv(cell, p, 0.86, 0.9) for p in pts])
        else:
            body.lit(moved, a_light)
    # The fragment: the top that broke off, from the break to its rounded crown, with a jagged
    # underside. It lies in the water beside the trunk, crown up and away, about half sunk.
    fz = [0.7, 0.78, 0.84, 0.9, 0.95, 0.985, 1.0]
    frings, ftags = a_rings(G_SEED, fz)
    for i, ring in enumerate(frings[:1]):
        for j, p in enumerate(ring):
            jag = 0.07 * (0.5 + 0.5 * fbm(p[0] * 6, p[1] * 6, 3.0, G_SEED + 9)) - 0.12 * (p[0] + 0.5)
            ring[j] = (p[0], p[1], p[2] + jag * 0.6)
    fquads = loft_quads(frings, [[False] * len(r) for r in ftags]) + bottom_cap(frings[0])
    w, t, h = G_SIZE

    def lay(p):
        # metres, from the foot of the break
        m = (p[0] * w, p[1] * t, (p[2] - 0.7) * h)
        m = rotate(m, 12.0, 72.0, 28.0)
        m = (m[0] + 0.95, m[1] - 0.35, m[2] - 0.32)
        return (m[0] / w, m[1] / t, m[2] / h)

    def frag_light(p, n):
        up = max(n[2], 0.0)
        return 0.3 * band_light(n) ** 4 + 0.12 * up ** 3
    for pts, _ in fquads:
        body.lit([lay(p) for p in pts], frag_light)
    body.write("g_body.obj")
    faces[0].write("g_face1.obj")
    faces[1].write("g_face2.obj")


# --- e: the disc --------------------------------------------------------------------------------

E_N = 72
E_CZ, E_R = 0.6, 0.4
E_HY = 0.5
E_SEED = 8


def jitter(p, seed, amp=0.006):
    return (p[0] + amp * fbm(p[0] * 7, p[1] * 7, p[2] * 7, seed),
            p[1] + amp * fbm(p[0] * 7 + 3, p[1] * 7, p[2] * 7, seed),
            p[2] + amp * fbm(p[0] * 7, p[1] * 7 + 5, p[2] * 7, seed))


def box(m, x0, x1, y0, y1, z0, z1, light, seed):
    def P(x, y, z):
        return jitter((x, y, z), seed)
    c = (0.5 * (x0 + x1), 0.5 * (y0 + y1), 0.5 * (z0 + z1))
    m.lit([P(x0, y1, z0), P(x1, y1, z0), P(x1, y1, z1), P(x0, y1, z1)], light, c)
    m.lit([P(x1, y0, z0), P(x0, y0, z0), P(x0, y0, z1), P(x1, y0, z1)], light, c)
    m.lit([P(x0, y0, z0), P(x0, y1, z0), P(x0, y1, z1), P(x0, y0, z1)], light, c)
    m.lit([P(x1, y1, z0), P(x1, y0, z0), P(x1, y0, z1), P(x1, y1, z1)], light, c)
    m.lit([P(x0, y1, z1), P(x1, y1, z1), P(x1, y0, z1), P(x0, y0, z1)], light, c)


def write_e():
    body = Mesh("disc e, its pedestal, back and rim; u is the light from the band")

    def step_light(p, n):
        up = max(n[2], 0.0)
        return 0.4 * up * up * smooth(-0.3, 0.5, -p[1]) + 0.3 * band_light(n) ** 4

    box(body, -0.36, 0.36, -0.55, 0.55, -0.05, 0.11, step_light, E_SEED)
    box(body, -0.28, 0.28, -0.45, 0.45, 0.11, 0.2, step_light, E_SEED + 1)
    ring = [(E_R * math.cos(2 * math.pi * j / E_N), E_CZ + E_R * math.sin(2 * math.pi * j / E_N)) for j in range(E_N)]
    for j in range(E_N):
        a, b = ring[j], ring[(j + 1) % E_N]
        body.lit([(0.0, -E_HY, E_CZ), (a[0], -E_HY, a[1]), (b[0], -E_HY, b[1])], lambda p, n: 0.0)

    def rim(p, n):
        top = smooth(-0.2, 1.0, n[2])
        back = smooth(-0.5, 0.5, -p[1])
        side = smooth(-0.3, 1.0, n[0])
        return 0.75 * top * top * back + 0.4 * side * side * back + 0.05 * back
    for j in range(E_N):
        a, b = ring[j], ring[(j + 1) % E_N]
        for ya, yb in ((-E_HY, 0.0), (0.0, E_HY)):
            body.lit([(b[0], ya, b[1]), (a[0], ya, a[1]), (a[0], yb, a[1]), (b[0], yb, b[1])], rim, (0.0, 0.5 * (ya + yb), E_CZ))
    body.write("e_body.obj")

    def uv(x, z):
        lx = 0.5 + x / (2 * E_R)
        ly = 0.5 - (z - E_CZ) / (2 * E_R)
        return (CELL_E[0] + (CELL_E[1] - CELL_E[0]) * lx, ly * E_V)

    def at(r, j):
        a = 2 * math.pi * j / E_N
        return (E_R * r * math.cos(a), E_HY, E_CZ + E_R * r * math.sin(a))
    radii = [0.0] + E_SPLIT + [1.0]
    for k in range(4):
        m = Mesh(f"disc e, ring {k + 1} of its front from the middle out")
        r0, r1 = radii[k], radii[k + 1]
        for j in range(E_N):
            if r0 == 0.0:
                pts = [(0.0, E_HY, E_CZ), at(r1, j + 1), at(r1, j)]
            else:
                pts = [at(r0, j), at(r0, j + 1), at(r1, j + 1), at(r1, j)][::-1]
            m.face(pts, [uv(p[0], p[2]) for p in pts])
        m.write(f"e_ring{k + 1}.obj")


if __name__ == "__main__":
    write_a()
    write_c()
    write_g()
    write_e()
    print("wrote a_body, a_face1, a_face2, c_body, c_face, c_rim, g_body, g_face1, g_face2, e_body, e_ring1 to e_ring4")
