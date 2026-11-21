"""Streaming AI voice conversion: mic -> RVC -> (optional effects) -> virtual cable.

Same streaming scheme as RVC's realtime GUI (MIT): every block, the newest
audio is appended to a rolling context window, the whole window goes through
HuBERT (context = better pronunciation), but the voice network only renders
the newest block plus a little overlap. SOLA (from DDSP-SVC) then finds the
best-aligned splice point and crossfades, which is what keeps it crisp instead
of clicky.

Latency  ~= block + crossfade + device buffers.  On an RTX 4060 Ti the
defaults (120 ms blocks, fp16, CUDA graphs) leave plenty of headroom.
"""
from __future__ import annotations

import time
from dataclasses import dataclass

import numpy as np
import sounddevice as sd
import torch
import torch.nn.functional as F
import torchaudio.transforms as tat

from .. import effects as fx
from ..devices import wasapi_settings
from ..presets import build_chain
from .rvc import RVCVoice
from .rvc_lib.cuda_graph import configure_cuda_graph, run_cuda_graph


@dataclass
class StreamSettings:
    block_time: float = 0.12      # s. Lower = less delay, needs inference < block_time
    crossfade_time: float = 0.05  # s
    context_time: float = 1.5     # s of past audio HuBERT sees each block
    gate_db: float = -50.0        # silence below this before conversion (no AI 'breathing')
    rms_mix: float = 0.0          # 0 = follow your mic's loudness envelope, 1 = model's own
    post_preset: str | None = None  # e.g. "radio" to add effects after the AI voice


