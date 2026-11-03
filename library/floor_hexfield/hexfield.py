"""Writes hex.obj for floor_hexfield next to this file: a hexagonal prism one unit across its
corners (corners towards +x and -x) and one unit high, centred on the origin, z up, with a top
face and open at the bottom. Its sides are whole faces, so the wireframe draws the six upright
edges and the top and bottom rings only. Run with no arguments."""

import math
from pathlib import Path

HERE = Path(__file__).resolve().parent
NL = "\n"


def main():
    lines = [
        "# A hexagonal prism for floor_hexfield: one unit across its corners (towards +x and -x) and one",
        "# high, centred on the origin, z up, with a top face. Written by hexfield.py.",
    ]
    ring = [(0.5 * math.cos(math.pi * k / 3), 0.5 * math.sin(math.pi * k / 3)) for k in range(6)]
    for z in (-0.5, 0.5):
        for x, y in ring:
            lines.append(f"v {x:.4f} {y:.4f} {z:.4f}")
    faces = []
    for k in range(6):
        a = k + 1
        b = (k + 1) % 6 + 1
        faces.append(f"f {a} {b} {b + 6} {a + 6}")
    faces.append("f " + " ".join(str(7 + k) for k in range(6)))
    (HERE / "hex.obj").write_text(NL.join(lines + faces) + NL, encoding="utf-8", newline=NL)
    print("wrote hex.obj")


if __name__ == "__main__":
    main()
