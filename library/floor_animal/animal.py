"""Writes pose_0.obj to pose_7.obj: a low-poly horse in eight poses of one gallop stride.

The stride is a transverse gallop as Muybridge photographed it: left hind, right hind, left fore
and right fore come down one after another, then all four legs leave the ground together. Pose k
shows the stride at k / 8 of the way through. The body rocks and bobs with the legs, the neck
swings against the body, the tail streams back. A horse 1.6 m to the withers and 3 m from nose
to tail, in metres, z up, the nose towards +y, the origin on the ground under the middle of the
body. Run with no arguments; the files land next to this script. Standard library only.
"""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
POSES = 8
DEG = math.pi / 180.0

# The body's rings from the buttock to the chest: y, top, bottom, half width (metres).
BODY = [
    (-0.84, 1.36, 1.12, 0.09),
    (-0.72, 1.52, 1.00, 0.20),
    (-0.50, 1.60, 0.96, 0.27),
    (-0.18, 1.54, 0.90, 0.29),
    (0.14, 1.55, 0.86, 0.30),
    (0.42, 1.66, 0.86, 0.27),
    (0.66, 1.60, 0.94, 0.22),
    (0.82, 1.36, 1.06, 0.13),
]
# The point the body turns about when it rocks.
PIVOT = (0.0, 1.25)

# Legs: where the visible leg leaves the body (y, z), the lengths of its three bones
# (top to knee or hock, to fetlock, to the hoof's sole), which way the middle joint bends
# (+1 forwards for the knee of a foreleg, -1 backwards for the hock of a hind leg), and its
# radii at the four joints.
FORE = dict(top=(0.48, 1.0), bones=(0.46, 0.36, 0.2), bend=1.0, radii=(0.085, 0.05, 0.045, 0.06))
HIND = dict(top=(-0.46, 1.04), bones=(0.5, 0.4, 0.2), bend=-1.0, radii=(0.1, 0.055, 0.045, 0.06))
# Half the distance between the left and the right legs.
STANCE_X = 0.16

# The gallop: when each leg's hoof is on the ground, as shares of the stride.
# name, which leg, x side (-1 left), start of the stance, its length.
LEGS = [
    ("left_hind", HIND, -1.0, 0.00, 0.32),
    ("right_hind", HIND, 1.0, 0.10, 0.32),
    ("left_fore", FORE, -1.0, 0.32, 0.30),
    ("right_fore", FORE, 1.0, 0.42, 0.30),
]


def smooth(t):
    t = min(max(t, 0.0), 1.0)
    return t * t * (3.0 - 2.0 * t)


class Mesh:
    def __init__(self):
        self.verts = []
        self.faces = []

    def add(self, verts, faces):
        base = len(self.verts) + 1
        self.verts += verts
        self.faces += [tuple(base + i for i in f) for f in faces]

    def loft(self, rings, cap_start=True, cap_end=True):
        """Rings of equal point count joined by quads, the ends closed by one polygon."""
        n = len(rings[0])
        verts = [p for r in rings for p in r]
        faces = []
        for i in range(len(rings) - 1):
            for k in range(n):
                j = (k + 1) % n
                faces.append((i * n + k, i * n + j, (i + 1) * n + j, (i + 1) * n + k))
        if cap_start:
            faces.append(tuple(reversed(range(n))))
        if cap_end:
            last = (len(rings) - 1) * n
            faces.append(tuple(last + k for k in range(n)))
        self.add(verts, faces)

    def write(self, path, header):
        lines = [f"# {header}"]
        lines += [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in self.verts]
        lines += ["f " + " ".join(str(i) for i in f) for f in self.faces]
        path.write_bytes(("\n".join(lines) + "\n").encode("ascii"))


def turn_yz(p, pivot, a):
    """Turns (y, z) about the pivot by `a` radians, nose up for positive `a`."""
    c, s = math.cos(a), math.sin(a)
    y, z = p[0] - pivot[0], p[1] - pivot[1]
    return (pivot[0] + c * y - s * z, pivot[1] + s * y + c * z)


def body_ring(y, top, bottom, hw):
    """Eight points round the barrel at y: flat at the back, round at the belly."""
    mid = 0.5 * (top + bottom)
    half = 0.5 * (top - bottom)
    pts = []
    for k in range(8):
        a = 2.0 * math.pi * k / 8.0
        sx, sz = math.sin(a), math.cos(a)
        # A squarer outline (a superellipse) at the shoulders than a plain ellipse.
        ex = math.copysign(abs(sx) ** 0.75, sx)
        ez = math.copysign(abs(sz) ** 0.8, sz)
        pts.append((hw * ex, y, mid + half * ez))
    return pts


