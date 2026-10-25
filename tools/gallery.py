"""Makes the README's pictures, each from a headless run of the release build:

- assets/screenshot.png: the window with the curl-noise example, a node selected;
- assets/arrange.png: the curl-noise example as first placed and after one arrange;
- assets/gallery/*.jpg: the output pictures of the showcase projects, 1280 pixels wide;
- assets/gallery/audio-meters.png: the audio analysis' meters after four seconds, fed by a
  Python script node that plays a 124 BPM pattern (a late frame of the run, so the kick, onset
  and beat strips are full);
- assets/gallery/relief.jpg: the projection-onto-objects example's camera photo of the relief
  beside the picture penumbra lights it with;
- assets/gallery/library.jpg: six of the content library's previews;
- assets/gallery/palettes.png: the built-in palettes and colormaps.

The photos in assets/photos/ come from a real setup, not from this script.

Run it with no arguments for every picture, or name some:
`python tools/gallery.py window arrange curl-noise`. It needs Python 3.12 or later with Pillow
(`pip install pillow`) and a release build of the app (`cargo build --release -p penumbra`,
made on demand if missing).

Projects are copied to a temporary folder first, so the examples stay as they are. Two runs
look alike but not pixel for pixel (the frame reached after the wait varies by a few).
"""

import json
import re
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
    # 70 s: the sky's palette has turned from warm to cold by then.
    ("synthwave", EXAMPLES / "synthwave", "/out", 70),
    ("fluid", EXAMPLES / "fluid", "/view", 25),
    ("feedback", EXAMPLES / "feedback", "/show", 20),
]

# The window shot: project, window size in points, node in the viewer, node selected (its params
# fill the panel), seconds of show time.
WINDOW = (EXAMPLES / "curl-noise", "1760x960", "/out", "/vel", 45)

# The arrange picture: a project shot without its node positions, so its nodes sit where the
# editor places nodes that come without one, and the window size it is shot at.
ARRANGE = (EXAMPLES / "curl-noise", "1700x900")
ARRANGE_LABELS = ("before: as the nodes were first placed", "after: one press of L")

# The relief picture: the example, the node shot, seconds of show time, and the part of the
# camera photo that holds the relief (left, top, right, bottom in the photo's pixels).
RELIEF = (EXAMPLES / "projection-onto-objects", "/view", 20, (500, 0, 920, 480))

# The library montage: the components shown, in reading order, and the columns.
LIBRARY = ROOT / "library"
LIBRARY_PICKS = ["scan_labels", "breathe", "edges", "lantern", "candle", "water"]
LIBRARY_COLUMNS = 3

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
        keep_places: bool = True) -> str:
    """Runs the app headless on a script; `project` is copied into `work` first, without its
    node positions and editor layout unless `keep_places`. Returns the run's replies."""
    script = work / f"{name}.jsonl"
    script.write_text("\n".join(json.dumps(s) for s in steps) + "\n", encoding="utf-8")
    command = [str(EXE), "--headless", str(script)]
    if project is not None:
        copy = work / name
        shutil.copytree(project, copy)
        shutil.rmtree(copy / "state", ignore_errors=True)
        if not keep_places:
            (copy / "layout.ron").unlink(missing_ok=True)
            file = copy / "project.ron"
            text = re.sub(r", pos: \[[^\]]*\]", "", file.read_text(encoding="utf-8"))
            file.write_text(text, encoding="utf-8")
        command += ["--project", str(copy), "--fresh"]
    if size is not None:
        command += ["--size", size]
    log = subprocess.run(command, capture_output=True, text=True, check=False)
    failed = [line for line in log.stdout.splitlines() if '"ok":false' in line.replace(" ", "")]
    if failed or log.returncode != 0:
        raise RuntimeError(f"{name}: the run failed\n" + "\n".join(failed) + f"\n{log.stderr}")
    return log.stdout


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
    run(steps, work, "arrange", project, size, keep_places=False)
    shots = []
    for png in (before, after):
        image = Image.open(png).convert("RGB")
        shots.append(image.crop(network_box(image)))
    try:
        font = ImageFont.truetype("segoeui.ttf", 30)
    except OSError:
        font = ImageFont.load_default(30)
    # Before above after: a network is wider than tall, and side by side the sheet would be
    # too wide to read in the README.
    label, gap = 60, 40
    width = max(shot.width for shot in shots) + gap * 2
    height = sum(shot.height + label for shot in shots) + gap
    sheet = Image.new("RGB", (width, height), (27, 27, 27))
    draw = ImageDraw.Draw(sheet)
    y = 0
    for shot, text in zip(shots, ARRANGE_LABELS):
        draw.text((gap, y + 12), text, fill=(200, 200, 200), font=font)
        sheet.paste(shot, (gap, y + label))
        y += shot.height + label
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
    # The brightest of the last frames: the beat light is on and the strips are full.
    late = [on_black(frames / f"{i:03}.png").convert("RGB") for i in range(METERS_FRAMES - 16, METERS_FRAMES)]
    pick = max(late, key=lambda im: sum(im.convert("L").getdata()))
    pick.save(OUT / "audio-meters.png", optimize=True)
    print("audio-meters: assets/gallery/audio-meters.png")


