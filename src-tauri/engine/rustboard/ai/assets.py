"""Shared model files every RVC voice needs (downloaded once).

  hubert_base/   ContentVec/HuBERT speech-content encoder (Transformers format)
  rmvpe/rmvpe.pt pitch tracker (best quality)
  FCPE (faster pitch tracker) ships inside the torchfcpe pip package.

Voices themselves (.pth + optional .index) go in <home>/voices/<name>/.
"""
from __future__ import annotations

import os
from pathlib import Path

HF_REPO = "lj1995/VoiceConversionWebUI"
HUBERT_FILES = ["hubert_base/config.json", "hubert_base/preprocessor_config.json",
                "hubert_base/pytorch_model.bin"]


def home() -> Path:
    """Where models, voices and clips live. Folders from the app's old name are reused."""
    env = os.environ.get("RUSTBOARD_HOME") or os.environ.get("VOICECHANGER_HOME")
    if env:
        return Path(env)
    old = Path.home() / ".voicechanger"
    return old if old.is_dir() else Path.home() / ".rustboard"


def hubert_dir() -> Path:
    return home() / "assets" / "hubert_base"


def rmvpe_path() -> Path:
    return home() / "assets" / "rmvpe" / "rmvpe.pt"


def voices_dir() -> Path:
    return home() / "voices"


def missing() -> list[str]:
    need = [hubert_dir() / Path(f).name for f in HUBERT_FILES] + [rmvpe_path()]
    return [str(p) for p in need if not p.is_file()]


def download() -> None:
    from huggingface_hub import hf_hub_download

    assets = home() / "assets"
    for f in HUBERT_FILES:
        print(f"downloading {f} ...")
        hf_hub_download(HF_REPO, f, local_dir=assets)
    print("downloading rmvpe.pt ...")
    hf_hub_download(HF_REPO, "rmvpe.pt", local_dir=assets / "rmvpe")
    voices_dir().mkdir(parents=True, exist_ok=True)
    print(f"\nDone. Put voices in {voices_dir() / '<name>'} (a .pth and optionally an added_*.index)")


def find_voice(name_or_path: str) -> tuple[Path, Path | None]:
    """Accepts a .pth path, a voice folder, or a folder name inside voices_dir()."""
    p = Path(name_or_path)
    if p.suffix.lower() == ".pth" and p.is_file():
        folder, pth = p.parent, p
    else:
        folder = p if p.is_dir() else voices_dir() / name_or_path
        pths = sorted(folder.glob("*.pth")) if folder.is_dir() else []
        if not pths:
            raise SystemExit(f"no .pth voice found for {name_or_path!r} (looked in {folder})")
        pth = pths[0]
    # Only 'added_*.index' works for retrieval; 'trained_*.index' is a training by-product.
    idx = sorted(folder.glob("added*.index")) or sorted(folder.glob("*.index"))
    return pth, (idx[0] if idx else None)


def list_voices() -> list[str]:
    d = voices_dir()
    if not d.is_dir():
        return []
    return sorted(v.name for v in d.iterdir() if v.is_dir() and any(v.glob("*.pth")))


# ---- starter voices --------------------------------------------------------
# RVC v2 models (RMVPE-trained, with .index) of speakers from the VCTK corpus
# (CC BY 4.0, recorded with the speakers' consent). Repo license: Apache-2.0.
# https://huggingface.co/Nekochu/RVC-VCTK_Voice-sample  - each speaker folder
# also has a .flac sample you can listen to on the website first.
STARTER_REPO = "Nekochu/RVC-VCTK_Voice-sample"
# key: (folder in the repo, installed folder name, download the .index?)
# The blends were trained on all speakers of one gender at once, so they don't
# sound like any single person. Their .index files are 0.5-1.3 GB and are
# skipped by default (they only feed the Similarity slider).
STARTER = {
    "p226": ("M/p226/rmvpe", "Male - English (p226)", True),
    "p274": ("M/p274", "Male 2 (p274)", True),
    "p311": ("M/p311", "Male 3 (p311)", True),
    "all_m": ("M/ALL_M-harvest CKPT", "Male - Blend of all", False),
    "p231": ("F/p231/rmvpe", "Female - English (p231)", True),
    "p238": ("F/p238/rmvpe", "Female - Northern Irish (p238)", True),
    "p249": ("F/p249/rmvpe", "Female - Scottish (p249)", True),
    "p262": ("F/p262/rmvpe", "Female - Scottish 2 (p262)", True),
    "p280": ("F/p280/rmvpe", "Female - Neutral (p280)", True),
    "p323": ("F/p323/rmvpe", "Female - South African (p323)", True),
    "p340": ("F/p340/rmvpe", "Female - Irish (p340)", True),
    "all_f": ("F/All_F/rmvpe", "Female - Blend of all", False),
}
STARTER_VOICES = {k: ("F" if v[1].startswith("Female") else "M") for k, v in STARTER.items()}


def starter_installed(key: str) -> bool:
    names = set(list_voices())
    return STARTER[key][1] in names or f"vctk-{key}" in names


def starter_files(key: str, with_index: bool | None = None) -> list[str]:
    """Repo paths of a starter voice's .pth (+ .index)."""
    from .. import net
    net.require_online("Getting starter voices")
    from huggingface_hub import list_repo_files

    if key not in STARTER:
        raise SystemExit(f"unknown voice {key!r}; choose from: {', '.join(STARTER)}")
    sub, _, index_default = STARTER[key]
    want_index = index_default if with_index is None else with_index
    files = [f for f in list_repo_files(STARTER_REPO) if f.startswith(sub + "/")]
    # some speaker folders keep the rmvpe-trained model in a subfolder: prefer it
    rmvpe = [f for f in files if f.startswith(sub + "/rmvpe/")]
    files = rmvpe or [f for f in files if f.count("/") == sub.count("/") + 1]
    pick = [f for f in files if f.endswith(".pth") or (want_index and f.endswith(".index"))]
    if not any(f.endswith(".pth") for f in pick):
        raise SystemExit(f"no .pth found under {sub}")
    return pick


def get_starter_voice(key: str, with_index: bool | None = None) -> Path:
    """Download one starter voice into voices_dir()/<friendly name>.
    Uses the Hugging Face cache, so a voice you previewed isn't downloaded twice."""
    import shutil

    from huggingface_hub import hf_hub_download

    pick = starter_files(key, with_index)
    dest = voices_dir() / STARTER[key][1]
    dest.mkdir(parents=True, exist_ok=True)
    for f in pick:
        shutil.copy2(hf_hub_download(STARTER_REPO, f), dest / Path(f).name)
    return dest