def tube_ring(centre, direction, r_side, r_flat, sides):
    """`sides` points round a tube in the y-z plane: x across, the other axis in the plane."""
    dy, dz = direction
    length = math.hypot(dy, dz) or 1.0
    ny, nz = -dz / length, dy / length
    pts = []
    for k in range(sides):
        a = 2.0 * math.pi * (k + 0.5) / sides
        u, w = r_side * math.cos(a), r_flat * math.sin(a)
        pts.append((centre[0] + u, centre[1] + w * ny, centre[2] + w * nz))
    return pts


def tube(mesh, points, radii, sides=4, flat=1.0, cap_start=True):
    """A tube along points (x, y, z) in a plane of constant x, with a radius per point."""
    rings = []
    for i, p in enumerate(points):
        a = points[max(i - 1, 0)]
        b = points[min(i + 1, len(points) - 1)]
        d = (b[1] - a[1], b[2] - a[2])
        r = radii[i]
        rings.append(tube_ring(p, d, r, r * flat, sides))
    mesh.loft(rings, cap_start=cap_start)


def two_bone(top, target, a, b, bend):
    """The middle joint of a two-bone chain from top towards target (y, z)."""
    dy, dz = target[0] - top[0], target[1] - top[1]
    d = math.hypot(dy, dz)
    d = min(max(d, abs(a - b) + 1e-4), a + b - 1e-4)
    # The angle at the top between the line to the target and the first bone.
    cos_t = (a * a + d * d - b * b) / (2.0 * a * d)
    t = math.acos(min(max(cos_t, -1.0), 1.0))
    base = math.atan2(dz, dy)
    # The joint bends to +y (bend 1) or -y (bend -1); the line points down, so turning it
    # clockwise in the y-z plane moves the joint forwards.
    ang = base + t * bend
    return (top[0] + a * math.cos(ang), top[1] + a * math.sin(ang))


def leg_points(leg, phase, start, share, body_turn, bob):
    """The four joints (y, z) of one leg at `phase` of the stride."""
    top = turn_yz(leg["top"], PIVOT, body_turn)
    top = (top[0], top[1] + bob)
    l1, l2, l3 = leg["bones"]
    full = l1 + l2 + l3
    fore = leg["bend"] > 0
    reach, back = (32.0 * DEG, 26.0 * DEG) if fore else (24.0 * DEG, 34.0 * DEG)
    t = (phase - start) % 1.0
    if t < share:
        # On the ground: the hoof sweeps from ahead of the top joint to behind it.
        s = t / share
        ang = reach + (-back - reach) * s
        hoof = (top[0] + top[1] * math.tan(ang), 0.0)
        lift = 0.0
    else:
        # In the air: back to the front, folded most half way.
        s = (t - share) / (1.0 - share)
        ang = -back + (reach + back) * smooth(s)
        fold = math.sin(math.pi * min(s * 1.15, 1.0)) * (0.42 if fore else 0.34)
        length = full * (1.0 - fold)
        hoof = (top[0] + length * math.sin(ang), top[1] - length * math.cos(ang))
        lift = math.sin(math.pi * s)
        if hoof[1] < 0.01:
            hoof = (hoof[0], 0.01)
    # Fetlock: above the hoof; in the air the pastern flips back behind the cannon.
    flick = (0.9 if fore else 0.6) * lift
    pastern = (-math.sin(flick) * l3, math.cos(flick) * l3)
    if t < share:
        # A loaded fetlock sinks a little and leans the pastern forwards.
        pastern = (-0.35 * l3, 0.94 * l3)
    fetlock = (hoof[0] + pastern[0], hoof[1] + pastern[1])
    knee = two_bone(top, fetlock, l1, l2, leg["bend"])
    return [top, knee, fetlock, hoof]


