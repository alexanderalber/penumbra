"""Renders the clip of the clips example: examples/clips/clip.mp4.

Run it with no arguments (the Run button works). It needs penumbra built in release
(cargo build --release -p penumbra) and ffmpeg on PATH; Python's standard library is enough.

What it does:

1. Copies project.ron and swing.wgsl from this folder into a scratch folder.
2. Runs penumbra headless on that copy for 21 seconds. The lfo /take turns /record's `record`
   on at 7.2 s of show time and off at 18 s, so the take is three swings of 3.6 s as ProRes 422
   with 24-bit PCM sound (render/take.mov).
3. Checks the take: 648 frames, none dropped or skipped, 518 400 samples, no gap.
4. Keeps the take's second swing (frames 216 to 431, samples 172 800 to 345 599): by then the
   notes of the swing before have rung out the same way every time. ffmpeg writes clip.mp4
   from it: H.264 at 540 x 540 and 60 fps, AAC at 256 kbit/s, mono, 48 kHz.

Options: --exe (penumbra.exe), --out (folder for clip.mp4), --port (control
channel of the headless run), --keep (keep the scratch folder and say where it is).
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
RATE = 60
SAMPLE_RATE = 48000
SWING_FRAMES = 216  # 3.6 s at 60 fps
SWING_SAMPLES = 172800  # 3.6 s at 48 kHz
TAKE_SWINGS = 3
KEEP_SWING = 1  # the second swing of the take, counted from 0

SCRIPT = [
    {"wait": 21},
    {"method": "video_out.status", "params": {"path": "/record"}},
]


def find_exe(given):
    if given:
        return Path(given)
    exe = REPO / "target" / "release" / ("penumbra.exe" if os.name == "nt" else "penumbra")
    if exe.exists():
        return exe
    found = shutil.which("penumbra")
    if found:
        return Path(found)
    sys.exit(f"penumbra not found at {exe}; build it with cargo build --release -p penumbra, or pass --exe")


def render(exe, work, port):
    project = work / "project"
    project.mkdir()
    for name in ("project.ron", "swing.wgsl"):
        shutil.copy(HERE / name, project / name)
    script = work / "render.jsonl"
    script.write_text("".join(json.dumps(line) + "\n" for line in SCRIPT), encoding="utf-8")
    env = dict(os.environ)
    # The run's settings and recent projects go to the scratch folder, not the user's.
    env["LOCALAPPDATA"] = str(work / "appdata")
    env["XDG_DATA_HOME"] = str(work / "appdata")
    env["PENUMBRA_PORT"] = str(port)
    print(f"rendering with {exe} (about 25 s)")
    run = subprocess.run(
        [str(exe), "--project", str(project), "--headless", str(script)],
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=180,
    )
    status = None
    for line in run.stdout.splitlines() + run.stderr.splitlines():
        if line.startswith("video_out.status:"):
            status = json.loads(line.split(":", 1)[1])
    if run.returncode != 0 or status is None or not status.get("ok"):
        sys.exit(f"the headless run failed (exit code {run.returncode}):\n{run.stdout}\n{run.stderr}")
    result = status["result"]
    take = result["recordings"][0] if "recordings" in result else result
    sound = take.get("audio") or {}
    frames = SWING_FRAMES * TAKE_SWINGS
    samples = SWING_SAMPLES * TAKE_SWINGS
    problems = []
    if not take.get("done") or take.get("error"):
        problems.append(f"the take is not finished: {take.get('error')}")
    if take.get("frames") != frames or take.get("dropped") or take.get("skipped"):
        problems.append(f"frames {take.get('frames')} (want {frames}), dropped {take.get('dropped')}, skipped {take.get('skipped')}")
    if sound.get("samples") != samples or sound.get("gap_samples") or sound.get("dropped"):
        problems.append(f"samples {sound.get('samples')} (want {samples}), gap {sound.get('gap_samples')}, dropped {sound.get('dropped')}")
    if problems:
        sys.exit("the take is not usable, run again on a less busy machine:\n  " + "\n  ".join(problems))
    return Path(take["file"])


def cut(take, out):
    first_frame = SWING_FRAMES * KEEP_SWING
    first_sample = SWING_SAMPLES * KEEP_SWING
    video = f"[0:v]trim=start_frame={first_frame}:end_frame={first_frame + SWING_FRAMES},setpts=PTS-STARTPTS[v]"
    audio = (
        f"[0:a]atrim=start_sample={first_sample}:end_sample={first_sample + SWING_SAMPLES},"
        "asetpts=PTS-STARTPTS,pan=mono|c0=c0[a]"
    )
    out.mkdir(parents=True, exist_ok=True)
    mp4 = out / "clip.mp4"
    common = ["ffmpeg", "-v", "error", "-y", "-i", str(take), "-filter_complex", f"{video};{audio}"]
    subprocess.run(
        common
        + ["-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "veryslow", "-crf", "20"]
        + ["-pix_fmt", "yuv420p", "-color_range", "tv", "-colorspace", "bt709"]
        + ["-color_primaries", "bt709", "-color_trc", "bt709", "-r", str(RATE)]
        + ["-c:a", "aac", "-b:a", "256k", "-ar", str(SAMPLE_RATE), "-movflags", "+faststart", str(mp4)],
        check=True,
    )
    print(f"wrote {mp4} ({mp4.stat().st_size // 1024} KB)")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--exe", help="penumbra executable (default: the release build of this checkout)")
    parser.add_argument("--out", default=str(HERE.parent), help="folder for clip.mp4 (default: examples/clips)")
    parser.add_argument("--port", type=int, default=9893, help="control channel port of the headless run (default 9893)")
    parser.add_argument("--keep", action="store_true", help="keep the scratch folder with the take")
    args = parser.parse_args()
    if shutil.which("ffmpeg") is None:
        sys.exit("ffmpeg is not on PATH")
    exe = find_exe(args.exe)
    work = Path(tempfile.mkdtemp(prefix="penumbra-clip-"))
    try:
        take = render(exe, work, args.port)
        cut(take, Path(args.out))
    finally:
        if args.keep:
            print(f"scratch folder kept: {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
