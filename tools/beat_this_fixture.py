"""Writes the test fixture of penumbra-nn's Beat This! port: a tiny Beat This! with random
weights, an input spectrogram and the logits PyTorch computes for it.

Output, in crates/nn/tests/fixtures/:

- beat_this_tiny.safetensors: the weights, written as tools/beat_this_convert.py writes a
  checkpoint (transformer width 32, 2 layers, heads of 8, stem of 8 channels).
- beat_this_tiny_io.safetensors: `spect` (157 frames x 128 bands), `beat` and `downbeat`
  (157 logits each).

The weights are drawn with a fixed seed and scaled so every layer passes on a signal of order 1;
the model's own initialisation (std 0.02) would leave the later layers close to zero and test
little. BatchNorm statistics are random too, so their folding is tested. The reference runs in
float64; the float32 PyTorch result is printed for scale.

Run it with no arguments. It needs Python 3.10 or later with torch, numpy and the `beat_this`
package (`pip install beat_this`, which brings `rotary_embedding_torch`).
"""

import importlib.util
import sys
from pathlib import Path

import numpy as np
import torch
from beat_this.model.beat_tracker import BeatThis

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "crates" / "nn" / "tests" / "fixtures"
FRAMES = 157
SEED = 20241110

# The converter's writer, loaded by path so the script also runs under `python -I`, and without
# leaving a __pycache__ folder in tools/.
sys.dont_write_bytecode = True
_spec = importlib.util.spec_from_file_location("beat_this_convert", Path(__file__).with_name("beat_this_convert.py"))
convert = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(convert)


def randomise(model, gen):
    """Weights of order 1 / sqrt(fan in), norms near 1, small biases."""
    with torch.no_grad():
        for name, p in model.named_parameters():
            if name.endswith("rotary_embed.freqs"):
                continue
            if p.ndim >= 2:
                fan_in = p[0].numel()
                p.copy_(torch.randn(p.shape, generator=gen) / fan_in**0.5)
            elif name.endswith("gamma") or name.endswith("weight"):
                p.copy_(1.0 + 0.2 * torch.randn(p.shape, generator=gen))
            else:
                p.copy_(0.1 * torch.randn(p.shape, generator=gen))
        for name, b in model.named_buffers():
            if name.endswith("running_mean"):
                b.copy_(0.3 * torch.randn(b.shape, generator=gen))
            elif name.endswith("running_var"):
                b.copy_(0.5 + torch.rand(b.shape, generator=gen))


def main():
    gen = torch.Generator().manual_seed(SEED)
    hparams = dict(spect_dim=128, transformer_dim=32, ff_mult=4, n_layers=2, head_dim=8, stem_dim=8)
    model = BeatThis(**hparams, sum_head=True, partial_transformers=True).eval()
    randomise(model, gen)
    # Values in the range of real input (log1p(1000 * mel) lies between 0 and about 8).
    spect = (4.0 * torch.rand((FRAMES, 128), generator=gen)).double()

    with torch.no_grad():
        ref = model.double()(spect[None])
        f32 = model.float()(spect.float()[None])
    beat, down = ref["beat"][0], ref["downbeat"][0]
    diff = max((f32["beat"][0].double() - beat).abs().max().item(), (f32["downbeat"][0].double() - down).abs().max().item())
    print(f"logits: beat {beat.min().item():.3f}..{beat.max().item():.3f}, downbeat {down.min().item():.3f}..{down.max().item():.3f}")
    print(f"PyTorch float32 vs float64: max |diff| {diff:.2e}")

    OUT.mkdir(parents=True, exist_ok=True)
    state = {k: v for k, v in model.float().state_dict().items()}
    tensors = convert.state_to_tensors(state)
    meta = convert.model_metadata(hparams)
    meta["source"] = f"random, seed {SEED}"
    w = OUT / "beat_this_tiny.safetensors"
    convert.write_safetensors(w, tensors, meta)
    io = OUT / "beat_this_tiny_io.safetensors"
    convert.write_safetensors(
        io,
        {"spect": spect.float().numpy(), "beat": beat.float().numpy(), "downbeat": down.float().numpy()},
        {"reference": "PyTorch float64"},
    )
    for p in (w, io):
        print(p.relative_to(ROOT), p.stat().st_size, "bytes")


if __name__ == "__main__":
    main()
