"""Converts Beat This! checkpoints (.ckpt, PyTorch Lightning) into .safetensors files for
penumbra-nn (crates/nn), quantized or as plain F32.

Run it with no arguments to write the model penumbra ships:
`assets/models/beat_this-final0-q6.safetensors`, quantized from `beat_this-final0.ckpt` in torch
hub's cache (`~/.cache/torch/hub/checkpoints/`, where the `beat_this` package downloads it).
Other uses:

- `python tools/beat_this_convert.py a.ckpt b.ckpt --out DIR`: quantize other checkpoints.
- `python tools/beat_this_convert.py --fp32 [a.ckpt ...] [--out DIR]`: plain F32, as before.
  Without checkpoints it converts final0 and small0 from the hub cache, each beside its
  checkpoint.

Both outputs hold the network's tensors under their PyTorch names without the `model.` prefix.
BatchNorm stays unfolded: penumbra-nn folds it at load time. The counters `num_batches_tracked`
are dropped. The metadata records the source file and its SHA-256, and `head_dim`, `n_layers`
and `sum_head` from the checkpoint's hyperparameters.

The quantized file ("int6 tf + int8 rest" in the quantization study; about 17 MB for final0
instead of 81 MB; beats and downbeats on a 2 h mix agree with F32 to F-measure 0.999 and 0.995):

- The Linear weights of the RoFormer (`transformer_blocks.*`): int6, symmetric, levels -31..31,
  one F16 scale per group of 32 values along the input axis. The codes are stored as `q + 32`
  (1..63), four in three bytes: a 24-bit little-endian word holding the first code in its low
  6 bits.
- Every other Linear and Conv weight (stem, frontend, frontend linear, head): int8, symmetric,
  levels -127..127, one F16 scale per output channel.
- Everything else (biases, norm gains, BatchNorm statistics, rotary frequencies): F32.

A quantized weight `W` of shape [O, ...] is stored as two tensors: `W.q` (U8 [O, K * 3 / 4]
for int6, I8 [O, K] for int8, K the product of the other axes) and `W.scale` (F16
[O, K / group]). The metadata key `quant.W` holds JSON: `bits`, `group`, `shape` and `layout`
(`packed6` or `int8`). Value `k` of row `o` is `code * scale[o, k // group]`, computed in F32.
Scales are the group's largest magnitude over the top level, rounded to F16, and codes are
`round(w / scale)` (half to even) clamped to the levels. The output is the same bytes on every
run.

It needs Python 3.10 or later with torch and numpy. The `beat_this` package is not needed.
"""

import hashlib
import json
import sys
from pathlib import Path

import numpy as np
import torch

HUB = Path.home() / ".cache" / "torch" / "hub" / "checkpoints"
ROOT = Path(__file__).resolve().parents[1]
SHIPPED_DIR = ROOT / "assets" / "models"
FP32_DEFAULT = [HUB / "beat_this-final0.ckpt", HUB / "beat_this-small0.ckpt"]
QUANT_DEFAULT = [HUB / "beat_this-final0.ckpt"]
GROUP = 32
QUANT_VERSION = "q6"


def write_safetensors(path, tensors, metadata):
    """Writes `tensors` (name to numpy array of float32, float16, uint8 or int8) as a
    safetensors file. Names are sorted and the JSON is compact, so equal input gives equal
    bytes."""
    dtypes = {np.dtype("<f4"): "F32", np.dtype("<f2"): "F16", np.dtype("u1"): "U8", np.dtype("i1"): "I8"}
    header = {"__metadata__": {k: str(v) for k, v in sorted(metadata.items())}}
    offset = 0
    blobs = []
    for name in sorted(tensors):
        a = np.ascontiguousarray(tensors[name])
        a = a.astype(a.dtype.newbyteorder("<"), copy=False)
        dtype = dtypes.get(a.dtype)
        if dtype is None:
            raise ValueError(f"{name} has dtype {a.dtype}, which this writer does not store")
        b = a.tobytes()
        header[name] = {"dtype": dtype, "shape": list(a.shape), "data_offsets": [offset, offset + len(b)]}
        offset += len(b)
        blobs.append(b)
    raw = json.dumps(header, separators=(",", ":")).encode()
    raw += b" " * (-len(raw) % 8)  # the data starts 8-byte aligned
    with open(path, "wb") as f:
        f.write(len(raw).to_bytes(8, "little"))
        f.write(raw)
        for b in blobs:
            f.write(b)


def state_to_tensors(state_dict):
    """The float tensors of a BeatThis state dict, without prefixes and counters, as torch
    float32 tensors on the CPU."""
    out = {}
    for key, value in state_dict.items():
        name = key.replace("_orig_mod.", "")
        if name.startswith("model."):
            name = name[len("model."):]
        if name.endswith("num_batches_tracked"):
            continue
        if not torch.is_floating_point(value):
            raise ValueError(f"{key} is {value.dtype}, expected a float tensor")
        out[name] = value.detach().to(torch.float32).cpu()
    return out


def model_metadata(hparams):
    """The settings penumbra-nn cannot read from the weights' shapes."""
    return {
        "model": "beat_this",
        "head_dim": int(hparams.get("head_dim", 32)),
        "n_layers": int(hparams.get("n_layers", 6)),
        "sum_head": "true" if hparams.get("sum_head", True) else "false",
    }