def label_font(size: int) -> ImageFont.FreeTypeFont:
    try:
        return ImageFont.truetype("segoeui.ttf", size)
    except OSError:
        return ImageFont.load_default(size)


def labelled_row(images: list[Image.Image], labels: list[str], height: int) -> Image.Image:
    """Pictures side by side at one height, each with a label above, on the editor's grey."""
    font = label_font(26)
    scaled = [image.resize((round(image.width * height / image.height), height), Image.Resampling.LANCZOS)
              for image in images]
    label, gap = 48, 24
    sheet = Image.new("RGB", (sum(s.width for s in scaled) + gap * (len(scaled) + 1), height + label + gap),
                      (27, 27, 27))
    draw = ImageDraw.Draw(sheet)
    x = gap
    for image, text in zip(scaled, labels):
        draw.text((x, 8), text, fill=(200, 200, 200), font=font)
        sheet.paste(image, (x, label))
        x += image.width + gap
    return sheet


def save_jpg(image: Image.Image, jpg: Path) -> None:
    height = round(image.height * WIDTH / image.width)
    image.resize((WIDTH, height), Image.Resampling.LANCZOS).save(jpg, quality=JPG_QUALITY)


def relief(work: Path) -> None:
    project, node, wait, box = RELIEF
    picture = work / "relief.png"
    run([call("ui.view", path=node), {"wait": wait}, call("node.snapshot", path=node, file=picture.as_posix())],
        work, "relief", project)
    lit = on_black(picture)
    lit = lit.crop(lit.getbbox() or (0, 0, lit.width, lit.height))
    photo = Image.open(project / "camera" / "photo_0.jpg").convert("RGB").crop(box)
    sheet = labelled_row([photo, lit], ["the relief, as the camera saw it", "what penumbra lights it with"], 720)
    save_jpg(sheet, OUT / "relief.jpg")
    print("relief: assets/gallery/relief.jpg")


def library(work: Path) -> None:
    font = label_font(26)
    images = [Image.open(LIBRARY / name / "preview.png").convert("RGB") for name in LIBRARY_PICKS]
    width, height = images[0].size
    label, gap = 44, 16
    rows = -(-len(images) // LIBRARY_COLUMNS)
    sheet = Image.new("RGB", (LIBRARY_COLUMNS * (width + gap) + gap, rows * (height + label + gap) + gap),
                      (27, 27, 27))
    draw = ImageDraw.Draw(sheet)
    for i, (image, name) in enumerate(zip(images, LIBRARY_PICKS)):
        x = gap + (i % LIBRARY_COLUMNS) * (width + gap)
        y = gap + (i // LIBRARY_COLUMNS) * (height + label + gap)
        draw.text((x, y + 4), name, fill=(200, 200, 200), font=font)
        sheet.paste(image.resize((width, height)), (x, y + label))
    save_jpg(sheet, OUT / "library.jpg")
    print("library: assets/gallery/library.jpg")


def palettes(work: Path) -> None:
    replies = run([call("palette.list")], work, "palettes")
    line = next(line for line in replies.splitlines() if line.startswith("palette.list:"))
    entries = json.loads(line.split(":", 1)[1])["result"]["palettes"]
    font = label_font(22)
    name_width, band, row, gap, columns = 130, 560, 30, 6, 2
    rows = -(-len(entries) // columns)
    sheet = Image.new("RGB", (columns * (name_width + band + 2 * gap) + gap, rows * (row + gap) + gap),
                      (27, 27, 27))
    draw = ImageDraw.Draw(sheet)
    for i, entry in enumerate(entries):
        x = gap + (i // rows) * (name_width + band + 2 * gap)
        y = gap + (i % rows) * (row + gap)
        draw.text((x, y + 2), entry["name"], fill=(200, 200, 200), font=font)
        colours = [tuple(int(h.lstrip("#")[k:k + 2], 16) for k in (0, 2, 4)) for h in entry["colours"]]
        strip = Image.new("RGB", (len(colours), 1))
        strip.putdata(colours)
        # Palettes as bands, colormaps blended.
        method = Image.Resampling.NEAREST if entry["kind"] == "palette" else Image.Resampling.BILINEAR
        sheet.paste(strip.resize((band, row), method), (x + name_width, y))
    sheet.save(OUT / "palettes.png", optimize=True)
    print("palettes: assets/gallery/palettes.png")


def main() -> int:
    jobs = {entry[0]: (lambda work, entry=entry: showcase(*entry, work)) for entry in SHOWCASE}
    jobs.update({"window": window, "arrange": arrange, "audio-meters": meters, "relief": relief,
                 "library": library, "palettes": palettes})
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
