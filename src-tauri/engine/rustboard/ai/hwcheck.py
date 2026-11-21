"""One-time speed check: which device and Mode keep AI voices smooth on this PC.

Runs the real streaming pipeline offline (no audio devices) on a voice and
measures time per block. A Mode is 'comfortable' when a block takes well
under its own length; past ~90% the audio starts to crackle.
"""
from __future__ import annotations

import os
import time
from dataclasses import dataclass

import numpy as np

# Mode name -> (block seconds, pitch tracker, context seconds). Shared with the UI.
MODES = {
    "Fast": (0.08, "fcpe", 1.0),
    "Balanced": (0.12, "rmvpe", 1.5),
    "Crisp": (0.20, "rmvpe", 2.5),
    "Laptop": (0.30, "fcpe", 0.6),    # big chunks + light pitch tracker: for CPUs
}
# (mode, max load to accept it), best first. GPUs try the nicer modes, CPUs go straight to Laptop.
GPU_ORDER = (("Balanced", 0.6), ("Fast", 0.7), ("Laptop", 0.85))
CPU_ORDER = (("Laptop", 0.85), ("Fast", 0.7))


@dataclass
class Result:
    device: str            # "cuda" or "cpu"
    device_name: str
    mode: str              # recommended Mode
    load: float            # its measured load (block time / block length)
    ok: bool               # False: even the lightest mode can't keep up
    measured: dict         # mode -> load


def _device():
    import torch
    if torch.cuda.is_available():
        return torch.device("cuda:0"), torch.cuda.get_device_name(0)
    import platform
    torch.set_num_threads(max(1, (os.cpu_count() or 2) // 2))  # physical cores, roughly
    return torch.device("cpu"), platform.processor() or "CPU"


def measure_mode(pth, shared, mode: str, blocks: int = 8, progress=lambda s: None) -> float:
    from .engine import AIVoiceEngine, StreamSettings
    from .rvc import RVCVoice, VoiceSettings
    from .rvc_lib.cuda_graph import graphs_disabled
    block, f0, context = MODES[mode]
    progress(f"{mode}: loading the {f0} pitch tracker …")
    shared.load_rmvpe() if f0 == "rmvpe" else shared.load_fcpe()
    progress(f"{mode}: loading the voice …")
    voice = RVCVoice(pth, shared, VoiceSettings(f0_method=f0))
    eng = AIVoiceEngine(voice, 0, 0, StreamSettings(block_time=block, context_time=context, gate_db=-120))
    t = np.arange(eng.block_frame) / eng.sr
    sig = (0.2 * np.sin(2 * np.pi * 160 * t) * (1 + 0.5 * np.sin(2 * np.pi * 3 * t))).astype(np.float32)
    out = np.zeros((eng.block_frame, 1), np.float32)
    times = []
    with graphs_disabled():  # conservative: live mode adds CUDA graphs on top and is a bit faster
        for i in range(blocks + 2):
            progress(f"{mode}: block {i + 1}/{blocks + 2}")
            t0 = time.perf_counter()
            eng._process(sig, out)
            if shared.device.type == "cuda":
                import torch
                torch.cuda.synchronize()
            if i >= 2:  # first blocks warm up kernels/caches
                times.append(time.perf_counter() - t0)
    return float(np.median(times)) / block


def check(voice_name: str, progress=lambda s: None) -> Result:
    from . import assets
    from .rvc import SharedModels
    if assets.missing():
        progress("Downloading the AI base models (once, ~550 MB) …")
        assets.download()
    progress("Starting PyTorch …")
    dev, name = _device()
    progress(f"Loading the speech model on {name} …")
    shared = SharedModels(dev, dev.type == "cuda")
    pth = assets.find_voice(voice_name)[0]

    def measure(mode):
        progress(f"Testing {mode} mode on {name} …")
        return measure_mode(pth, shared, mode, progress=progress)

    mode, load, ok, measured = pick(GPU_ORDER if dev.type == "cuda" else CPU_ORDER, measure)
    return Result(dev.type, name, mode, load, ok, measured)


def pick(order, measure) -> tuple[str, float, bool, dict]:
    """Try modes best-first; take the first under its load limit, else the fastest one found."""
    measured = {}
    for mode, max_load in order:
        measured[mode] = load = measure(mode)
        if load <= max_load:
            return mode, load, True, measured
    best = min(measured, key=measured.get)
    return best, measured[best], measured[best] < 0.95, measured
