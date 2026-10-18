"""Makes the README's pictures, each from a headless run of the release build:

- assets/screenshot.png: the window with the curl-noise example, a node selected;
- assets/arrange.png: the projection-room example as first placed and after one arrange;
- assets/gallery/*.jpg: the output pictures of the showcase projects, 1280 pixels wide;
- assets/gallery/audio-meters.gif: four seconds of the audio analysis' meters, fed by a
  Python script node that plays a 124 BPM pattern.

Run it with no arguments for every picture, or name some:
`python tools/gallery.py window arrange curl-noise`. It needs Python 3.12 or later with Pillow
(`pip install pillow`) and a release build of the app (`cargo build --release -p penumbra`,
made on demand if missing).

Projects are copied to a temporary folder first, so the examples stay as they are. Two runs
look alike but not pixel for pixel (the frame reached after the wait varies by a few).
"""

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
OUT = ASSETS / "gallery"
EXE = ROOT / "target" / "release" / "penumbra.exe"
EXAMPLES = ROOT / "examples"
WIDTH = 1280
JPG_QUALITY = 90

# name, project folder (None: the demo the app opens without arguments), node whose picture is
# saved, seconds of show time before the shot. Pictures with transparency are laid on black.
SHOWCASE = [
    ("demo", None, "/show", 45),
    ("curl-noise", EXAMPLES / "curl-noise", "/out", 45),
    ("projection-room", EXAMPLES / "projection-room", "/view", 10),
]

# The window shot: project, window size in points, node in the viewer, node selected (its params
# fill the panel), seconds of show time.
WINDOW = (EXAMPLES / "curl-noise", "1760x960", "/out", "/vel", 45)

# The arrange picture: a project shot without its layout.ron, so its nodes sit where a project
# without a layout puts them, and the window size it is shot at.
ARRANGE = (EXAMPLES / "projection-room", "1800x1500")
ARRANGE_LABELS = ("before: as the nodes were first placed", "after: one press of L")

# The meters animation: size, frames, frames per second, seconds before the first frame (for
# the tempo to lock on).
METERS_SIZE = (640, 360)
METERS_FRAMES = 60
METERS_FPS = 15
METERS_WAIT = 9

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


def call(method: str, **params) -> dict:
    return {"method": method, "params": params}


def run(steps: list[dict], work: Path, name: str, project: Path | None = None, size: str | None = None,
        keep_layout: bool = True) -> None:
    """Runs the app headless on a script; `project` is copied into `work` first."""
    script = work / f"{name}.jsonl"
    script.write_text("\n".join(json.dumps(s) for s in steps) + "\n", encoding="utf-8")
    command = [str(EXE), "--headless", str(script)]
    if project is not None:
        copy = work / name
        shutil.copytree(project, copy)
        shutil.rmtree(copy / "state", ignore_errors=True)
        if not keep_layout:
            (copy / "layout.ron").unlink(missing_ok=True)
        command += ["--project", str(copy), "--fresh"]
    if size is not None:
        command += ["--size", size]
    log = subprocess.run(command, capture_output=True, text=True, check=False)
    failed = [line for line in log.stdout.splitlines() if '"ok":false' in line.replace(" ", "")]
    if failed or log.returncode != 0:
        raise RuntimeError(f"{name}: the run failed\n" + "\n".join(failed) + f"\n{log.stderr}")


def on_black(png: Path) -> Image.Image:
    with Image.open(png) as image:
        image = image.convert("RGBA")
        return Image.alpha_composite(Image.new("RGBA", image.size, (0, 0, 0, 255)), image).convert("RGB")


def to_jpg(png: Path, jpg: Path) -> None:
    image = on_black(png)
    height = round(image.height * WIDTH / image.width)
    image.resize((WIDTH, height), Image.Resampling.LANCZOS).save(jpg, quality=JPG_QUALITY)


def showcase(name: str, project: Path | None, output: str, wait: float, work: Path) -> None:
    picture = work / f"{name}.png"
    steps = [call("ui.view", path=output), {"wait": wait}, call("node.snapshot", path=output, file=picture.as_posix())]
    run(steps, work, name, project)
    to_jpg(picture, OUT / f"{name}.jpg")
    print(f"{name}: assets/gallery/{name}.jpg")


def window(work: Path) -> None:
    project, size, viewed, selected, wait = WINDOW
    shot = work / "window.png"
    steps = [
        call("ui.view", path=viewed),
        call("ui.select", paths=[selected]),
        {"wait": wait},
        call("ui.frame"),
        call("ui.screenshot", file=shot.as_posix()),
    ]
    run(steps, work, "window", project, size)
    Image.open(shot).convert("RGB").save(ASSETS / "screenshot.png", optimize=True)
    print("window: assets/screenshot.png")


