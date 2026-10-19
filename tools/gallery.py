"""Makes the README's pictures: for each showcase project, a headless run that lets the show
play, saves the output node's picture and a screenshot of the whole window, and writes both as
JPG (1280 pixels wide) into assets/gallery/.

Run it with no arguments for every project, or name some: `python tools/gallery.py curl-noise`.
It needs Python 3.12 or later with Pillow (`pip install pillow`) and a release build of the app
(`cargo build --release -p penumbra`, made on demand if missing).

Projects are copied to a temporary folder first, so the examples stay as they are. Two runs
look alike but not pixel for pixel (the frame reached after the wait varies by a few).
"""

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "gallery"
EXE = ROOT / "target" / "release" / "penumbra.exe"
WIDTH = 1280
JPG_QUALITY = 90

# name, project folder (None: the demo the app opens without arguments, or the network the
# setup below builds), node whose picture is saved, seconds of show time before the shots,
# whether the window is kept too (only worth it where the network fits the screen readably)
SHOWCASE = [
    ("demo", None, "/show", 45, False),
    ("curl-noise", ROOT / "examples" / "curl-noise", "/out", 45, True),
    ("projection-room", ROOT / "examples" / "projection-room", "/view", 10, False),
    ("audio-meters", None, "/meters", 9, False),
]

# A Python script node playing a 124 BPM pattern (kick on every beat, louder on the bar's first,
# a snare on two and four, hats between, a bass note per bar), so the audio meters have a
# steady beat to lock on without a microphone.
AUDIO_SYNTH = r'''
import math, random
BPM = 124.0
RATE = 48000
last = None
def cook(cx):
    global last
    t = cx.time
    if t is None:
        return
    if last is None or t < last or t - last > 0.5:
        last = t - 1.0 / 60.0
    n = int((t - last) * RATE)
    start = last
    last = start + n / RATE
    out = []
    beat_len = 60.0 / BPM
    chords = [110.0, 123.47, 98.0, 87.31]
    for k in range(n):
        s = start + k / RATE
        b = s / beat_len
        beat = int(b)
        ph = (b - beat) * beat_len
        bar = beat // 4
        in_bar = beat % 4
        v = 0.0
        # kick on every beat, louder on the bar's first
        amp = 0.9 if in_bar == 0 else 0.6
        v += amp * math.exp(-ph * 18.0) * math.sin(2 * math.pi * (50.0 + 90.0 * math.exp(-ph * 30.0)) * ph)
        # snare-ish noise on 2 and 4
        if in_bar in (1, 3):
            v += 0.35 * math.exp(-ph * 25.0) * (random.random() * 2 - 1)
        # hat on the off-beat
        ph2 = ph - beat_len / 2
        if ph2 >= 0:
            v += 0.12 * math.exp(-ph2 * 60.0) * (random.random() * 2 - 1)
        # a bass tone that changes with each bar
        f = chords[bar % 4]
        v += 0.15 * math.sin(2 * math.pi * f * s)
        out.append(v)
    if not out:
        out = [0.0]
    cx.out_rate = RATE
    cx.out["ch1"] = out
'''


def setup(name: str, work: Path) -> list[dict]:
    """Commands that build a showcase's network before the shots, for those without a project."""
    if name != "audio-meters":
        return []

    def call(method: str, **params) -> dict:
        return {"method": method, "params": params}

    return [
        call("project.new", dir=(work / name).as_posix()),
        call("node.create", path="/synth", kind="script.python", params={"code": AUDIO_SYNTH, "cook": "every frame"}),
        call("node.create", path="/analysis", kind="chan.audio_analysis"),
        call("node.connect", **{"from": "/synth", "to": "/analysis"}),
        call("node.create", path="/meters", kind="tex.audio_meters"),
        call("node.connect", **{"from": "/analysis", "output": "out", "to": "/meters", "input": "in"}),
        call("node.connect", **{"from": "/analysis", "output": "spectrum", "to": "/meters", "input": "spectrum"}),
    ]


def script_lines(output: str, wait: float, picture: Path, window: Path, before: list[dict]) -> str:
    steps = before + [
        {"method": "ui.view", "params": {"path": output}},
        {"wait": wait},
        {"method": "node.snapshot", "params": {"path": output, "file": picture.as_posix()}},
        {"method": "ui.screenshot", "params": {"file": window.as_posix()}},
    ]
    return "\n".join(json.dumps(s) for s in steps) + "\n"


def to_jpg(png: Path, jpg: Path) -> None:
    with Image.open(png) as image:
        image = image.convert("RGB")
        height = round(image.height * WIDTH / image.width)
        image.resize((WIDTH, height), Image.Resampling.LANCZOS).save(jpg, quality=JPG_QUALITY)


def shoot(name: str, project: Path | None, output: str, wait: float, network: bool, work: Path) -> None:
    picture, window = work / f"{name}.png", work / f"{name}-window.png"
    script = work / f"{name}.jsonl"
    script.write_text(script_lines(output, wait, picture, window, setup(name, work)), encoding="utf-8")
    command = [str(EXE), "--headless", str(script)]
    if project is not None:
        copy = work / name
        shutil.copytree(project, copy)
        shutil.rmtree(copy / "state", ignore_errors=True)
        command += ["--project", str(copy), "--fresh"]
    log = subprocess.run(command, capture_output=True, text=True, check=False)
    for line in log.stdout.splitlines():
        if '"ok":false' in line.replace(" ", ""):
            print(f"  {line}")
    if not picture.exists() or not window.exists():
        raise RuntimeError(f"{name}: no pictures made\n{log.stdout}\n{log.stderr}")
    OUT.mkdir(parents=True, exist_ok=True)
    to_jpg(picture, OUT / f"{name}.jpg")
    made = [f"{name}.jpg"]
    if network:
        to_jpg(window, OUT / f"{name}-network.jpg")
        made.append(f"{name}-network.jpg")
    print(f"{name}: assets/gallery/{', '.join(made)}")


def main() -> int:
    wanted = set(sys.argv[1:])
    unknown = wanted - {entry[0] for entry in SHOWCASE}
    if unknown:
        print(f"unknown project(s): {', '.join(sorted(unknown))}")
        return 2
    if not EXE.exists():
        subprocess.run(["cargo", "build", "--release", "-p", "penumbra"], cwd=ROOT, check=True)
    with tempfile.TemporaryDirectory(prefix="penumbra-gallery-") as tmp:
        for name, project, output, wait, network in SHOWCASE:
            if wanted and name not in wanted:
                continue
            shoot(name, project, output, wait, network, Path(tmp))
    return 0


if __name__ == "__main__":
    sys.exit(main())
