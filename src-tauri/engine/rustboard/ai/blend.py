"""Blend two RVC voices into a new one by mixing their weights.

Two voices can be blended when they share an architecture: same RVC version,
both with (or both without) pitch, same sample rate, and the same tensor
shapes. All the VCTK starter voices qualify. The result is a normal .pth in
the voices folder, so it costs nothing extra while live.

The retrieval .index isn't blended (it holds one speaker's recorded features),
so the Similarity slider does nothing for blends.
"""
from __future__ import annotations

from pathlib import Path

from . import assets


def _load(pth: Path) -> dict:
    import torch
    return torch.load(str(pth), map_location="cpu", weights_only=True)


def _sr(cpt) -> int:
    return int(cpt["config"][-1])


def check(a: dict, b: dict) -> str | None:
    """None if the two checkpoints can be blended, else the reason."""
    if a.get("version", "v1") != b.get("version", "v1"):
        return f"different RVC versions ({a.get('version', 'v1')} vs {b.get('version', 'v1')})"
    if a.get("f0", 1) != b.get("f0", 1):
        return "one voice uses pitch and the other doesn't"
    if _sr(a) != _sr(b):
        return f"different sample rates ({_sr(a) // 1000}k vs {_sr(b) // 1000}k)"
    wa, wb = a["weight"], b["weight"]
    if set(wa) != set(wb):
        return "the models have different layers"
    for k in wa:
        if k != "emb_g.weight" and tuple(wa[k].shape) != tuple(wb[k].shape):
            return f"layer {k} has a different size"
    return None


def blend(pth_a: Path, pth_b: Path, mix: float) -> dict:
    """mix = 0 -> all A, 1 -> all B."""
    import torch
    a, b = _load(pth_a), _load(pth_b)
    why = check(a, b)
    if why:
        raise ValueError(f"These two voices can't be blended: {why}.")
    out = {}
    for k, ta in a["weight"].items():
        tb = b["weight"][k]
        if k == "emb_g.weight":
            # speaker embedding: inference always uses speaker 0, so blend that row only
            ta, tb = ta[:1], tb[:1]
        out[k] = ((1 - mix) * ta.float() + mix * tb.float()).to(ta.dtype)
    config = list(a["config"])
    config[-3] = 1  # one speaker
    return {
        "weight": out, "config": config, "f0": a.get("f0", 1), "version": a.get("version", "v1"),
        "sr": a.get("sr", f"{_sr(a) // 1000}k"),
        "info": f"RustBoard blend: {round((1 - mix) * 100)}% {pth_a.parent.name} + "
                f"{round(mix * 100)}% {pth_b.parent.name}",
    }


def write(cpt: dict, path: Path) -> Path:
    import torch
    path.parent.mkdir(parents=True, exist_ok=True)
    torch.save(cpt, str(path))
    return path


def preview_path() -> Path:
    return assets.home() / "preview" / "blend.pth"


def save_as_voice(cpt: dict, name: str) -> Path:
    from .catalog import safe_name
    folder = assets.voices_dir() / safe_name(name)
    if folder.exists() and any(folder.glob("*.pth")):
        raise FileExistsError(f"a voice called '{folder.name}' already exists")
    return write(cpt, folder / f"{safe_name(name)}.pth")