def is_qweight(name, t):
    """Linear and Conv weights: named `weight` with two or more axes. Norm gains, BatchNorm,
    biases and rotary frequencies have one axis or other names and stay F32."""
    return name.endswith(".weight") and t.ndim >= 2


def quantize(t, bits, group):
    """Symmetric quantization of `t` in rows of the first axis, `group` values per scale along
    the rest. Returns integer codes [O, K] (torch int32) and F16 scales [O, K / group] as
    numpy. Matches the quantization study's `q_group` (int6) and `q_int8_channel` (int8)."""
    w = t.reshape(t.shape[0], -1)
    rows, k = w.shape
    if k % group:
        raise ValueError(f"{k} values per row do not split into groups of {group}")
    wg = w.reshape(rows, k // group, group)
    qmax = 2 ** (bits - 1) - 1
    s16 = (wg.abs().amax(-1, keepdim=True).clamp_min(1e-12) / qmax).half()
    if not torch.isfinite(s16).all() or (s16 <= 0).any():
        raise ValueError("a scale is zero or not finite in F16: a group of near-zero weights")
    q = torch.clamp(torch.round(wg / s16.float()), -qmax, qmax).to(torch.int32)
    return q.reshape(rows, k), s16.reshape(rows, k // group).numpy()


def pack6(codes):
    """int6 codes [O, K] (-31..31) to bytes [O, K * 3 / 4]: `code + 32`, four codes in a
    24-bit little-endian word, the first code in the low bits."""
    rows, k = codes.shape
    if k % 4:
        raise ValueError(f"{k} codes per row are not a multiple of 4")
    u = (codes + 32).numpy().astype(np.uint32).reshape(rows, k // 4, 4)
    word = u[..., 0] | (u[..., 1] << 6) | (u[..., 2] << 12) | (u[..., 3] << 18)
    out = np.stack([word & 0xFF, (word >> 8) & 0xFF, (word >> 16) & 0xFF], axis=-1)
    return out.astype(np.uint8).reshape(rows, k // 4 * 3)


def quantize_tensors(tensors):
    """The quantized file's tensors (numpy) and its `quant.*` metadata."""
    out, meta = {}, {}
    for name, t in tensors.items():
        if not is_qweight(name, t):
            out[name] = t.numpy().astype("<f4")
            continue
        k = t.numel() // t.shape[0]
        if name.startswith("transformer_blocks."):
            bits, group, layout = 6, GROUP, "packed6"
            codes, scales = quantize(t, 6, GROUP)
            out[f"{name}.q"] = pack6(codes)
        else:
            bits, group, layout = 8, k, "int8"
            codes, scales = quantize(t, 8, k)
            out[f"{name}.q"] = codes.numpy().astype(np.int8)
        out[f"{name}.scale"] = scales.astype("<f2")
        spec = {"bits": bits, "group": group, "layout": layout, "shape": list(t.shape)}
        meta[f"quant.{name}"] = json.dumps(spec, separators=(",", ":"), sort_keys=True)
    return out, meta


def convert(ckpt, out_dir, fp32):
    data = Path(ckpt).read_bytes()
    checkpoint = torch.load(ckpt, map_location="cpu", weights_only=True)
    tensors = state_to_tensors(checkpoint["state_dict"])
    meta = model_metadata(checkpoint.get("hyper_parameters", {}))
    meta["source"] = Path(ckpt).name
    meta["source_sha256"] = hashlib.sha256(data).hexdigest()
    name = Path(ckpt).stem.removeprefix("beat_this-")
    if fp32:
        out = Path(out_dir or Path(ckpt).parent) / f"beat_this-{name}.safetensors"
        arrays = {k: v.numpy().astype("<f4") for k, v in tensors.items()}
        what = "F32"
    else:
        out = Path(out_dir or SHIPPED_DIR) / f"beat_this-{name}-{QUANT_VERSION}.safetensors"
        arrays, qmeta = quantize_tensors(tensors)
        meta.update(qmeta)
        meta["model_id"] = f"beat_this-{name}-{QUANT_VERSION}"
        what = f"quantized ({len(qmeta)} weights: RoFormer int6 in groups of {GROUP}, the rest int8)"
    out.parent.mkdir(parents=True, exist_ok=True)
    write_safetensors(out, arrays, meta)
    count = sum(v.numel() for v in tensors.values())
    print(f"{ckpt} -> {out}")
    print(f"  {what}: {len(tensors)} tensors, {count} values, {out.stat().st_size} bytes")
    print(f"  source SHA-256 {meta['source_sha256']}")
    print(f"  output SHA-256 {hashlib.sha256(out.read_bytes()).hexdigest()}")
    return out


def main(argv):
    out_dir = None
    fp32 = "--fp32" in argv
    if fp32:
        argv.remove("--fp32")
    if "--out" in argv:
        i = argv.index("--out")
        if i + 1 >= len(argv):
            print("--out needs a folder")
            return 2
        out_dir = argv[i + 1]
        del argv[i : i + 2]
    ckpts = [Path(a) for a in argv] or (FP32_DEFAULT if fp32 else QUANT_DEFAULT)
    missing = [c for c in ckpts if not c.exists()]
    if missing:
        print("missing:", *missing, sep="\n  ")
        print("Run `beat_this` once to download the checkpoints, or name them as arguments.")
        return 1
    for c in ckpts:
        convert(c, out_dir, fp32)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
