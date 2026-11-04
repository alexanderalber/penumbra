"""Writes body.obj and wings.obj for mid_birds: a bird 1 m across the wings, +y forward, z up. The
body is a slim upright diamond with a head and a tail, so a bird seen from the side shows it at
every wingbeat. The wings rise from the body to the tips in two pieces, an inner wing and a
pointed outer one; mid_birds scales their height to beat them. Run with no arguments."""
from pathlib import Path

HERE = Path(__file__).resolve().parent


def obj(name, head, verts, faces):
    lines = [f"# {head}"] + [f"v {x:.4f} {y:.4f} {z:.4f}" for x, y, z in verts]
    lines += ["f " + " ".join(str(i + 1) for i in f) for f in faces] + [""]
    (HERE / name).write_bytes("\n".join(lines).encode())


def body():
    # Upright in the x = 0 plane, and a second one across it, so it has some width from above.
    v = [(0, 0.24, 0.015), (0, 0.06, 0.045), (0, -0.16, 0.02), (0, -0.26, 0.01), (0, -0.16, -0.01),
         (0, 0.04, -0.035),
         (0.035, 0.05, 0.005), (-0.035, 0.05, 0.005), (0.06, -0.24, 0.01), (-0.06, -0.24, 0.01)]
    f = [(0, 1, 2, 3), (0, 3, 4, 5), (0, 6, 2, 8), (0, 9, 2, 7)]
    obj("body.obj", "The body of a mid_birds bird: head at +y, tail at -y, z up, 0.5 m long.", v, f)


def wings():
    v = []
    f = []
    for s in (1, -1):
        b = len(v)
        v += [(s * 0.03, 0.07, 0.0), (s * 0.26, 0.06, 0.07), (s * 0.26, -0.08, 0.06), (s * 0.03, -0.09, 0.0),
              (s * 0.50, -0.13, 0.18)]
        f += [(b, b + 1, b + 2, b + 3), (b + 1, b + 4, b + 2)]
    obj("wings.obj", "The wings of a mid_birds bird: 1 m across along x, +y forward, the tips raised.", v, f)


if __name__ == "__main__":
    body()
    wings()
    print("wrote body.obj and wings.obj")
