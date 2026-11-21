"""One RVC voice, ready for streaming inference.

Adapted from RVC's infer/rtrvc.py (MIT). Differences: no GUI/i18n, faiss is
optional, the heavy shared models (HuBERT, RMVPE, FCPE) live in a SharedModels
object so switching voices only reloads the small voice network.
"""
from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F
from torchaudio.transforms import Resample

from . import _compat  # noqa: F401  (must run before transformers is imported)
from . import assets
from .rvc_lib import hubert as hubert_mod
from .rvc_lib.cuda_graph import run_cuda_graph

F0_MIN, F0_MAX = 50.0, 1100.0
F0_MEL_MIN = 1127 * np.log(1 + F0_MIN / 700)
F0_MEL_MAX = 1127 * np.log(1 + F0_MAX / 700)


@dataclass
class SharedModels:
    device: torch.device
    is_half: bool
    hubert: object = None
    rmvpe: object = None
    fcpe: object = None

    def load_hubert(self):
        if self.hubert is None:
            hubert_mod.HUBERT_MODEL_PATH = assets.hubert_dir()
            self.hubert = hubert_mod.load_hubert_model(self.device, self.is_half)
        return self.hubert

    def load_rmvpe(self):
        if self.rmvpe is None:
            from .rvc_lib.rmvpe import RMVPE
            self.rmvpe = RMVPE(str(assets.rmvpe_path()), is_half=self.is_half, device=self.device)
        return self.rmvpe

    def load_fcpe(self):
        if self.fcpe is None:
            from .rvc_lib.fcpe import FCPEInfer
            self.fcpe = FCPEInfer(self.device)
        return self.fcpe


def load_synthesizer(pth: Path, device):
    from .rvc_lib.models import (SynthesizerTrnMs256NSFsid, SynthesizerTrnMs256NSFsid_nono,
                                 SynthesizerTrnMs768NSFsid, SynthesizerTrnMs768NSFsid_nono)

    # weights_only=True (torch default) refuses pickled code inside .pth files.
    # Community voices are untrusted downloads, so never turn this off.
    cpt = torch.load(str(pth), map_location="cpu", weights_only=True)
    cpt["config"][-3] = cpt["weight"]["emb_g.weight"].shape[0]
    if_f0 = cpt.get("f0", 1)
    version = cpt.get("version", "v1")
    cls = {("v1", 1): SynthesizerTrnMs256NSFsid, ("v1", 0): SynthesizerTrnMs256NSFsid_nono,
           ("v2", 1): SynthesizerTrnMs768NSFsid, ("v2", 0): SynthesizerTrnMs768NSFsid_nono}[(version, if_f0)]
    net_g = cls(*cpt["config"], is_half=False) if if_f0 else cls(*cpt["config"])
    del net_g.enc_q
    net_g.load_state_dict(cpt["weight"], strict=False)
    net_g = net_g.float().eval().to(device)
    net_g.remove_weight_norm()
    return net_g, cpt["config"][-1], if_f0, version


@dataclass
class VoiceSettings:
    pitch: float = 0.0          # semitones (+12 = male -> female range)
    formant: float = 0.0        # semitones of timbre shift (-2..+2 is plenty)
    index_rate: float = 0.0     # 0..1, how much to pull timbre toward the training set
    f0_method: str = "rmvpe"    # 'rmvpe' (cleanest) or 'fcpe' (fastest)
    extra: dict = field(default_factory=dict)


