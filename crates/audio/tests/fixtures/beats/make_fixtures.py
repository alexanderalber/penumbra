"""Writes the reference data for the Beat This! signal chain tests in `crates/audio/src/beats_tests.rs`.

The references come from the upstream Python package `beat_this` (CPJKU, MIT) and torchaudio:
the log-mel spectrogram (`beat_this.preprocessing.LogMelSpect`), the chunk plan
(`beat_this.inference.split_piece`) and the "minimal" post-processing
(`beat_this.model.postprocessor.Postprocessor`).

Run with an interpreter that has `beat_this`, torch and torchaudio installed. With no arguments
it writes every fixture into the directory of this script. All files are little-endian:
`*.f32` holds float32, `*.f64` float64, `*.txt` is text.
"""

from pathlib import Path

import numpy as np
import torch
from beat_this.inference import split_piece
from beat_this.model.postprocessor import Postprocessor
from beat_this.preprocessing import LogMelSpect

OUT = Path(__file__).resolve().parent
SR = 22050


def synthetic(seconds: float, extra: int, seed: int) -> np.ndarray:
    """Clicks, a chirp, noise, a silent gap and a level step at 22.05 kHz."""
    rng = np.random.default_rng(seed)
    n = int(seconds * SR) + extra
    t = np.arange(n) / SR
    x = 0.2 * np.sin(2 * np.pi * (100 * t + 2000 * t * t))
    x += 0.05 * rng.standard_normal(n)
    for c in np.arange(0.1, seconds, 0.5):
        i = int(c * SR)
        k = np.arange(min(400, n - i))
        x[i : i + len(k)] += 0.8 * np.exp(-k / 60.0) * np.sin(2 * np.pi * 60 * k / SR)
    x[int(0.9 * SR) : int(1.1 * SR)] = 0.0
    x[int(1.5 * SR) :] *= 0.05
    return x.astype(np.float32)


def mel(name: str, x: np.ndarray) -> None:
    x.tofile(OUT / f"{name}.pcm.f32")
    spect = LogMelSpect()(torch.from_numpy(x)).numpy().astype(np.float32)
    spect.tofile(OUT / f"{name}.spect.f32")
    print(name, len(x), "samples", spect.shape)


def chunk_plans() -> None:
    lines = []
    for n in [1, 2, 7, 100, 1487, 1488, 1489, 1490, 1493, 1494, 1495, 1499, 1500, 1501,
              2975, 2976, 2977, 2988, 3000, 4463, 4464, 4465, 5000, 352058]:
        spect = torch.zeros(n, 1)
        chunks, starts = split_piece(spect, 1500, border_size=6, avoid_short_end=True)
        pairs = " ".join(f"{int(s)}:{c.shape[0]}" for s, c in zip(starts, chunks))
        lines.append(f"{n} {pairs}")
    (OUT / "chunk_plans.txt").write_text("\n".join(lines) + "\n")
    print("chunk plans", len(lines))


def logits() -> None:
    """Logits with plateaus (adjacent equal peaks), ties between beats and peaks at both ends."""
    rng = np.random.default_rng(7)
    n = 3000
    beat = np.full(n, -8.0)
    down = np.full(n, -8.0)
    pos = 3.0
    k = 0
    while pos < n - 1:
        i = int(pos)
        width = rng.integers(0, 3)
        beat[i : i + 1 + width] = np.maximum(beat[i : i + 1 + width], rng.choice([0.5, 2.0, 4.0]))
        if k % 4 == 0:
            j = i + rng.integers(-2, 3)
            if 0 <= j < n:
                down[j : j + 1 + rng.integers(0, 2)] = rng.choice([0.5, 3.0])
        k += 1
        pos += rng.uniform(18, 32)
    beat += np.round(rng.standard_normal(n) * 2) / 4
    down += np.round(rng.standard_normal(n) * 2) / 4
    beat[0] = 5.0
    beat[-1] = 5.0
    down[-2] = 2.0
    # a downbeat exactly between two beats: the earlier one wins in numpy's argmin
    beat[2000:2040] = -8.0
    down[2000:2040] = -8.0
    beat[2005] = 3.0
    beat[2015] = 3.0
    down[2010] = 3.0
    beat = beat.astype(np.float32)
    down = down.astype(np.float32)
    np.stack([beat, down], 1).astype(np.float32).tofile(OUT / "post.logits.f32")
    b, d = Postprocessor("minimal")(torch.from_numpy(beat), torch.from_numpy(down))
    np.asarray(b, dtype=np.float64).tofile(OUT / "post.beats.f64")
    np.asarray(d, dtype=np.float64).tofile(OUT / "post.downs.f64")
    print("post", len(b), "beats", len(d), "downbeats")


if __name__ == "__main__":
    mel("synth", synthetic(2.0, 123, 1))
    mel("short", synthetic(0.0, 600, 2))
    chunk_plans()
    logits()
