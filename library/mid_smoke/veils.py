"""Writes veil1.obj to veil3.obj for mid_smoke: an upright quad one unit wide (x -0.5 to 0.5) and
one unit high (z 0 to 1), facing -y, whose texture coordinates take one third of smoke.wgsl's
picture: veil1 the top third, veil3 the bottom one. Run with no arguments."""
from pathlib import Path

HERE = Path(__file__).resolve().parent
MARGIN = 0.004


def write(k):
    top = 1.0 - ((k - 1) / 3 + MARGIN)
    foot = 1.0 - (k / 3 - MARGIN)
    lines = [
        f"# A veil of mid_smoke: an upright quad, 1 wide and 1 high, its foot on z = 0, facing -y.",
        f"# Its texture coordinates take third {k} of the smoke picture (1 is the top one).",
        "v -0.5 0 0", "v 0.5 0 0", "v 0.5 0 1", "v -0.5 0 1",
        f"vt 0 {foot:.4f}", f"vt 1 {foot:.4f}", f"vt 1 {top:.4f}", f"vt 0 {top:.4f}",
        "f 1/1 2/2 3/3 4/4", "",
    ]
    (HERE / f"veil{k}.obj").write_bytes("\n".join(lines).encode())


if __name__ == "__main__":
    for k in (1, 2, 3):
        write(k)
    print("wrote veil1.obj to veil3.obj")
