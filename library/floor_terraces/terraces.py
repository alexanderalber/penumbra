"""Writes floor_terraces' models next to this file. Run it with no arguments.

terrace_fill.obj and terrace_edges.obj: a platform of three tiers in a box one unit on each side,
z up. x runs along the shore, +y faces the lane, the tiers step up away from it: the lowest
spans the whole depth, each next one a third less, each a third of the height.

pyramid_fill.obj and pyramid_edges.obj: a stepped pyramid of five tiers with a small temple on
top, one unit across and one high, and a stair up its +y face from the water to the top tier.

Both stand on z = 0, their base a little below it (BASE), so the water line cuts them. The fill
models are closed boxes, drawn black. The edge models are the same boxes a hair larger: their
outlines are the rim lines. Each edge corner carries a light value in its u (1 at the top of its
tier, 0.1 at its foot), for a ramp that makes the top edges bright and the upright ones fade
downwards.
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent

# Where the base lies below the water line, as a share of the height.
BASE = -0.05
# How far the edges lie outside the fill, in model units.
GROW = 0.006
# The light value at a box's top corners and at its foot.
TOP_LIGHT = 1.0
FOOT_LIGHT = 0.1


class Obj:
    def __init__(self, title):
        self.lines = [f"# {title}"]
        self.vs = 0

    def box(self, x0, x1, y0, y1, z0, z1, grow=0.0, light=False):
        x0, y0, z0 = x0 - grow, y0 - grow, z0 - grow
        x1, y1, z1 = x1 + grow, y1 + grow, z1 + grow
        corners = [(x, y, z) for z in (z0, z1) for y in (y0, y1) for x in (x0, x1)]
        base = self.vs
        for i, (x, y, z) in enumerate(corners):
            self.lines.append(f"v {x:.4f} {y:.4f} {z:.4f}")
            if light:
                # Each box's top edges catch the light, its foot stays dim.
                self.lines.append(f"vt {TOP_LIGHT if i >= 4 else FOOT_LIGHT:.4f} 0.5")
        self.vs += 8
        faces = [(0, 2, 3, 1), (4, 5, 7, 6), (0, 1, 5, 4), (2, 6, 7, 3), (0, 4, 6, 2), (1, 3, 7, 5)]
        for f in faces:
            self.face([base + i for i in f], light)

    def face(self, idx, light):
        if light:
            self.lines.append("f " + " ".join(f"{i + 1}/{i + 1}" for i in idx))
        else:
            self.lines.append("f " + " ".join(str(i + 1) for i in idx))

    def write(self, name):
        (HERE / name).write_text("\n".join(self.lines) + "\n", encoding="ascii", newline="\n")


def lit(z, top):
    """The stair's light: dim at the water, bright at the top tier."""
    return FOOT_LIGHT + (TOP_LIGHT - FOOT_LIGHT) * min(max(z / top, 0.0), 1.0)


def terrace(edges):
    o = Obj("floor_terraces: a platform of three tiers, " + ("its rim edges" if edges else "its black fill"))
    g = GROW if edges else 0.0
    for k in range(3):
        z0 = BASE if k == 0 else k / 3
        o.box(-0.5, 0.5, -0.5, 0.5 - k / 3, z0, (k + 1) / 3, g, edges)
    return o


# The pyramid: half widths of the tiers, the tier height, the temple's half sizes and height.
TIERS = [0.5, 0.42, 0.34, 0.26, 0.18]
TIER = 0.15
TEMPLE = (0.11, 0.08, 0.22)
# The stair: half its width, how far its foot reaches out beyond the lowest tier, steps.
STAIR = 0.07
STAIR_OUT = 0.08
STEPS = 10


def pyramid(edges):
    o = Obj("floor_terraces: a stepped pyramid with a stair on +y, " + ("its rim edges" if edges else "its black fill"))
    g = GROW if edges else 0.0
    for k, s in enumerate(TIERS):
        z0 = BASE if k == 0 else k * TIER
        o.box(-s, s, -s, s, z0, (k + 1) * TIER, g, edges)
    top = len(TIERS) * TIER
    tx, ty, th = TEMPLE
    o.box(-tx, tx, -ty, ty, top, top + th, g, edges)
    # The stair: a wedge from the water in front of the lowest tier up to the top tier's edge.
    # Its sloped face is cut into steps, so the rim draws a line across each.
    foot_y = TIERS[0] + STAIR_OUT + g
    head_y = TIERS[-1]
    w = STAIR + g
    base = o.vs
    pts = []
    for i in range(STEPS + 1):
        t = i / STEPS
        y = foot_y + (head_y - foot_y) * t
        z = BASE + (top - BASE) * t + (g if edges else 0.0)
        pts += [(-w, y, z), (w, y, z)]
    # The back of the wedge, inside the pyramid.
    back = head_y - 0.05
    pts += [(-w, back, BASE), (w, back, BASE), (-w, back, top), (w, back, top)]
    for (x, y, z) in pts:
        o.lines.append(f"v {x:.4f} {y:.4f} {z:.4f}")
        if edges:
            o.lines.append(f"vt {lit(z, top):.4f} 0.5")
    o.vs += len(pts)
    for i in range(STEPS):
        a = base + 2 * i
        o.face([a, a + 1, a + 3, a + 2], edges)
    bl, br, tl, tr = (base + 2 * (STEPS + 1) + k for k in range(4))
    last = base + 2 * STEPS
    # The two sides, each a quad from the foot up the slope to the head and down the back.
    o.face([base, last, tl, bl], edges)
    o.face([base + 1, br, tr, last + 1], edges)
    o.face([base, bl, br, base + 1], edges)
    o.face([bl, tl, tr, br], edges)
    o.face([last, last + 1, tr, tl], edges)
    return o


if __name__ == "__main__":
    terrace(False).write("terrace_fill.obj")
    terrace(True).write("terrace_edges.obj")
    pyramid(False).write("pyramid_fill.obj")
    pyramid(True).write("pyramid_edges.obj")
    print("wrote terrace_fill.obj, terrace_edges.obj, pyramid_fill.obj, pyramid_edges.obj")
