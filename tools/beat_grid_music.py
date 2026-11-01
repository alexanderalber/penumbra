"""Writes the music of the beat-grid example: examples/beat-grid/music.wav, an original track
made from nothing but numbers, so it can ship in the repository.

The track is 32 bars of 4/4 at 124 BPM (a beat is 10 670 samples, 123.993 BPM exactly), about
62 seconds, mono, 16-bit, 22 050 Hz. The first beat is at 0 s and the file ends where bar 33
would begin, so it loops without a seam: the tails of the last bar sound again at the start.

- Bars 1 to 4: kick and hats, the bass comes in.
- Bars 5 to 16: kick on every beat, snare on 2 and 4, hats, bass, a chord on each bar's first
  beat. A crash every four bars.
- Bars 17 to 24: the breakdown. No kick and no snare, only chords, a soft bass and quiet hats.
  In its last bar the snare rolls up.
- Bars 25 to 32: everything again.
- The bass changes note every bar (A, F, C, G). Bars 8, 16 and 32 end with a fill.

Run it with no arguments. Python's standard library is enough. With `--times DIR` it also
writes the beat and downbeat times (seconds, one per line) to DIR/beats.txt and
DIR/downbeats.txt, for comparing with what the beat tracker finds. The noise comes from a fixed
seed, so every run writes the same file.
"""

import argparse
import math
import random
import struct
import sys
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "examples" / "beat-grid" / "music.wav"
RATE = 22050
BEAT = 10670  # samples: 123.996 BPM
BEATS_PER_BAR = 4
BARS = 32
LENGTH = BEAT * BEATS_PER_BAR * BARS
SEED = 124
PEAK = 10 ** (-1.0 / 20)  # -1 dBFS

# Bars counted from 0.
BREAK = range(16, 24)
FILL_BARS = {7, 15, 31}
ROLL_BAR = 23
ROOTS = [45, 41, 48, 43]  # MIDI: A2, F2, C3, G2
CHORDS = [[57, 60, 64], [57, 60, 65], [55, 60, 64], [55, 59, 62]]  # Am, F, C, G


def hz(note):
    return 440.0 * 2 ** ((note - 69) / 12)


class Mix:
    def __init__(self):
        self.buf = [0.0] * LENGTH

    def add(self, start, samples, gain=1.0):
        """Adds `samples` from sample `start` on, wrapping past the end to the beginning."""
        buf = self.buf
        for i, s in enumerate(samples):
            buf[(start + i) % LENGTH] += gain * s


def kick():
    n = int(0.35 * RATE)
    out, phase = [], 0.0
    for i in range(n):
        t = i / RATE
        f = 48 + 110 * math.exp(-t * 28)
        phase += 2 * math.pi * f / RATE
        env = math.exp(-t * 9)
        click = math.exp(-t * 400) * 0.4
        out.append(math.sin(phase) * env + click)
    return out


def noise(rng, n):
    return [rng.uniform(-1, 1) for _ in range(n)]


def highpass(x, a):
    out, prev_x, prev_y = [], 0.0, 0.0
    for s in x:
        y = a * (prev_y + s - prev_x)
        prev_x, prev_y = s, y
        out.append(y)
    return out


def lowpass(x, a):
    out, y = [], 0.0
    for s in x:
        y += a * (s - y)
        out.append(y)
    return out


def snare(rng):
    n = int(0.22 * RATE)
    body = highpass(noise(rng, n), 0.75)
    out = []
    for i in range(n):
        t = i / RATE
        tone = math.sin(2 * math.pi * 185 * t) * math.exp(-t * 30)
        out.append(body[i] * math.exp(-t * 18) * 0.8 + tone * 0.6)
    return out


def hat(rng, length):
    n = int(length * 6 * RATE)
    body = highpass(highpass(noise(rng, n), 0.5), 0.5)
    return [s * math.exp(-i / RATE / length) for i, s in enumerate(body)]


def crash(rng):
    n = int(1.6 * RATE)
    body = highpass(noise(rng, n), 0.6)
    return [s * math.exp(-i / RATE * 2.6) for i, s in enumerate(body)]


def bass_note(note, seconds, bright):
    n = int(seconds * RATE)
    f = hz(note)
    raw = []
    for i in range(n):
        p = (i * f / RATE) % 1.0
        raw.append(2 * p - 1)
    out = lowpass(raw, bright)
    for i in range(n):
        t = i / RATE
        env = min(1.0, t / 0.004) * math.exp(-t * 3.0)
        release = min(1.0, (n - i) / (0.01 * RATE))
        out[i] *= env * release
    return out


