"""Find and install RVC voices: Hugging Face search, repo inspection, downloads, local files.

Everything here is blocking network/disk work: call it from a worker thread.
Downloaded models are only ever loaded with torch.load(weights_only=True), and
zip archives are unpacked by file name only (never by the paths stored inside),
so a malicious download can't run code or write outside the voice folder.
"""
from __future__ import annotations

import re
import shutil
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from . import assets

MAX_ZIP_BYTES = 3 * 1024 ** 3  # refuse absurd archives


@dataclass
class RepoHit:
    repo_id: str
    license: str
    downloads: int
    likes: int
    updated: str

    @property
    def url(self) -> str:
        return f"https://huggingface.co/{self.repo_id}"


@dataclass
class VoiceFile:
    """One installable voice inside a repo: a .pth (+ best .index), or a .zip bundle."""
    path: str
    index: str | None
    size: int

    @property
    def label(self) -> str:
        kind = "zip" if self.path.lower().endswith(".zip") else ("pth + index" if self.index else "pth")
        return f"{self.path}  ({kind}, {self.size / 1e6:.0f} MB)"

    @property
    def default_name(self) -> str:
        stem = PurePosixPath(self.path).stem
        parent = PurePosixPath(self.path).parent.name
        if parent and parent.lower() not in ("rmvpe", "weights", "models", "."):
            stem = f"{parent} {stem}" if stem.lower() not in parent.lower() else parent
        return safe_name(stem)


def safe_name(name: str) -> str:
    name = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "", name).strip(" .")
    return name[:60] or "voice"


# ---------------------------------------------------------------- search
def search(query: str, limit: int = 40) -> list[RepoHit]:
    from .. import net
    net.require_online("Searching voices")
    from huggingface_hub import HfApi

    api = HfApi()
    seen, hits = set(), []

    def add(models):
        for m in models:
            if m.id in seen:
                continue
            seen.add(m.id)
            tags = getattr(m, "tags", None) or []
            lic = next((t.split(":", 1)[1] for t in tags if t.startswith("license:")), "?")
            upd = getattr(m, "last_modified", None) or getattr(m, "lastModified", None)
            hits.append(RepoHit(m.id, lic, getattr(m, "downloads", 0) or 0,
                                getattr(m, "likes", 0) or 0, str(upd)[:10] if upd else ""))

    q = query.strip() or None
    add(api.list_models(search=q, filter="rvc", sort="downloads", limit=limit))
    if q and len(hits) < 10:  # many voice repos aren't tagged: widen the search
        add(api.list_models(search=q, sort="downloads", limit=limit))
    return hits


def repo_voices(repo_id: str) -> list[VoiceFile]:
    """List installable voices in a repo, pairing each .pth with its best .index."""
    from .. import net
    net.require_online("Listing a voice's files")
    from huggingface_hub import HfApi

    info = HfApi().model_info(repo_id, files_metadata=True)
    files = {s.rfilename: (s.size or 0) for s in info.siblings}
    indexes = [f for f in files if f.lower().endswith(".index")]
    out = []
    for f, size in files.items():
        low = f.lower()
        if low.endswith(".zip") and size < MAX_ZIP_BYTES:
            out.append(VoiceFile(f, None, size))
        elif low.endswith(".pth") and not _is_base_model(low):
            out.append(VoiceFile(f, _match_index(f, indexes), size))
    return sorted(out, key=lambda v: v.path.lower())


def _is_base_model(low: str) -> bool:
    # training checkpoints / shared models that are not voices
    name = PurePosixPath(low).name
    return name.startswith(("g_", "d_", "f0g", "f0d")) or name in ("rmvpe.pt", "hubert_base.pt")


