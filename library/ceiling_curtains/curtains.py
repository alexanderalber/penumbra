"""Writes ceiling_curtains' two models next to this file. segment.obj is one piece of a cloth
strip: its two side edges, one unit apart across x, hanging one unit down along -z from the
origin. hem.obj is the strip's lower edge, one unit across at z = -1. Each edge is a thin prism,
which the wireframe draws as one line. Run with no arguments."""

from pathlib import Path

HERE = Path(__file__).resolve().parent

# Half the thickness of an edge, in the model's units (a strip is about half a metre wide).
THIN = 0.004
NL = "\n"


def prism(lines, faces, a, b, across):
    """A thin triangular prism from point a to point b; `across` names the axis it is thin in
    besides y."""
    o = len([x for x in lines if x.startswith("v ")]) + 1
    for p in (a, b):
        for d1, d2 in [(THIN, 0.0), (-0.5 * THIN, THIN), (-0.5 * THIN, -THIN)]:
            if across == "x":
                lines.append(f"v {p[0] + d1:.4f} {p[1] + d2:.4f} {p[2]:.4f}")
            else:
                lines.append(f"v {p[0]:.4f} {p[1] + d2:.4f} {p[2] + d1:.4f}")
    for k in range(3):
        m = (k + 1) % 3
        faces.append(f"f {o + k} {o + m} {o + 3 + m} {o + 3 + k}")


def main():
    lines = [
        "# A segment of a curtain strip for ceiling_curtains: its two side edges at x = -0.5 and 0.5,",
        "# from the origin's height one unit down along -z, each a thin prism. Written by curtains.py.",
    ]
    faces = []
    prism(lines, faces, (-0.5, 0, 0), (-0.5, 0, -1), "x")
    prism(lines, faces, (0.5, 0, 0), (0.5, 0, -1), "x")
    (HERE / "segment.obj").write_text(NL.join(lines + faces) + NL, encoding="utf-8", newline=NL)
    lines = [
        "# The hem of a curtain strip for ceiling_curtains: a thin prism across x from -0.5 to 0.5 at",
        "# z = -1, the lower edge of the last segment. Written by curtains.py.",
    ]
    faces = []
    prism(lines, faces, (-0.5, 0, -1), (0.5, 0, -1), "z")
    (HERE / "hem.obj").write_text(NL.join(lines + faces) + NL, encoding="utf-8", newline=NL)
    print("wrote segment.obj and hem.obj")


if __name__ == "__main__":
    main()