def chord(notes, seconds, rise):
    n = int(seconds * RATE)
    raw = [0.0] * n
    for note in notes:
        for detune in (-0.004, 0.004):
            f = hz(note) * (1 + detune)
            for i in range(n):
                raw[i] += 2 * ((i * f / RATE) % 1.0) - 1
    out = lowpass(raw, 0.08)
    for i in range(n):
        t = i / RATE
        env = min(1.0, t / rise) * math.exp(-t * 1.4)
        release = min(1.0, (n - i) / (0.05 * RATE))
        out[i] *= env * release / (2 * len(notes))
    return out


def track():
    rng = random.Random(SEED)
    mix = Mix()
    k, s, cr = kick(), snare(rng), crash(rng)
    closed = [hat(rng, 0.018) for _ in range(4)]
    open_hat = hat(rng, 0.09)
    bar_len = BEAT * BEATS_PER_BAR
    eighth, sixteenth = BEAT // 2, BEAT // 4
    for bar in range(BARS):
        b0 = bar * bar_len
        in_break = bar in BREAK
        full = bar >= 4 and not in_break
        fill = bar in FILL_BARS
        chord_i = bar % 4
        # Kick on every beat, snare on 2 and 4, outside the breakdown.
        if not in_break:
            for beat in range(BEATS_PER_BAR):
                if fill and beat == 3:
                    continue
                mix.add(b0 + beat * BEAT, k, 0.9)
        if full:
            for beat in (1, 3):
                if fill and beat == 3:
                    continue
                mix.add(b0 + beat * BEAT, s, 0.55)
        if fill:
            # The last beat: four snare hits rising in level, then a kick.
            for i in range(4):
                mix.add(b0 + 3 * BEAT + i * sixteenth, s, 0.3 + 0.12 * i)
        if bar == ROLL_BAR:
            # The breakdown's last bar: the snare rolls up, eighths then sixteenths.
            for i in range(8):
                mix.add(b0 + i * eighth, s, 0.12 + 0.03 * i)
            for i in range(8):
                mix.add(b0 + 2 * BEAT + i * sixteenth, s, 0.3 + 0.04 * i)
        # Hats: open on the offbeat eighths, closed sixteenths between.
        hat_gain = 0.35 if in_break else 0.6
        for i in range(BEATS_PER_BAR * 4):
            at = b0 + i * sixteenth
            if i % 4 == 2:
                mix.add(at, open_hat, 0.35 * hat_gain)
            elif bar >= 2:
                mix.add(at, closed[i % 4], (0.22 if i % 2 else 0.12) * hat_gain)
        # Crash on the first beat of every four bars once the drums are full, and out of the
        # breakdown.
        if (full and bar % 4 == 0) or bar == 24:
            mix.add(b0, cr, 0.28)
        # Bass: the root of the bar's chord, on the first beat long and loud, then eighths
        # off the beat.
        root = ROOTS[chord_i]
        if bar >= 2:
            bright = 0.08 if in_break else 0.18
            gain = 0.35 if in_break else 0.55
            mix.add(b0, bass_note(root, 0.42, bright), gain * 1.3)
            for beat in range(1, BEATS_PER_BAR):
                mix.add(b0 + beat * BEAT + eighth, bass_note(root + (12 if beat == 2 else 0), 0.2, bright), gain)
            if not in_break:
                mix.add(b0 + eighth, bass_note(root, 0.2, bright), gain * 0.8)
        # A chord on each bar's first beat: a stab in the groove, a slow swell in the breakdown.
        if bar >= 4:
            if in_break:
                mix.add(b0, chord(CHORDS[chord_i], bar_len / RATE, 0.02), 0.9)
            else:
                mix.add(b0, chord(CHORDS[chord_i], 0.9, 0.005), 0.55)
    return mix.buf


def write_wav(path, samples):
    peak = max(abs(s) for s in samples) or 1.0
    scale = PEAK / peak * 32767
    frames = b"".join(struct.pack("<h", max(-32768, min(32767, round(s * scale)))) for s in samples)
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(frames)


def write_times(folder):
    folder.mkdir(parents=True, exist_ok=True)
    beats = [i * BEAT / RATE for i in range(BARS * BEATS_PER_BAR)]
    (folder / "beats.txt").write_text("".join(f"{t:.6f}\n" for t in beats), encoding="utf-8")
    downs = beats[::BEATS_PER_BAR]
    (folder / "downbeats.txt").write_text("".join(f"{t:.6f}\n" for t in downs), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--out", type=Path, default=OUT, help=f"the WAV to write (default {OUT})")
    parser.add_argument("--times", type=Path, help="folder for beats.txt and downbeats.txt")
    args = parser.parse_args()
    print(f"synthesising {BARS} bars at {60 * RATE / BEAT:.3f} BPM, {LENGTH / RATE:.2f} s")
    write_wav(args.out, track())
    print(f"wrote {args.out} ({args.out.stat().st_size / 1e6:.2f} MB)")
    if args.times:
        write_times(args.times)
        print(f"wrote {args.times / 'beats.txt'} and {args.times / 'downbeats.txt'}")


if __name__ == "__main__":
    sys.exit(main())
