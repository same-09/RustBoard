"""Try a voice before installing it: convert a short recording of you with it.

The clip runs through the exact same streaming engine as live mode (block by
block, no audio devices), so the preview sounds like what Discord would get.
"""
from __future__ import annotations

import shutil
import zipfile
from pathlib import Path, PurePosixPath

import numpy as np

from . import assets

DEMO_SR = 48000
DEMO_SECONDS = 5


def demo_path() -> Path:
    return assets.home() / "demo_input.wav"


def _resample(x: np.ndarray, src: int, dst: int) -> np.ndarray:
    if src == dst:
        return x.astype(np.float32)
    from scipy.signal import resample_poly
    g = np.gcd(src, dst)
    return resample_poly(x, dst // g, src // g).astype(np.float32)


def _native_rate(device: int | None, kind: str) -> int:
    """The rate the device actually runs at; opening it at anything else can fail on Windows."""
    import sounddevice as sd
    return int(sd.query_devices(device, kind)["default_samplerate"])


def record_demo(device: int | None, seconds: float = DEMO_SECONDS) -> np.ndarray:
    import sounddevice as sd
    from scipy.io import wavfile

    rate = _native_rate(device, "input")
    x = sd.rec(int(seconds * rate), samplerate=rate, channels=1, dtype="float32", device=device)
    sd.wait()
    x = _resample(x[:, 0], rate, DEMO_SR)
    peak = float(np.max(np.abs(x))) or 1.0
    x = x * min(0.9 / peak, 8.0)  # normalise, but don't blow up silence
    demo_path().parent.mkdir(parents=True, exist_ok=True)
    wavfile.write(demo_path(), DEMO_SR, x.astype(np.float32))
    return x


def play(y: np.ndarray, sr: int, device: int | None) -> None:
    import sounddevice as sd
    rate = _native_rate(device, "output")
    sd.stop()
    sd.play(_resample(y, sr, rate), rate, device=device)


def load_demo() -> np.ndarray | None:
    from scipy.io import wavfile
    if not demo_path().is_file():
        return None
    _, x = wavfile.read(demo_path())
    return x.astype(np.float32)


def fetch_pth(repo_id: str, path: str) -> Path:
    """Download just the model (cached by Hugging Face). Zips are unpacked to a preview cache."""
    from huggingface_hub import hf_hub_download

    from .catalog import _is_base_model, safe_name
    local = Path(hf_hub_download(repo_id, path))
    if not path.lower().endswith(".zip"):
        return local
    out = assets.home() / "preview" / safe_name(f"{repo_id}-{PurePosixPath(path).stem}")
    out.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(local) as z:
        pths = [m for m in z.infolist() if m.filename.lower().endswith(".pth")
                and not _is_base_model(m.filename.lower())]
        if not pths:
            raise ValueError("no .pth voice model inside this zip")
        m = max(pths, key=lambda m: m.file_size)
        target = out / "model.pth"
        with z.open(m) as src, open(target, "wb") as dst:
            shutil.copyfileobj(src, dst)
    return target


class Previewer:
    """Holds its own copy of the shared models so previews never touch the live engine's."""

    def __init__(self):
        import threading
        self.shared = None
        self._lock = threading.Lock()   # two windows may preview at once

    def convert(self, pth: Path, clip: np.ndarray, pitch: float) -> tuple[np.ndarray, int]:
        from .rvc_lib.cuda_graph import graphs_disabled
        # no CUDA graph capture here: capturing while the live engine uses the GPU
        # from another thread is what PyTorch's allocator refuses
        with self._lock, graphs_disabled():
            return self._convert(pth, clip, pitch)

    def _convert(self, pth: Path, clip: np.ndarray, pitch: float) -> tuple[np.ndarray, int]:
        import torch
        from scipy.signal import resample_poly

        from .engine import AIVoiceEngine, StreamSettings
        from .rvc import RVCVoice, SharedModels, VoiceSettings

        if assets.missing():
            assets.download()
        if self.shared is None:
            dev = torch.device("cuda:0" if torch.cuda.is_available() else "cpu")
            self.shared = SharedModels(dev, dev.type == "cuda")
        voice = RVCVoice(pth, self.shared, VoiceSettings(pitch=pitch, f0_method="rmvpe"))
        eng = AIVoiceEngine(voice, 0, 0, StreamSettings(block_time=0.2, context_time=1.5, gate_db=-120))
        g = np.gcd(DEMO_SR, eng.sr)
        x = resample_poly(clip, eng.sr // g, DEMO_SR // g).astype(np.float32)
        x = np.concatenate([x, np.zeros(int(0.4 * eng.sr), np.float32)])  # flush the pipeline
        n = eng.block_frame
        out = np.zeros((n, 1), np.float32)
        chunks = []
        for i in range(0, len(x) - n + 1, n):
            eng._process(x[i:i + n], out)
            chunks.append(out[:, 0].copy())
        return np.concatenate(chunks), eng.sr  # (no empty_cache: unsafe while another thread captures)


_shared: Previewer | None = None


def get_previewer() -> Previewer:
    """One Previewer for the whole app, so the AI models load once for all preview buttons."""
    global _shared
    if _shared is None:
        _shared = Previewer()
    return _shared
