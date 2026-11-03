"""Writes hexgap.obj for floor_hexgap next to this file: a hexagonal prism one unit across its
corners (corners towards +y and -y, as floor_pillars' prism) and one unit high, centred on the
origin, z up, closed at both ends. Its sides are whole faces, so the wireframe draws the six
upright edges and the two end rings only. floor_hexgap scales one per part of a pillar: the
base on the floor and the counterpart hanging from the ceiling. Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
NL = "\n"


def main():
    lines = [
        "# A hexagonal prism for floor_hexgap: one unit across its corners (towards +y and -y) and one",
        "# high, centred on the origin, z up, closed at both ends. Written by hexgap.py.",
    ]
    ring = [(0.5 * math.cos(math.pi * (k / 3 + 0.5)), 0.5 * math.sin(math.pi * (k / 3 + 0.5))) for k in range(6)]
    for z in (-0.5, 0.5):
        for x, y in ring:
            lines.append(f"v {x + 0.0:.4f} {y + 0.0:.4f} {z:.4f}")
    faces = []
    for k in range(6):
        a = k + 1
        b = (k + 1) % 6 + 1
        faces.append(f"f {a} {b} {b + 6} {a + 6}")
    faces.append("f " + " ".join(str(7 + k) for k in range(6)))
    faces.append("f " + " ".join(str(6 - k) for k in range(6)))
    (HERE / "hexgap.obj").write_text(NL.join(lines + faces) + NL, encoding="utf-8", newline=NL)
    print("wrote hexgap.obj")


if __name__ == "__main__":
    main()
