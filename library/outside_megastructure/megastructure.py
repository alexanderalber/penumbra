"""Writes the models of outside_megastructure next to this file. Run it with no arguments.

The near part is a lattice around stage_ascent's tower, centred on the tower's axis, z up:
a deck every 12 m from z = -24 to z = 60. Each deck is an octagonal collar of box girders just
outside the tower's wall (7 to 7.8 m from the axis), with heavy nodes at its corners. Lattice
columns join every second corner from deck to deck. Two long trusses run out from each deck's collar,
on opposite sides; the pair turns by 135 degrees from deck to deck, so the decks repeat every
36 m. The element moves the lattice down by the climb, wrapped to 36 m, so it never ends.

The far part is a colossal ring around a glowing core, in the x-z plane, facing the camera
along y: an outer band of 32 segments (600 to 680 m from the centre), a thinner lattice ring
inside it (505 to 520 m) joined to it by struts, and four trusses from the lattice ring in
towards the core. The core is a sphere of 120 m, the corona three thin dials around it.

Each solid comes twice: *_fill.obj as closed boxes, drawn black so it hides what lies behind,
and *_edges.obj, the same boxes a hair larger, whose outlines the wireframe draws.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

DECK = 12.0
DECKS = range(-2, 6)
COLLAR_IN = 7.0
COLLAR_OUT = 7.8
COLLAR_HALF_Z = 0.6
TRUSS_END = 40.0
# The sides (of 8, counted from straight ahead) the two trusses of each deck leave from, for
# the deck's place in the repeat of three.
TRUSS_SIDES = [(0, 4), (3, 7), (6, 2)]
# How far the edges lie outside the solids: near in metres, far in metres too.
GROW_NEAR = 0.03
GROW_FAR = 1.5


def add(a, b, s=1.0):
    return (a[0] + s * b[0], a[1] + s * b[1], a[2] + s * b[2])


def unit(v):
    n = math.sqrt(v[0] ** 2 + v[1] ** 2 + v[2] ** 2)
    return (v[0] / n, v[1] / n, v[2] / n)


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


class Mesh:
    def __init__(self):
        self.verts, self.faces = [], []

    def hexa(self, corners):
        """A closed solid of eight corners: bottom four, then top four, each round the same
        way."""
        base = len(self.verts)
        self.verts += corners
        b = [base + i for i in range(4)]
        t = [base + 4 + i for i in range(4)]
        self.faces.append([b[3], b[2], b[1], b[0]])
        self.faces.append(t)
        for i in range(4):
            j = (i + 1) % 4
            self.faces.append([b[i], b[j], t[j], t[i]])

    def write(self, name, head):
        lines = [f"# {line}" for line in head]
        lines += [f"v {x:.3f} {y:.3f} {z:.3f}" for x, y, z in self.verts]
        lines += ["f " + " ".join(str(i + 1) for i in f) for f in self.faces]
        (HERE / name).write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def box(meshes, c, e1, e2, half, grows):
    """A box round centre c, along e1 and e2 (e3 their cross), with half sizes `half`, into
    each mesh with its own growth."""
    e1, e2 = unit(e1), unit(e2)
    e3 = cross(e1, e2)
    for mesh, g in zip(meshes, grows):
        h = [x + g for x in half]
        pts = []
        for s3 in (-1, 1):
            for s1, s2 in ((-1, -1), (1, -1), (1, 1), (-1, 1)):
                p = add(c, e1, s1 * h[0])
                p = add(p, e2, s2 * h[1])
                pts.append(add(p, e3, s3 * h[2]))
        mesh.hexa(pts)


def beam(meshes, a, b, side, up, grows):
    """A box from a to b, `side` and `up` metres across."""
    d = (b[0] - a[0], b[1] - a[1], b[2] - a[2])
    length = math.sqrt(d[0] ** 2 + d[1] ** 2 + d[2] ** 2)
    c = add(a, d, 0.5)
    e1 = unit(d)
    ref = (0.0, 0.0, 1.0) if abs(e1[2]) < 0.9 else (1.0, 0.0, 0.0)
    e2 = unit(cross(ref, e1))
    box(meshes, c, e1, e2, (0.5 * length, 0.5 * side, 0.5 * up), grows)


def ribbon(edges, a, b, width, normal):
    """A thin member from a to b as one flat quad, `width` across, lying in the plane with
    `normal`: its outline draws as a doubled line. Only an outline, no body: too thin to hide
    anything."""
    d = (b[0] - a[0], b[1] - a[1], b[2] - a[2])
    side = unit(cross(d, normal))
    h = 0.5 * width
    base = len(edges.verts)
    edges.verts += [add(a, side, -h), add(b, side, -h), add(b, side, h), add(a, side, h)]
    edges.faces.append([base, base + 1, base + 2, base + 3])


def around(r, a, z):
    """A point r from the axis at azimuth a (radians from straight ahead, turning to +x)."""
    return (r * math.sin(a), r * math.cos(a), z)


def near():
    fill, edges = Mesh(), Mesh()
    both, grows = (fill, edges), (0.0, GROW_NEAR)
    mid = 0.5 * (COLLAR_IN + COLLAR_OUT)
    half_side = mid * math.tan(math.pi / 8)
    corner_r = mid / math.cos(math.pi / 8)
    for j in DECKS:
        z = DECK * j
        for s in range(8):
            a = s * math.pi / 4
            radial = (math.sin(a), math.cos(a), 0.0)
            tangent = (math.cos(a), -math.sin(a), 0.0)
            # Three girders per side, with joints between them.
            piece = (2.0 * half_side - 1.3) / 3.0
            for i in range(3):
                off = -half_side + 0.65 + piece * (i + 0.5)
                c = add(around(mid, a, z), tangent, off)
                box(both, c, tangent, radial, (0.5 * piece - 0.08, 0.5 * (COLLAR_OUT - COLLAR_IN), COLLAR_HALF_Z), grows)
            # The node at the corner after this side.
            ca = a + math.pi / 8
            box(both, around(corner_r, ca, z), (math.sin(ca), math.cos(ca), 0.0), (math.cos(ca), -math.sin(ca), 0.0),
                (0.75, 0.75, 0.95), grows)
            # Lattice columns from every second corner up to the next deck.
            if s % 2 == 0 and j < max(DECKS):
                column(both, grows, corner_r, ca, z + 0.95, z + DECK - 0.95)
        for s in TRUSS_SIDES[j % 3]:
            a = s * math.pi / 4
            truss(both, grows, a, COLLAR_OUT, TRUSS_END, z)
    fill.write("near_fill.obj", [
        "The lattice of outside_megastructure around the tower, as closed boxes: a collar every 12 m",
        "from z -24 to 60, 7 to 7.8 m from the axis, columns and trusses. Drawn black.",
    ])
    edges.write("near_edges.obj", [
        "The lattice of outside_megastructure around the tower: the boxes of near_fill.obj a hair",
        "larger, whose outlines the wireframe draws.",
    ])


def column(both, grows, r, a, z0, z1):
    """A lattice column at azimuth a, r from the axis, from z0 to z1: four chords at the corners
    of a square 1.2 m across, and a zigzag of struts on each face, so it can be seen through."""
    radial = (math.sin(a), math.cos(a), 0.0)
    tangent = (math.cos(a), -math.sin(a), 0.0)
    h = 0.6
    corners = [add(add(around(r, a, 0.0), radial, sr * h), tangent, st * h) for sr, st in ((-1, -1), (1, -1), (1, 1), (-1, 1))]
    for c in corners:
        beam(both, (c[0], c[1], z0), (c[0], c[1], z1), 0.22, 0.22, grows)
    n = 6
    step = (z1 - z0) / n
    for f in range(4):
        p, q = corners[f], corners[(f + 1) % 4]
        normal = unit(cross((q[0] - p[0], q[1] - p[1], 0.0), (0.0, 0.0, 1.0)))
        for i in range(n):
            lo, hi = (p, q) if (i + f) % 2 == 0 else (q, p)
            ribbon(both[1], (lo[0], lo[1], z0 + step * i), (hi[0], hi[1], z0 + step * (i + 1)), 0.12, normal)


def truss(both, grows, a, r0, r1, z):
    """A truss along azimuth a from r0 to r1 at height z: two chords one above the other and a
    zigzag of struts between them."""
    half = 1.1
    beam(both, around(r0, a, z + half), around(r1, a, z + half), 0.5, 0.5, grows)
    beam(both, around(r0, a, z - half), around(r1, a, z - half), 0.5, 0.5, grows)
    step = 2.4
    n = int((r1 - r0) / step)
    for i in range(n):
        lo, hi = (z - half, z + half) if i % 2 == 0 else (z + half, z - half)
        ribbon(both[1], around(r0 + step * i, a, lo), around(r0 + step * (i + 1), a, hi), 0.18,
               (math.cos(a), -math.sin(a), 0.0))


def ring_point(r, a, y):
    """A point of the far ring's plane: r from its centre at angle a, y towards the camera's
    far side."""
    return (r * math.sin(a), y, r * math.cos(a))


def far():
    fill, edges = Mesh(), Mesh()
    both, grows = (fill, edges), (0.0, GROW_FAR)

    def band(r0, r1, half_y, a0, a1, mesh_grows):
        for mesh, g in mesh_grows:
            pts = []
            for y in (-half_y - g, half_y + g):
                pts += [ring_point(r0 - g, a0, y), ring_point(r1 + g, a0, y), ring_point(r1 + g, a1, y),
                        ring_point(r0 - g, a1, y)]
            mesh.hexa(pts)

    pairs = list(zip(both, grows))
    outer = 32
    for s in range(outer):
        place = 2.0 * math.pi / outer
        a0 = place * s
        span = place * 0.86
        for i in range(3):
            band(600.0, 680.0, 25.0, a0 + span * i / 3, a0 + span * (i + 1) / 3, pairs)
        # A strut from the lattice ring to the band's middle.
        am = a0 + 0.5 * span
        beam(both, ring_point(520.0, am, 0.0), ring_point(600.0, am, 0.0), 8.0, 8.0, grows)
    inner = 48
    for s in range(inner):
        place = 2.0 * math.pi / inner
        band(505.0, 520.0, 10.0, place * s, place * (s + 0.9), pairs)
    for k in range(4):
        a = math.pi / 4 + k * math.pi / 2
        tangent = (math.cos(a), 0.0, -math.sin(a))
        r0, r1 = 505.0, 215.0
        for side in (-12.0, 12.0):
            beam(both, add(ring_point(r0, a, 0.0), tangent, side), add(ring_point(r1, a, 0.0), tangent, side), 6.0, 6.0,
                 grows)
        n = 10
        for i in range(n):
            ra = r0 + (r1 - r0) * i / n
            rb = r0 + (r1 - r0) * (i + 1) / n
            sa, sb = (-12.0, 12.0) if i % 2 == 0 else (12.0, -12.0)
            beam(both, add(ring_point(ra, a, 0.0), tangent, sa), add(ring_point(rb, a, 0.0), tangent, sb), 3.0, 3.0, grows)
    fill.write("far_fill.obj", [
        "The far ring of outside_megastructure, as closed solids: a band of 32 segments 600 to 680 m",
        "from its centre, a lattice ring at 505 to 520 m, struts, and four trusses towards the core.",
        "In the x-z plane, facing along y. Drawn black.",
    ])
    edges.write("far_edges.obj", [
        "The far ring of outside_megastructure: the solids of far_fill.obj a hair larger, whose",
        "outlines the wireframe draws.",
    ])

    core = Mesh()
    rows, cols, r = 12, 24, 120.0
    pts = [[(r * math.sin(math.pi * i / rows) * math.sin(2 * math.pi * k / cols), r * math.cos(math.pi * i / rows),
             r * math.sin(math.pi * i / rows) * math.cos(2 * math.pi * k / cols)) for k in range(cols)] for i in range(rows + 1)]
    for row in pts:
        core.verts += row
    for i in range(rows):
        for k in range(cols):
            k1 = (k + 1) % cols
            core.faces.append([i * cols + k, i * cols + k1, (i + 1) * cols + k1, (i + 1) * cols + k])
    core.write("core.obj", ["The core of outside_megastructure: a sphere of 120 m round the far ring's centre."])

    corona = Mesh()
    for radius, width, n in ((150.0, 2.0, 72), (172.0, 1.2, 96), (200.0, 3.0, 48)):
        base = len(corona.verts)
        for k in range(n):
            a = 2.0 * math.pi * k / n
            corona.verts += [ring_point(radius - width, a, 0.0), ring_point(radius + width, a, 0.0)]
        for k in range(n):
            k1 = (k + 1) % n
            corona.faces.append([base + 2 * k, base + 2 * k1, base + 2 * k1 + 1, base + 2 * k + 1])
    corona.write("corona.obj", [
        "The corona of outside_megastructure: three thin flat rings round the core, 150, 172 and 200 m",
        "from it, cut into quads, whose outlines the wireframe draws as dials.",
    ])


def main():
    near()
    far()


if __name__ == "__main__":
    main()