def _match_index(pth: str, indexes: list[str]) -> str | None:
    p = PurePosixPath(pth)
    stem = p.stem.lower()
    def prefer_added(group):
        added = [i for i in group if "added" in PurePosixPath(i).name.lower()]
        return (added or group)[0]

    named = [i for i in indexes if stem in PurePosixPath(i).name.lower()]
    if named:
        return prefer_added(named)
    same_dir = [i for i in indexes if PurePosixPath(i).parent == p.parent]
    added_here = [i for i in same_dir if "added" in PurePosixPath(i).name.lower()]
    # without a name match, only pair when the folder makes it unambiguous
    if len(added_here) == 1:
        return added_here[0]
    if len(same_dir) == 1:
        return same_dir[0]
    return None


# ---------------------------------------------------------------- install
def install_from_repo(repo_id: str, voice: VoiceFile, name: str, with_index: bool = True) -> Path:
    from huggingface_hub import hf_hub_download

    dest = _new_voice_dir(name)
    try:
        local = Path(hf_hub_download(repo_id, voice.path))
        if voice.path.lower().endswith(".zip"):
            _extract_zip(local, dest)
        else:
            shutil.copy2(local, dest / PurePosixPath(voice.path).name)
            if with_index and voice.index:
                shutil.copy2(Path(hf_hub_download(repo_id, voice.index)), dest / PurePosixPath(voice.index).name)
        _check(dest)
        return dest
    except Exception:
        shutil.rmtree(dest, ignore_errors=True)
        raise


def install_from_file(path: Path, name: str) -> Path:
    """A .pth (an .index next to it is picked up too) or a .zip bundle."""
    dest = _new_voice_dir(name)
    try:
        if path.suffix.lower() == ".zip":
            _extract_zip(path, dest)
        elif path.suffix.lower() == ".pth":
            shutil.copy2(path, dest / path.name)
            near = sorted(path.parent.glob("added*.index")) or sorted(path.parent.glob("*.index"))
            if len(near) >= 1 and (len(near) == 1 or any(path.stem in n.name for n in near)):
                pick = next((n for n in near if path.stem in n.name), near[0])
                shutil.copy2(pick, dest / pick.name)
        else:
            raise ValueError("pick a .pth or .zip file")
        _check(dest)
        return dest
    except Exception:
        shutil.rmtree(dest, ignore_errors=True)
        raise


def _new_voice_dir(name: str) -> Path:
    dest = assets.voices_dir() / safe_name(name)
    if dest.exists() and any(dest.iterdir()):
        raise FileExistsError(f"a voice called '{dest.name}' already exists")
    dest.mkdir(parents=True, exist_ok=True)
    return dest


def _extract_zip(zpath: Path, dest: Path) -> None:
    with zipfile.ZipFile(zpath) as z:
        members = [m for m in z.infolist() if not m.is_dir()
                   and m.filename.lower().endswith((".pth", ".index"))
                   and not _is_base_model(m.filename.lower())]
        if sum(m.file_size for m in members) > MAX_ZIP_BYTES:
            raise ValueError("archive is unreasonably large")
        pths = [m for m in members if m.filename.lower().endswith(".pth")]
        if not pths:
            raise ValueError("no .pth voice model inside this zip")
        for m in members:
            # basename only: entries like '../../x' can't escape the voice folder
            target = dest / safe_name(PurePosixPath(m.filename.replace("\\", "/")).name)
            with z.open(m) as src, open(target, "wb") as out:
                shutil.copyfileobj(src, out)


def _check(dest: Path) -> None:
    """Make sure it's really an RVC voice before it shows up in the list."""
    pths = list(dest.glob("*.pth"))
    if not pths:
        raise ValueError("no .pth file was installed")
    try:
        import torch
    except ImportError:
        return  # effects-only install: can't verify, the AI loader will
    for p in pths:
        cpt = torch.load(str(p), map_location="cpu", weights_only=True)
        if not (isinstance(cpt, dict) and "weight" in cpt and "config" in cpt):
            raise ValueError(f"{p.name} isn't an RVC voice model (it may be a training checkpoint)")
    if len(pths) > 1:  # keep the biggest-looking one, voices use one model per folder
        keep = max(pths, key=lambda q: q.stat().st_size)
        for p in pths:
            if p != keep:
                p.unlink()
