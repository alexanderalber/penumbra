"""Writes floor_waves' two models next to this file: segment.obj (a thin prism one unit long,
drawn as one line) and curtain.obj (a sheet hanging under it, drawn black). Run with no
arguments."""

from pathlib import Path

HERE = Path(__file__).resolve().parent
# Half the prism's width across, in metres.
R = 0.006


def main():
    corners = [(0.0, R), (-0.866 * R, -0.5 * R), (0.866 * R, -0.5 * R)]
    seg = [
        "# A segment of a wave line: a thin triangular prism one unit long along y, centred on the",
        "# origin, z up, 1.2 cm across. floor_waves scales it to a segment's length, so the wireframe",
        "# draws its three long edges as one line. Written by waves.py.",
    ]
    for y in (-0.5, 0.5):
        seg += [f"v {x:.4f} {y} {z:.4f}" for x, z in corners]
    seg += ["f 1 2 5 4", "f 2 3 6 5", "f 3 1 4 6"]
    (HERE / "segment.obj").write_text("\n".join(seg) + "\n", encoding="utf-8", newline="\n")
    cur = [
        "# The sheet under a segment of a wave line: upright in the segment's plane, from 1.5 cm under",
        "# the line down to 3 m, a little longer than the segment so that neighbours overlap.",
        "# floor_waves draws it black, so a wave hides the lines, the floor and the sky behind it.",
        "# Written by waves.py.",
        "v 0 -0.55 -0.015",
        "v 0 0.55 -0.015",
        "v 0 0.55 -3",
        "v 0 -0.55 -3",
        "f 1 2 3 4",
    ]
    (HERE / "curtain.obj").write_text("\n".join(cur) + "\n", encoding="utf-8", newline="\n")
    print("wrote segment.obj and curtain.obj")


if __name__ == "__main__":
    main()
