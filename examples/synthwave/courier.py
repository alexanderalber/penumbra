"""Writes courier.obj and fan.obj: the courier glider the player flies in Hikyaku on alber.me.

A flat wedge with a wraparound cabin, four ducted lift fans on stub pylons at the corners, two
main nozzles and a spoiler between canted fins. courier.obj is everything that stays still,
fan.obj the four blades of one fan around its own axis, so the network can spin them. Both in
metres at the original 15 m length, z up, the nose towards +y, centred near the middle of the
craft. Run with no arguments; the files land next to this script. Standard library only.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
CENTRE = (0.0, 0.37, 0.0)  # middle of the craft in the original frame, moved to the origin
PODS = [(3.15, -4.5), (3.15, 4.4)]  # fan centres (x, z) on the starboard side


def rot_x(a):
    c, s = math.cos(a), math.sin(a)
    return lambda p: (p[0], c * p[1] - s * p[2], s * p[1] + c * p[2])


def rot_y(a):
    c, s = math.cos(a), math.sin(a)
    return lambda p: (c * p[0] + s * p[2], p[1], -s * p[0] + c * p[2])


def rot_z(a):
    c, s = math.cos(a), math.sin(a)
    return lambda p: (c * p[0] - s * p[1], s * p[0] + c * p[1], p[2])


def euler(r):
    """A three.js Euler XYZ as turns in the order they apply: z, then y, then x."""
    return [rot_z(r[2]), rot_y(r[1]), rot_x(r[0])]


def box_verts(size):
    x, y, z = (s / 2 for s in size)
    verts = [(sx * x, sy * y, sz * z) for sx in (-1, 1) for sy in (-1, 1) for sz in (-1, 1)]
    faces = [(0, 1, 3, 2), (4, 6, 7, 5), (0, 4, 5, 1), (2, 3, 7, 6), (0, 2, 6, 4), (1, 5, 7, 3)]
    return verts, faces


def sec(z, hw_t, y_t, hw, y_m, hw_b, y_b):
    """Six-point cross section at z, as the original draws its hull."""
    return [(hw_t, y_t, z), (hw, y_m, z), (hw_b, y_b, z), (-hw_b, y_b, z), (-hw, y_m, z), (-hw_t, y_t, z)]


class Mesh:
    """Polygons in the original's frame (y up, nose towards -z)."""

    def __init__(self, centre=(0.0, 0.0, 0.0)):
        self.parts = []
        self.centre = centre

    def part(self, name, verts, faces, pos=(0, 0, 0), turns=()):
        """turns: functions applied in order, before pos."""
        out = []
        for v in verts:
            for t in turns:
                v = t(v)
            out.append((v[0] + pos[0], v[1] + pos[1], v[2] + pos[2]))
        self.parts.append((name, out, faces))

    def box(self, name, size, pos=(0, 0, 0), rot=(0, 0, 0)):
        self.part(name, *box_verts(size), pos, euler(rot))

    def loft(self, name, rings):
        """Rings of six points joined by quads, both ends closed by one polygon."""
        verts = [p for r in rings for p in r]
        n = len(rings[0])
        faces = []
        for i in range(len(rings) - 1):
            for k in range(n):
                j = (k + 1) % n
                faces.append((i * n + k, i * n + j, (i + 1) * n + j, (i + 1) * n + k))
        faces.append(tuple(reversed(range(n))))
        last = (len(rings) - 1) * n
        faces.append(tuple(last + k for k in range(n)))
        self.part(name, verts, faces)

    def lathe(self, name, profile, seg, pos=(0, 0, 0), turns=(), caps=False):
        """profile: (radius, height) points, turned about the y axis."""
        verts = []
        for i in range(seg):
            a = 2 * math.pi * i / seg
            for r, h in profile:
                verts.append((r * math.sin(a), h, r * math.cos(a)))
        n = len(profile)
        faces = []
        for i in range(seg):
            j = (i + 1) % seg
            for k in range(n - 1):
                faces.append((i * n + k, j * n + k, j * n + k + 1, i * n + k + 1))
        if caps:
            faces.append(tuple(i * n for i in range(seg)))
            faces.append(tuple(i * n + n - 1 for i in reversed(range(seg))))
        self.part(name, verts, faces, pos, turns)

    def fin(self, name, profile, thickness, pos, rot):
        """A plate from a profile in (z, y), thickness along x."""
        t = thickness / 2
        verts = [(x, y, z) for x in (t, -t) for z, y in profile]
        n = len(profile)
        faces = [tuple(range(n)), tuple(reversed(range(n, 2 * n)))]
        faces += [(k, n + k, n + (k + 1) % n, (k + 1) % n) for k in range(n)]
        self.part(name, verts, faces, pos, euler(rot))

    def write(self, path, header):
        lines = [f"# {line}" for line in header] + [""]
        base = 1
        cx, cy, cz = self.centre
        for name, verts, faces in self.parts:
            lines.append(f"o {name}")
            # Into penumbra's frame: z up, the nose (-z of the original) towards +y.
            lines += [f"v {x - cx:.4f} {-(z - cz):.4f} {y - cy:.4f}" for x, y, z in verts]
            lines += ["f " + " ".join(str(base + i) for i in f) for f in faces]
            base += len(verts)
        path.write_text("\n".join(lines) + "\n", encoding="ascii")