def network_box(image: Image.Image) -> tuple[int, int, int, int]:
    """The part of the node editor that holds nodes and wires: left of the right column, above the
    status bar, in the rows and columns with more than a few pixels brighter than the grid (a
    lone grid line has only its own)."""
    editor = image.crop((0, 0, round(image.width * 0.68), image.height - 40))
    mask = editor.convert("L").point(lambda v: 1 if v > 90 else 0)
    width, height = mask.size
    pixels = mask.load()
    columns = [x for x in range(width) if sum(pixels[x, y] for y in range(0, height, 2)) > 8]
    rows = [y for y in range(height) if sum(pixels[x, y] for x in range(0, width, 2)) > 8]
    if not columns or not rows:
        return 0, 0, width, height
    pad = 16
    return max(columns[0] - pad, 0), max(rows[0] - pad, 0), min(columns[-1] + pad, width), min(rows[-1] + pad, height)


def arrange(work: Path) -> None:
    project, size = ARRANGE
    before, after = work / "arrange-before.png", work / "arrange-after.png"
    steps = [
        call("ui.frame"),
        {"wait": 3},
        call("ui.screenshot", file=before.as_posix()),
        call("ui.arrange"),
        call("ui.frame"),
        {"wait": 1},
        call("ui.screenshot", file=after.as_posix()),
    ]
    run(steps, work, "arrange", project, size, keep_layout=False)
    shots = []
    for png in (before, after):
        image = Image.open(png).convert("RGB")
        shots.append(image.crop(network_box(image)))
    try:
        font = ImageFont.truetype("segoeui.ttf", 30)
    except OSError:
        font = ImageFont.load_default(30)
    label, gap = 60, 40
    width = sum(shot.width for shot in shots) + gap * 3
    height = max(shot.height for shot in shots) + label + gap
    sheet = Image.new("RGB", (width, height), (27, 27, 27))
    draw = ImageDraw.Draw(sheet)
    x = gap
    for shot, text in zip(shots, ARRANGE_LABELS):
        draw.text((x, 12), text, fill=(200, 200, 200), font=font)
        sheet.paste(shot, (x, label))
        x += shot.width + gap
    sheet.save(ASSETS / "arrange.png", optimize=True)
    print("arrange: assets/arrange.png")


def meters(work: Path) -> None:
    frames = work / "frames"
    frames.mkdir()
    width, height = METERS_SIZE
    steps = [
        call("project.new", dir=(work / "project").as_posix()),
        call("node.create", path="/synth", kind="script.python", params={"code": AUDIO_SYNTH, "cook": "every frame"}),
        call("node.create", path="/analysis", kind="chan.audio_analysis", params={"width": width, "height": height}),
        call("node.connect", **{"from": "/synth", "to": "/analysis"}),
        call("ui.view", path="/analysis", output="meters"),
        {"wait": METERS_WAIT},
    ]
    for i in range(METERS_FRAMES):
        steps.append(call("node.snapshot", path="/analysis", output="meters", file=(frames / f"{i:03}.png").as_posix()))
        steps.append({"wait": 1 / METERS_FPS})
    run(steps, work, "meters")
    images = [on_black(frames / f"{i:03}.png").quantize(colors=64) for i in range(METERS_FRAMES)]
    images[0].save(OUT / "audio-meters.gif", save_all=True, append_images=images[1:],
                   duration=round(1000 / METERS_FPS), loop=0, optimize=True)
    print("audio-meters: assets/gallery/audio-meters.gif")


def main() -> int:
    jobs = {entry[0]: (lambda work, entry=entry: showcase(*entry, work)) for entry in SHOWCASE}
    jobs.update({"window": window, "arrange": arrange, "audio-meters": meters})
    wanted = sys.argv[1:] or list(jobs)
    unknown = [name for name in wanted if name not in jobs]
    if unknown:
        print(f"unknown picture(s): {', '.join(unknown)}; known: {', '.join(jobs)}")
        return 2
    if not EXE.exists():
        subprocess.run(["cargo", "build", "--release", "-p", "penumbra"], cwd=ROOT, check=True)
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="penumbra-gallery-") as tmp:
        for name in wanted:
            work = Path(tmp) / name
            work.mkdir()
            jobs[name](work)
    return 0


if __name__ == "__main__":
    sys.exit(main())