class RVCVoice:
    def __init__(self, pth: Path, shared: SharedModels, settings: VoiceSettings,
                 index_path: Path | None = None):
        self.name = pth.parent.name if pth.parent != assets.voices_dir() else pth.stem
        self.shared = shared
        self.device = shared.device
        self.is_half = shared.is_half
        self.s = settings
        self.net_g, self.tgt_sr, self.if_f0, self.version = load_synthesizer(pth, self.device)
        if self.is_half:
            self.net_g = self.net_g.half()
        shared.load_hubert()
        if self.if_f0:
            shared.load_rmvpe() if settings.f0_method == "rmvpe" else shared.load_fcpe()

        self.index = None
        if index_path is not None:
            try:
                import faiss
                self.index = faiss.read_index(str(index_path))
                self.big_npy = self.index.reconstruct_n(0, self.index.ntotal)
            except ImportError:
                print("faiss not installed: ignoring .index (pip install faiss-cpu)")
        self.cache_pitch = torch.zeros(1024, device=self.device, dtype=torch.long)
        self.cache_pitchf = torch.zeros(1024, device=self.device, dtype=torch.float32)
        self.resample_kernel: dict[int, Resample] = {}
        self.last_timings = (0.0, 0.0, 0.0, 0.0)
        self.sync_timing = False  # benchmark: wait for the GPU so per-stage times are real

    def _now(self):
        if self.sync_timing and self.device.type == "cuda":
            torch.cuda.synchronize(self.device)
        return time.perf_counter()

    def reset_state(self):
        self.cache_pitch.zero_()
        self.cache_pitchf.zero_()

    # ---- pitch ------------------------------------------------------------
    def _f0_post(self, f0):
        if not torch.is_tensor(f0):
            f0 = torch.from_numpy(f0)
        f0 = f0.float().to(self.device).squeeze()
        f0_mel = 1127 * torch.log(1 + f0 / 700)
        pos = f0_mel > 0
        f0_mel[pos] = (f0_mel[pos] - F0_MEL_MIN) * 254 / (F0_MEL_MAX - F0_MEL_MIN) + 1
        f0_mel[f0_mel <= 1] = 1
        f0_mel[f0_mel > 255] = 255
        return torch.round(f0_mel).long(), f0

    def _f0(self, x, key):
        if self.s.f0_method == "fcpe":
            f0 = self.shared.load_fcpe().infer(
                x.unsqueeze(0).float(), sr=16000, decoder_mode="local_argmax", threshold=0.006,
            ).squeeze().detach().cpu().numpy()
        else:
            f0 = self.shared.load_rmvpe().infer_from_audio(x, thred=0.03)
        uv = f0 == 0
        if np.any(~uv):  # fill unvoiced gaps so pitch glides instead of dropping out
            f0[uv] = np.interp(np.where(uv)[0], np.where(~uv)[0], f0[~uv])
        f0 *= pow(2, key / 12)
        return self._f0_post(f0)

    # ---- one streaming step ------------------------------------------------
    @torch.no_grad()
    def infer(self, input_wav_16k: torch.Tensor, block_frame_16k: int,
              skip_head: int, return_length: int) -> torch.Tensor:
        t1 = self._now()
        feats = (input_wav_16k.half() if self.is_half else input_wav_16k.float()).view(1, -1)
        feats = hubert_mod.extract_hubert_features(self.shared.hubert, feats, self.version)
        feats = torch.cat((feats, feats[:, -1:, :]), 1)
        t2 = self._now()

        if self.index is not None and self.s.index_rate > 0:
            npy = feats[0][skip_head // 2:].cpu().numpy().astype("float32")
            score, ix = self.index.search(npy, k=8)
            if (ix >= 0).all():
                weight = np.square(1 / score)
                weight /= weight.sum(axis=1, keepdims=True)
                npy = np.sum(self.big_npy[ix] * np.expand_dims(weight, axis=2), axis=1)
                if self.is_half:
                    npy = npy.astype("float16")
                feats[0][skip_head // 2:] = (
                    torch.from_numpy(npy).unsqueeze(0).to(self.device) * self.s.index_rate
                    + (1 - self.s.index_rate) * feats[0][skip_head // 2:])
        t3 = self._now()

        p_len = input_wav_16k.shape[0] // 160
        factor = pow(2, self.s.formant / 12)
        return_length2 = int(np.ceil(return_length * factor))
        if self.if_f0:
            f0_frames = block_frame_16k + 800
            if self.s.f0_method == "rmvpe":
                f0_frames = 5120 * ((f0_frames - 1) // 5120 + 1) - 160
            pitch, pitchf = self._f0(input_wav_16k[-f0_frames:], self.s.pitch - self.s.formant)
            shift = block_frame_16k // 160
            self.cache_pitch[:-shift] = self.cache_pitch[shift:].clone()
            self.cache_pitchf[:-shift] = self.cache_pitchf[shift:].clone()
            self.cache_pitch[4 - pitch.shape[0]:] = pitch[3:-1]
            self.cache_pitchf[4 - pitch.shape[0]:] = pitchf[3:-1]
            cache_pitch = self.cache_pitch[None, -p_len:]
            cache_pitchf = self.cache_pitchf[None, -p_len:] * return_length2 / return_length
        t4 = self._now()

        feats = F.interpolate(feats.permute(0, 2, 1), scale_factor=2).permute(0, 2, 1)[:, :p_len, :]
        p_len_t = torch.LongTensor([p_len]).to(self.device)
        sid = torch.LongTensor([0]).to(self.device)
        key = f"{skip_head}-{return_length}-{return_length2}"
        if self.if_f0:
            audio = run_cuda_graph(
                self.net_g, "rvc-f0-" + key,
                lambda ph, ln, c, cf, sp: self.net_g.infer(ph, ln, c, cf, sp, skip_head,
                                                           return_length, return_length2)[0],
                feats, p_len_t, cache_pitch, cache_pitchf, sid)
        else:
            audio = run_cuda_graph(
                self.net_g, "rvc-nof0-" + key,
                lambda ph, ln, sp: self.net_g.infer(ph, ln, sp, skip_head,
                                                    return_length, return_length2)[0],
                feats, p_len_t, sid)
        audio = audio.squeeze(1).float()

        upp = int(np.floor(factor * self.tgt_sr // 100))
        if upp != self.tgt_sr // 100:  # formant shift: undo the time stretch
            if upp not in self.resample_kernel:
                self.resample_kernel[upp] = Resample(orig_freq=upp, new_freq=self.tgt_sr // 100,
                                                     dtype=torch.float32).to(self.device)
            audio = self.resample_kernel[upp](audio[:, : return_length * upp])
        t5 = self._now()
        self.last_timings = (t2 - t1, t3 - t2, t4 - t3, t5 - t4)
        return audio.squeeze()