def duct(ri, ro, h):
    t = min(0.18, (ro - ri) * 0.4)
    return [(ri, -h / 2), (ro - t, -h / 2), (ro, -h / 2 + t), (ro, h / 2 - t), (ro - t, h / 2),
            (ri + t, h / 2), (ri - t * 0.4, h / 2 - t), (ri, -h / 2 + t), (ri, -h / 2)]


def body():
    m = Mesh(CENTRE)
    m.loft("hull", [
        sec(-7.5, 0.5, -0.05, 0.9, -0.30, 0.6, -0.55),
        sec(-6.0, 1.2, 0.38, 1.85, -0.12, 1.4, -0.80),
        sec(-4.0, 1.65, 0.58, 2.15, 0.00, 1.7, -0.92),
        sec(0.0, 1.75, 0.62, 2.30, 0.05, 1.8, -0.95),
        sec(4.5, 1.75, 0.68, 2.30, 0.10, 1.8, -0.92),
        sec(6.6, 1.6, 0.74, 2.10, 0.15, 1.6, -0.72),
        sec(7.4, 1.4, 0.70, 1.80, 0.15, 1.4, -0.45),
    ])
    m.loft("cabin", [
        sec(-4.4, 0.40, 0.57, 1.10, 0.56, 1.30, 0.52),
        sec(-2.6, 0.80, 1.42, 1.35, 1.02, 1.55, 0.52),
        sec(0.4, 0.85, 1.62, 1.42, 1.08, 1.62, 0.52),
        sec(2.8, 0.72, 1.20, 1.32, 0.88, 1.56, 0.52),
        sec(3.8, 0.45, 0.64, 1.20, 0.64, 1.40, 0.56),
    ])
    m.loft("roof_blade", [
        sec(-2.4, 0.10, 1.50, 0.42, 1.46, 0.42, 1.38),
        sec(0.4, 0.22, 1.69, 0.55, 1.65, 0.55, 1.56),
        sec(2.6, 0.18, 1.30, 0.48, 1.26, 0.48, 1.18),
    ])
    m.box("belly", (2.6, 0.12, 9.5), (0, -0.95, 0.2))
    for s in (1, -1):
        m.box("headlight", (0.5, 0.07, 0.05), (s * 0.6, -0.12, -7.42), (0, s * 0.3, 0))
        for px, pz in PODS:
            tilt = 0.12 if pz > 0 else -0.06
            pos = (s * px, 0.05, pz)
            m.lathe("duct", duct(0.8, 1.05, 0.62), 12, pos, [rot_x(tilt)])
            m.lathe("hub", [(0.28, -0.2), (0.22, 0.2)], 8, pos, [rot_x(tilt)], caps=True)
            m.part("marker", *box_verts((0.05, 0.12, 0.5)), pos, [lambda p, s=s: (p[0] + s * 1.06, p[1] + 0.05, p[2]), rot_x(tilt)])
            m.box("pylon", (0.9, 0.22, 0.8), (s * (px - 1.0), 0.08, pz))
        m.box("nozzle", (0.95, 0.5, 0.7), (s * 0.95, 0.02, 7.15))
        m.fin("fin", [(5.4, 0), (7.3, 0), (7.5, 0.78), (6.7, 0.78)], 0.12, (s * 1.3, 0.68, 0), (0, 0, -s * 0.35))
    m.box("spoiler", (3.1, 0.07, 0.75), (0, 1.42, 6.95))
    return m


def fan():
    m = Mesh()
    for b in range(4):
        m.box("blade", (1.5, 0.03, 0.18), (0, 0, 0), (0.25, b * math.pi / 4, 0))
    return m


def main():
    origin = "after the courier glider of Hikyaku on alber.me; written by courier.py"
    body().write(HERE / "courier.obj", [f"The courier's hull, cabin, ducts, nozzles and fins, {origin}."])
    fan().write(HERE / "fan.obj", [f"One lift fan's blades, about its own axis, {origin}."])
    print("wrote", HERE / "courier.obj", "and", HERE / "fan.obj")
    # Where fan.obj goes, for the table of /fan_places (penumbra's frame, tilt in degrees).
    for s in (1, -1):
        for px, pz in PODS:
            tilt = 0.12 if pz > 0 else -0.06
            print(f"{s * px - CENTRE[0]:.3f},{-pz:.3f},{0.05 - CENTRE[1]:.3f},{math.degrees(tilt):.2f}")


if __name__ == "__main__":
    main()