class AIVoiceEngine:
    def __init__(self, voice: RVCVoice, input_device: int, output_device: int,
                 settings: StreamSettings):
        self.voice = voice
        self.st = settings
        self.input_device, self.output_device = input_device, output_device
        self.device = voice.device
        self.sr = voice.tgt_sr  # run at the model's rate: no extra resample on the output
        self.muted = False
        self.bypass = False
        self.last_error: str | None = None
        self.monitor = None  # optional Monitor: hear yourself in headphones
        self.soundboard = None  # optional soundboard Mixer
        self.stream: sd.Stream | None = None
        self.infer_ms = 0.0
        self.load = 0.0
        self.xruns = 0
        self.peak_out = 0.0
        self.peak_in = 0.0
        configure_cuda_graph(self.device)
        self._build()

    # ---- buffers ----------------------------------------------------------
    def _frames(self, seconds: float) -> int:
        return int(round(seconds * self.sr / self.zc)) * self.zc

    def _build(self):
        dev = self.device
        self.zc = self.sr // 100  # 10 ms
        self.block_frame = self._frames(self.st.block_time)
        self.block_frame_16k = 160 * self.block_frame // self.zc
        self.crossfade_frame = self._frames(self.st.crossfade_time)
        self.sola_buffer_frame = min(self.crossfade_frame, 4 * self.zc)
        self.sola_search_frame = self.zc
        self.extra_frame = self._frames(self.st.context_time)
        total = self.extra_frame + self.crossfade_frame + self.sola_search_frame + self.block_frame
        self.input_wav = torch.zeros(total, device=dev, dtype=torch.float32)
        self.input_wav_res = torch.zeros(160 * total // self.zc, device=dev, dtype=torch.float32)
        self.sola_buffer = torch.zeros(self.sola_buffer_frame, device=dev, dtype=torch.float32)
        self.sola_den_kernel = torch.ones(1, 1, self.sola_buffer_frame, device=dev, dtype=torch.float32)
        self.skip_head = self.extra_frame // self.zc
        self.return_length = (self.block_frame + self.sola_buffer_frame + self.sola_search_frame) // self.zc
        self.fade_in = torch.sin(0.5 * np.pi * torch.linspace(
            0.0, 1.0, self.sola_buffer_frame, device=dev, dtype=torch.float32)) ** 2
        self.fade_out = 1 - self.fade_in
        self.resampler = tat.Resample(self.sr, 16000, dtype=torch.float32).to(dev)
        self.gate = fx.NoiseGate(self.sr, threshold_db=self.st.gate_db) if self.st.gate_db > -120 else None
        self.post = build_chain(self.st.post_preset, self.sr, gate_db=None) if self.st.post_preset else None

    def _reset(self):
        for t in (self.input_wav, self.input_wav_res, self.sola_buffer):
            t.zero_()
        self.voice.reset_state()

    def prewarm(self):
        """Run a few blocks of silence-ish audio so CUDA graphs/kernels are compiled before going live."""
        probe = 0.05 * np.sin(2 * np.pi * 220 * np.arange(self.block_frame) / self.sr).astype(np.float32)
        out = np.zeros((self.block_frame, 1), np.float32)
        try:
            for _ in range(3):
                self._process(probe, out)
        except Exception as e:
            # A CUDA-graph capture/replay can fail on tight VRAM. Rather than let
            # going live crash, turn graphs off for the session and warm up eager.
            from .rvc_lib.cuda_graph import disable_cuda_graph
            disable_cuda_graph(f"prewarm failed: {e!r}")
            if self.device.type == "cuda":
                try:
                    torch.cuda.synchronize(self.device)
                except Exception:
                    pass
            self._reset()
            for _ in range(3):
                self._process(probe, out)  # graphs are off now: plain eager
        if self.device.type == "cuda":
            torch.cuda.synchronize(self.device)
        self._reset()

    # ---- control ---------------------------------------------------------
    def switch_voice(self, voice: RVCVoice):
        """Swap voice. The stream pauses for a moment so the new voice can be
        warmed up (CUDA graph capture) without racing the audio thread."""
        running = self.stream is not None
        self.stop()
        self.voice = voice
        if voice.tgt_sr != self.sr:
            self.sr = voice.tgt_sr
            self._build()
        self._reset()
        if running:
            self.start()

    def set_gate(self, gate_db: float):
        """-120 turns the gate off. Safe while live (single attribute swap)."""
        self.st.gate_db = gate_db
        self.gate = fx.NoiseGate(self.sr, threshold_db=gate_db) if gate_db > -120 else None

    def set_post(self, preset: str | None):
        """Change the effects applied after the AI voice. Safe while live:
        the chain is built here and swapped in with one attribute assignment."""
        self.st.post_preset = preset if preset and preset != "clean" else None
        self.post = build_chain(self.st.post_preset, self.sr, gate_db=None) if self.st.post_preset else None

    # ---- per-block processing ----------------------------------------------
    def _rms(self, x: torch.Tensor, n: int) -> torch.Tensor:
        frame, hop = 4 * self.zc, self.zc
        padded = F.pad(x[None, None], (frame // 2, frame // 2))[0, 0]
        r = torch.sqrt((padded.unfold(0, frame, hop) ** 2).mean(-1))
        return F.interpolate(r[None, None], size=n + 1, mode="linear", align_corners=True)[0, 0, :-1]

    def _process(self, mono: np.ndarray, outdata: np.ndarray):
        self.peak_in = float(np.max(np.abs(mono))) if len(mono) else 0.0
        gate = self.gate  # read once: the UI thread may swap it
        if gate is not None:  # gate in 10 ms steps so word onsets open instantly
            zc = self.zc
            x = np.concatenate([gate.process(mono[i:i + zc]) for i in range(0, len(mono), zc)])
        else:
            x = mono
        n = len(x)

        self.input_wav[:-n] = self.input_wav[n:].clone()
        self.input_wav[-n:] = torch.from_numpy(x).to(self.device)
        self.input_wav_res[:-self.block_frame_16k] = self.input_wav_res[self.block_frame_16k:].clone()
        res_in = self.input_wav[-n - 2 * self.zc:]
        self.input_wav_res[-160 * (n // self.zc + 1):] = run_cuda_graph(
            self.resampler, "in-resample", lambda a: self.resampler(a), res_in)[160:]

        if self.bypass:
            wav = self.input_wav[self.extra_frame:].clone()
        else:
            wav = self.voice.infer(self.input_wav_res, self.block_frame_16k,
                                   self.skip_head, self.return_length)
            if self.st.rms_mix < 1:
                src = self.input_wav[self.extra_frame:][: wav.shape[0]]
                r1 = self._rms(src, wav.shape[0])
                r2 = torch.clamp(self._rms(wav, wav.shape[0]), min=1e-3)
                wav = wav * torch.pow(r1 / r2, 1.0 - self.st.rms_mix)

        # SOLA: find the offset where the new chunk best matches the tail of the last one
        conv_in = wav[None, None, : self.sola_buffer_frame + self.sola_search_frame]
        nom = F.conv1d(conv_in, self.sola_buffer[None, None, :])
        den = torch.sqrt(F.conv1d(conv_in ** 2, self.sola_den_kernel) + 1e-8)
        off = int(torch.argmax(nom[0, 0] / den[0, 0]))
        wav = wav[off:]
        wav[: self.sola_buffer_frame] *= self.fade_in
        wav[: self.sola_buffer_frame] += self.sola_buffer * self.fade_out
        self.sola_buffer[:] = wav[self.block_frame: self.block_frame + self.sola_buffer_frame]

        y = wav[: self.block_frame].cpu().numpy().astype(np.float32)
        post = self.post  # read once: the UI thread may swap it
        if post is not None:
            y = post.process(y)
        if self.muted:
            y = np.zeros_like(y)
        sb = self.soundboard
        if sb is not None:
            clips = sb.mix(len(y))
            if clips is not None:
                y = y + clips
        np.clip(y, -1.0, 1.0, out=y)
        self.peak_out = float(np.max(np.abs(y)))
        outdata[:] = y[:, None]
        mon = self.monitor
        if mon is not None:
            mon.push(y)

    def _callback(self, indata, outdata, frames, _t, status):
        t0 = time.perf_counter()
        if status:
            self.xruns += 1
        try:
            self._process(indata[:, 0].copy(), outdata)
        except Exception as e:  # never let an exception kill the stream silently
            outdata.fill(0)
            self.last_error = repr(e)
        dt = time.perf_counter() - t0
        self.infer_ms = dt * 1000
        self.load = dt / self.st.block_time

    # ---- lifecycle ---------------------------------------------------------
    def start(self):
        self.prewarm()
        out_ch = min(2, sd.query_devices(self.output_device)["max_output_channels"])
        extra = (wasapi_settings(self.input_device), wasapi_settings(self.output_device))
        self.stream = sd.Stream(
            samplerate=self.sr, blocksize=self.block_frame,
            device=(self.input_device, self.output_device), channels=(1, out_ch),
            dtype="float32", latency="low", callback=self._callback,
            extra_settings=None if extra == (None, None) else extra,
        )
        self.stream.start()

    @property
    def latency_ms(self) -> float:
        dev = sum(self.stream.latency) if self.stream else 0.0
        return (dev + self.st.block_time + min(self.st.crossfade_time, 0.04) + 0.01) * 1000

    def stop(self):
        if self.stream:
            self.stream.abort()
            self.stream.close()
            self.stream = None
