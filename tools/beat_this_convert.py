"""Converts Beat This! checkpoints (.ckpt, PyTorch Lightning) into .safetensors files for
penumbra-nn (crates/nn).

The output holds the network's float tensors under their PyTorch names without the `model.`
prefix, all as F32. BatchNorm stays unfolded: penumbra-nn folds it at load time. The counters
`num_batches_tracked` are dropped. The metadata records the source file, `head_dim`,
`n_layers` and `sum_head` from the checkpoint's hyperparameters, and the SHA-256 of the checkpoint.

Run it with no arguments to convert `beat_this-final0.ckpt` and `beat_this-small0.ckpt` from
torch hub's cache (`~/.cache/torch/hub/checkpoints/`, where the `beat_this` package downloads
them); each .safetensors file lands beside its checkpoint. Or name checkpoints and an output
folder: `python tools/beat_this_convert.py a.ckpt b.ckpt --out DIR`.

It needs Python 3.10 or later with torch and numpy. The `beat_this` package is not needed.
"""

import hashlib
import json
import sys
from pathlib import Path

import numpy as np
import torch

HUB = Path.home() / ".cache" / "torch" / "hub" / "checkpoints"
DEFAULT = [HUB / "beat_this-final0.ckpt", HUB / "beat_this-small0.ckpt"]


def write_safetensors(path, tensors, metadata):
    """Writes `tensors` (name to float32 numpy array) as a safetensors file."""
    header = {"__metadata__": {k: str(v) for k, v in metadata.items()}}
    offset = 0
    blobs = []
    for name in sorted(tensors):
        a = np.ascontiguousarray(tensors[name], dtype="<f4")
        b = a.tobytes()
        header[name] = {"dtype": "F32", "shape": list(a.shape), "data_offsets": [offset, offset + len(b)]}
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
    """The float tensors of a BeatThis state dict, without prefixes and counters."""
    out = {}
    for key, value in state_dict.items():
        name = key.replace("_orig_mod.", "")
        if name.startswith("model."):
            name = name[len("model."):]
        if name.endswith("num_batches_tracked"):
            continue
        if not torch.is_floating_point(value):
            raise ValueError(f"{key} is {value.dtype}, expected a float tensor")
        out[name] = value.detach().to(torch.float32).cpu().numpy()
    return out


def model_metadata(hparams):
    """The settings penumbra-nn cannot read from the weights' shapes."""
    return {
        "model": "beat_this",
        "head_dim": int(hparams.get("head_dim", 32)),
        "n_layers": int(hparams.get("n_layers", 6)),
        "sum_head": "true" if hparams.get("sum_head", True) else "false",
    }


def convert(ckpt, out_dir):
    data = Path(ckpt).read_bytes()
    checkpoint = torch.load(ckpt, map_location="cpu", weights_only=True)
    tensors = state_to_tensors(checkpoint["state_dict"])
    meta = model_metadata(checkpoint.get("hyper_parameters", {}))
    meta["source"] = Path(ckpt).name
    meta["source_sha256"] = hashlib.sha256(data).hexdigest()
    name = Path(ckpt).stem.removeprefix("beat_this-")
    out = Path(out_dir or Path(ckpt).parent) / f"beat_this-{name}.safetensors"
    write_safetensors(out, tensors, meta)
    count = sum(a.size for a in tensors.values())
    print(f"{ckpt} -> {out}: {len(tensors)} tensors, {count} values, {out.stat().st_size} bytes")
    return out


def main(argv):
    out_dir = None
    if "--out" in argv:
        i = argv.index("--out")
        out_dir = argv[i + 1]
        del argv[i : i + 2]
        Path(out_dir).mkdir(parents=True, exist_ok=True)
    ckpts = [Path(a) for a in argv] or DEFAULT
    missing = [c for c in ckpts if not c.exists()]
    if missing:
        print("missing:", *missing, sep="\n  ")
        print("Run `beat_this` once to download the checkpoints, or name them as arguments.")
        return 1
    for c in ckpts:
        convert(c, out_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
