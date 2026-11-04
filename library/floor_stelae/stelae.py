"""Writes floor_stelae's models next to this file. Run it with no arguments.

stela_1.obj, stela_2.obj, stela_3.obj: an upright stone one unit wide, deep and high, z up,
its front facing +y, its top narrowing above a shoulder like a low roof. Its base lies a little below z = 0
(BASE), so the water line cuts it. The three differ only in their front's texture coordinates:
the front of stela_k.obj below the shoulder shows cell k of the glyph picture (glyphs.wgsl, four cells side by
side), every other face cell 0, which is black.

stela_edges.obj: the same stone a hair larger, whose outlines the rim lines draw. Each corner
carries a light value in its u: brighter at the top and at the back, the side towards the
horizon's band.
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent

BASE = -0.05
# The rings of the stone from its foot up: height, half its width, half its depth. Above the
# shoulder the top narrows like a roof.
RINGS = [(BASE, 0.5, 0.5), (0.9, 0.47, 0.5), (1.0, 0.32, 0.36)]
GROW = 0.012
CELLS = 4


def corners(grow):
    """Four corners per ring: (x0 y0) (x1 y0) (x0 y1) (x1 y1), y1 the front."""
    out = []
    for k, (z, hx, hy) in enumerate(RINGS):
        z = z - grow if k == 0 else z + grow
        for y in (-hy - grow, hy + grow):
            for x in (-hx - grow, hx + grow):
                out.append((x, y, z))
    return out


def faces():
    """The front below the shoulder first, then every other face."""
    front = (2, 3, 7, 6)
    others = [(0, 2, 3, 1), (8, 9, 11, 10), (6, 7, 11, 10)]
    for b in (0, 4):
        others += [(b + 0, b + 1, b + 5, b + 4), (b + 0, b + 4, b + 6, b + 2), (b + 1, b + 3, b + 7, b + 5)]
    return front, others


def stela(cell):
    lines = [f"# floor_stelae: a stela, its front showing cell {cell} of the glyph picture"]
    for (x, y, z) in corners(0.0):
        lines.append(f"v {x:.4f} {y:.4f} {z:.4f}")
    # Texture corners: the front's four, seen from the front (foot left at x1, foot right at
    # x0, shoulder right, shoulder left), then one in the black cell for every other face.
    u0 = cell / CELLS + 0.004
    u1 = (cell + 1) / CELLS - 0.004
    lines += [f"vt {u1:.4f} 0.0", f"vt {u0:.4f} 0.0", f"vt {u0:.4f} 1.0", f"vt {u1:.4f} 1.0",
              f"vt {0.5 / CELLS:.4f} 0.5"]
    front, others = faces()
    lines.append("f " + " ".join(f"{v + 1}/{t + 1}" for v, t in zip(front, range(4))))
    for f in others:
        lines.append("f " + " ".join(f"{v + 1}/5" for v in f))
    (HERE / f"stela_{cell}.obj").write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")


def lit(y, z):
    back = 1.0 if y < 0.0 else 0.0
    return min(max(0.2 + 0.5 * max(z, 0.0) + 0.3 * back, 0.0), 1.0)


def edges():
    lines = ["# floor_stelae: the stela a hair larger, its rim edges; u is the light along them"]
    for (x, y, z) in corners(GROW):
        lines.append(f"v {x:.4f} {y:.4f} {z:.4f}")
        lines.append(f"vt {lit(y, z):.4f} 0.5")
    front, others = faces()
    for f in [front] + others:
        lines.append("f " + " ".join(f"{v + 1}/{v + 1}" for v in f))
    (HERE / "stela_edges.obj").write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")


if __name__ == "__main__":
    for k in (1, 2, 3):
        stela(k)
    edges()
    print("wrote stela_1.obj, stela_2.obj, stela_3.obj, stela_edges.obj")