def neck_and_head(mesh, body_turn, bob, phase):
    """Neck from the shoulders up to the poll, then the head hanging forwards, ears, mane."""
    # The neck swings down as the forelegs land and up in the air, against the body's rock.
    swing = 9.0 * DEG * math.cos(2.0 * math.pi * (phase - 0.5))
    base = turn_yz((0.6, 1.36), PIVOT, body_turn)
    base = (base[0], base[1] + bob)
    up = 44.0 * DEG - swing + body_turn * 0.4
    length = 0.78
    poll = (base[0] + length * math.cos(up), base[1] + length * math.sin(up))
    rings = []
    count = 5
    for i in range(count):
        f = i / (count - 1)
        # The neck curves a little: its line bows upwards in the middle.
        y = base[0] + (poll[0] - base[0]) * f
        z = base[1] + (poll[1] - base[1]) * f + 0.06 * math.sin(math.pi * f)
        depth = 0.40 + (0.15 - 0.40) * f
        hw = 0.17 + (0.09 - 0.17) * f
        d = (math.cos(up), math.sin(up))
        rings.append(tube_ring((0.0, y, z), d, hw, depth, 6))
    mesh.loft(rings, cap_start=False, cap_end=False)
    # The head: from the poll down to the muzzle, deep at the jaw.
    down = -50.0 * DEG - 0.6 * swing + body_turn * 0.4
    d = (math.cos(down), math.sin(down))
    stations = [(0.0, 0.12, 0.15), (0.2, 0.115, 0.17), (0.45, 0.09, 0.12), (0.66, 0.08, 0.09)]
    rings = []
    # The head starts a little behind the poll and below it, where the throat meets the jaw.
    start = (poll[0] - 0.05, poll[1] - 0.02)
    for s, hw, depth in stations:
        y = start[0] + s * d[0]
        z = start[1] + s * d[1]
        # The jaw sits below the head's line: move the ring down at the cheek.
        drop = 0.06 * math.sin(math.pi * min(s / 0.35, 1.0))
        ny, nz = -d[1], d[0]
        rings.append(tube_ring((0.0, y - ny * drop, z - nz * drop), d, hw, depth, 6))
    mesh.loft(rings)
    # Ears: two thin spikes on the poll, pricked forwards a little.
    for side in (-1.0, 1.0):
        root = (side * 0.06, poll[0] - 0.02, poll[1] + 0.08)
        tip = (side * 0.08, poll[0] + 0.02, poll[1] + 0.26)
        w = 0.035
        verts = [(root[0] - w, root[1] - w, root[2]), (root[0] + w, root[1] - w, root[2]),
                 (root[0], root[1] + w, root[2]), tip]
        mesh.add(verts, [(0, 1, 3), (1, 2, 3), (2, 0, 3)])
    # Mane: a saw edge standing up along the top of the neck, flicking with the swing.
    pts = []
    n = 9
    for i in range(n):
        f = 0.08 + 0.92 * i / (n - 1)
        y = base[0] + (poll[0] - base[0]) * f
        z = base[1] + (poll[1] - base[1]) * f + 0.06 * math.sin(math.pi * f)
        depth = 0.40 + (0.15 - 0.40) * f
        ny, nz = -math.sin(up), math.cos(up)
        top = (y - ny * depth, z + nz * depth)
        hair = 0.09 + 0.05 * (i % 2) - 0.03 * f
        back = 0.06 + 0.04 * math.sin(2.0 * math.pi * (phase + f))
        pts.append((top[0], top[1]))
        pts.append((top[0] - back - ny * hair, top[1] + nz * hair))
    verts = [(0.0, y, z) for y, z in pts]
    faces = [(2 * i, 2 * i + 1, 2 * i + 2) for i in range(n - 1)]
    mesh.add(verts, faces)


def tail(mesh, body_turn, bob, phase):
    root = turn_yz((-0.8, 1.42), PIVOT, body_turn)
    root = (root[0], root[1] + bob)
    pts = [(0.0,) + root]
    # Streaming back, lifted in the air, waving once per stride.
    angle = -26.0 * DEG + 10.0 * DEG * math.sin(2.0 * math.pi * (phase + 0.2))
    seg = 0.2
    y, z = root
    for i in range(5):
        a = angle - i * (11.0 * DEG) + 6.0 * DEG * math.sin(2.0 * math.pi * (phase - 0.15 * i))
        y -= seg * math.cos(a)
        z += seg * math.sin(a)
        pts.append((0.0, y, z))
    radii = [0.05, 0.07, 0.085, 0.085, 0.065, 0.02]
    tube(mesh, pts, radii, sides=4, flat=0.45)


def pose(phase):
    mesh = Mesh()
    # The body rocks: nose up as the hind legs land, down as the forelegs do; and it is
    # highest while all four legs are in the air.
    body_turn = 4.0 * DEG * math.cos(2.0 * math.pi * (phase - 0.12))
    bob = 0.05 * math.cos(2.0 * math.pi * (phase - 0.85))
    rings = []
    for y, top, bottom, hw in BODY:
        ring = body_ring(y, top, bottom, hw)
        moved = []
        for x, py, pz in ring:
            ty, tz = turn_yz((py, pz), PIVOT, body_turn)
            moved.append((x, ty, tz + bob))
        rings.append(moved)
    mesh.loft(rings)
    neck_and_head(mesh, body_turn, bob, phase)
    tail(mesh, body_turn, bob, phase)
    for _, leg, side, start, share in LEGS:
        joints = leg_points(leg, phase, start, share, body_turn, bob)
        # The top joint sits inside the body, so the leg grows out of the barrel.
        top = joints[0]
        inside = (top[0], top[1] + 0.12)
        pts = [(side * STANCE_X, y, z) for y, z in [inside] + joints]
        r = leg["radii"]
        tube(mesh, pts, [r[0], r[0], r[1], r[2], r[3]], sides=4)
    return mesh


def main():
    for k in range(POSES):
        phase = k / POSES
        mesh = pose(phase)
        out = HERE / f"pose_{k}.obj"
        mesh.write(out, f"floor_animal: a galloping horse, pose {k} of {POSES} (written by animal.py)")
        print(f"{out.name}: {len(mesh.verts)} points, {len(mesh.faces)} faces")


if __name__ == "__main__":
    main()
