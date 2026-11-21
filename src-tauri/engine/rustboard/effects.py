"""DSP building blocks.

Every effect is *stateful* and processes mono float32 blocks of any length,
so the exact same code runs inside the live audio callback and in offline
rendering. Rules for anything that goes in here:

  * no allocation-heavy work, no I/O, no locks (it runs in the audio thread)
  * vectorise with numpy; never loop per sample in Python
  * keep state between calls so block boundaries are inaudible
"""
from __future__ import annotations

import math

import numpy as np
from scipy.signal import lfilter

F32 = np.float32


class Effect:
    def process(self, x: np.ndarray) -> np.ndarray:  # pragma: no cover
        raise NotImplementedError


class Chain(Effect):
    def __init__(self, effects: list[Effect]):
        self.effects = list(effects)

    def process(self, x: np.ndarray) -> np.ndarray:
        for fx in self.effects:
            x = fx.process(x)
        return x


# --------------------------------------------------------------------------
# Level / dynamics
# --------------------------------------------------------------------------
class Gain(Effect):
    def __init__(self, db: float):
        self.g = F32(10 ** (db / 20))

    def process(self, x):
        return x * self.g


class _BlockEnvelope(Effect):
    """Shared logic: compute a target gain per block, smooth it, ramp across the block."""

    def __init__(self, sr: int, attack_ms: float, release_ms: float):
        self.sr, self.attack_s, self.release_s = sr, attack_ms / 1000, release_ms / 1000
        self.g = 1.0

    def _smooth(self, target: float, n: int) -> np.ndarray:
        t = self.attack_s if target < self.g else self.release_s
        a = math.exp(-n / (self.sr * t)) if t > 0 else 0.0
        new = target + (self.g - target) * a
        ramp = np.linspace(self.g, new, n, dtype=F32)
        self.g = new
        return ramp


class NoiseGate(_BlockEnvelope):
    """Mutes the mic between words so keyboard/fan noise doesn't get 'voice changed'."""

    def __init__(self, sr, threshold_db=-50.0, attack_ms=3.0, release_ms=150.0):
        super().__init__(sr, attack_ms, release_ms)
        self.g = 0.0
        self.threshold = 10 ** (threshold_db / 20)

    def _smooth(self, target, n):
        # for a gate, opening is the fast direction
        t = self.attack_s if target > self.g else self.release_s
        a = math.exp(-n / (self.sr * t)) if t > 0 else 0.0
        new = target + (self.g - target) * a
        ramp = np.linspace(self.g, new, n, dtype=F32)
        self.g = new
        return ramp

    def process(self, x):
        rms = float(np.sqrt(np.mean(x * x))) if len(x) else 0.0
        return x * self._smooth(1.0 if rms >= self.threshold else 0.0, len(x))


class Limiter(_BlockEnvelope):
    """Keeps peaks under the ceiling (heavy effects like distortion can get loud)."""

    def __init__(self, sr, ceiling_db=-1.0, release_ms=80.0):
        super().__init__(sr, attack_ms=0.0, release_ms=release_ms)
        self.ceiling = 10 ** (ceiling_db / 20)

    def process(self, x):
        peak = float(np.max(np.abs(x))) if len(x) else 0.0
        target = min(1.0, self.ceiling / peak) if peak > 0 else 1.0
        return x * self._smooth(target, len(x))


class Compressor(_BlockEnvelope):
    """Downward compressor: turns loud parts down so the mic sits at a steadier
    level (quiet speech and shouting end up closer together). Above `threshold_db`
    the level is pushed toward the threshold by `ratio`; `makeup_db` lifts it back."""

    def __init__(self, sr, threshold_db=-24.0, ratio=4.0, makeup_db=0.0,
                 attack_ms=8.0, release_ms=120.0):
        super().__init__(sr, attack_ms, release_ms)
        self.threshold_db = float(threshold_db)
        self.slope = 1.0 - 1.0 / max(1.0, float(ratio))
        self.makeup = F32(10 ** (makeup_db / 20))

    def process(self, x):
        n = len(x)
        if not n:
            return x
        rms = float(np.sqrt(np.mean(x * x)))
        level_db = 20 * math.log10(max(rms, 1e-7))
        over = max(0.0, level_db - self.threshold_db)
        # target gain = attenuate the amount over threshold, then apply makeup
        target = (10 ** (-(over * self.slope) / 20)) * float(self.makeup)
        return (x * self._smooth(target, n)).astype(F32)


# --------------------------------------------------------------------------
# Filters
# --------------------------------------------------------------------------
class Biquad(Effect):
    """RBJ-cookbook biquad: lowpass, highpass, bandpass, peak."""

    def __init__(self, sr, kind: str, freq: float, q: float = 0.707, gain_db: float = 0.0):
        w0 = 2 * math.pi * freq / sr
        cos, alpha = math.cos(w0), math.sin(w0) / (2 * q)
        A = 10 ** (gain_db / 40)
        if kind == "lowpass":
            b = [(1 - cos) / 2, 1 - cos, (1 - cos) / 2]
            a = [1 + alpha, -2 * cos, 1 - alpha]
        elif kind == "highpass":
            b = [(1 + cos) / 2, -(1 + cos), (1 + cos) / 2]
            a = [1 + alpha, -2 * cos, 1 - alpha]
        elif kind == "bandpass":
            b = [alpha, 0.0, -alpha]
            a = [1 + alpha, -2 * cos, 1 - alpha]
        elif kind == "peak":
            b = [1 + alpha * A, -2 * cos, 1 - alpha * A]
            a = [1 + alpha / A, -2 * cos, 1 - alpha / A]
        else:
            raise ValueError(f"unknown filter kind {kind!r}")
        self.b = np.array(b) / a[0]
        self.a = np.array(a) / a[0]
        self.zi = np.zeros(2)

    def process(self, x):
        y, self.zi = lfilter(self.b, self.a, x, zi=self.zi)
        return y.astype(F32)


# --------------------------------------------------------------------------
# Pitch
# --------------------------------------------------------------------------
class PitchShift(Effect):
    """Low-latency delay-line pitch shifter (the classic 'harmonizer' design).

    Two read taps sweep through a short delay line at speed `ratio`; each tap
    fades out (sin^2 window) right before it wraps around, and the two taps are
    half a window apart so their gains always sum to 1. Latency ~= window/2.

    Bigger window = smoother but more 'echoey'; smaller = more warble.
    Note: this shifts formants along with pitch (deep voice sounds 'big',
    high voice sounds 'chipmunk'). A formant-preserving shifter is a later upgrade.
    """

    MAX_CHUNK = 4096

    def __init__(self, sr: int, ratio: float, window_ms: float = 45.0):
        self.ratio = float(ratio)
        self.W = max(64, int(sr * window_ms / 1000))
        self.min_delay = 2
        size = 1
        while size < self.W + self.min_delay + self.MAX_CHUNK + 4:
            size <<= 1
        self.N = size
        self.mask = size - 1
        self.buf = np.zeros(size, F32)
        self.t = 0  # absolute index of the next sample to be written
        self.phase = 0.0
        self.inc = (1.0 - self.ratio) / self.W

    def process(self, x):
        if self.ratio == 1.0:
            return x
        if len(x) > self.MAX_CHUNK:
            return np.concatenate([self.process(x[i:i + self.MAX_CHUNK])
                                   for i in range(0, len(x), self.MAX_CHUNK)])
        n = len(x)
        idx = (self.t + np.arange(n)) & self.mask
        self.buf[idx] = x
        tabs = self.t + np.arange(n, dtype=np.float64)

        phases = (self.phase + self.inc * np.arange(1, n + 1)) % 1.0
        self.phase = float(phases[-1]) if n else self.phase
        out = np.zeros(n, np.float64)
        for p in (phases, (phases + 0.5) % 1.0):
            pos = tabs - (self.min_delay + p * self.W)
            i0 = np.floor(pos)
            frac = pos - i0
            i0 = i0.astype(np.int64)
            s0 = self.buf[i0 & self.mask]
            s1 = self.buf[(i0 + 1) & self.mask]
            out += (s0 + (s1 - s0) * frac) * np.sin(np.pi * p) ** 2
        self.t += n
        return out.astype(F32)


# --------------------------------------------------------------------------
# Character
# --------------------------------------------------------------------------
class RingMod(Effect):
    """Multiply by a sine. ~30-60 Hz = classic robot/Dalek, a few hundred Hz = alien."""

    def __init__(self, sr, freq: float, mix: float = 1.0):
        self.step = 2 * math.pi * freq / sr
        self.mix = mix
        self.phase = 0.0

    def process(self, x):
        n = len(x)
        ph = self.phase + self.step * np.arange(n)
        self.phase = (self.phase + self.step * n) % (2 * math.pi)
        mod = np.sin(ph).astype(F32)
        return x * (1 - self.mix) + x * mod * self.mix


class Distortion(Effect):
    def __init__(self, drive_db: float = 12.0, out_db: float = -6.0):
        self.drive = F32(10 ** (drive_db / 20))
        self.out = F32(10 ** (out_db / 20))

    def process(self, x):
        return np.tanh(x * self.drive) * self.out


class Bitcrush(Effect):
    """Fewer bits + sample-and-hold to a lower sample rate (8-bit console vibe)."""

    def __init__(self, sr, bits: int = 6, target_sr: float = 8000):
        self.levels = F32(2 ** (bits - 1))
        self.step = sr / target_sr
        self.t = 0
        self.last = F32(0.0)

    def process(self, x):
        n = len(x)
        tabs = self.t + np.arange(n)
        held = np.floor(np.floor(tabs / self.step) * self.step).astype(np.int64) - self.t
        y = np.where(held >= 0, x[np.clip(held, 0, n - 1)], self.last).astype(F32)
        if n:
            self.last = y[-1]
        self.t += n
        return np.round(y * self.levels) / self.levels


class _FeedbackLine:
    """w[t] = x[t] + g * damp(w[t-d]). Returns (w[t-d], w[t]).

    Vectorised by processing chunks no longer than the delay d: every sample a
    chunk needs from the past is already known.
    """

    def __init__(self, d: int, g: float, damp: float = 0.0):
        self.d = max(1, int(d))
        self.g = F32(g)
        self.hist = np.zeros(self.d, F32)
        self.damp = damp
        self.zi = np.zeros(1)

    def run(self, x):
        n = len(x)
        delayed = np.empty(n, F32)
        w_all = np.empty(n, F32)
        i = 0
        while i < n:
            m = min(self.d, n - i)
            dly = self.hist[:m]
            fb = dly
            if self.damp:
                fb, self.zi = lfilter([1 - self.damp], [1, -self.damp], dly, zi=self.zi)
            w = x[i:i + m] + self.g * fb.astype(F32)
            delayed[i:i + m] = dly
            w_all[i:i + m] = w
            self.hist = np.concatenate((self.hist[m:], w))
            i += m
        return delayed, w_all


class Echo(Effect):
    def __init__(self, sr, delay_ms=250.0, feedback=0.4, mix=0.35, damping=0.3):
        self.line = _FeedbackLine(sr * delay_ms / 1000, feedback, damping)
        self.mix = F32(mix)

    def process(self, x):
        delayed, _ = self.line.run(x)
        return x + delayed * self.mix


class Reverb(Effect):
    """Small Schroeder reverb: 4 damped combs in parallel -> 2 allpasses."""

    COMBS_MS = (29.7, 37.1, 41.1, 43.7)
    ALLPASS_MS = (5.0, 1.7)

    def __init__(self, sr, decay_s=1.2, mix=0.25, damping=0.35):
        self.combs = []
        for ms in self.COMBS_MS:
            d = sr * ms / 1000
            g = 10 ** (-3 * d / (decay_s * sr))
            self.combs.append(_FeedbackLine(d, g, damping))
        self.allpasses = [(_FeedbackLine(sr * ms / 1000, 0.7), F32(0.7)) for ms in self.ALLPASS_MS]
        self.mix = F32(mix)

    def process(self, x):
        wet = sum(c.run(x)[0] for c in self.combs) * F32(0.25)
        for line, g in self.allpasses:
            delayed, w = line.run(wet)
            wet = -g * w + delayed
        return x * (1 - self.mix) + wet * self.mix


class Fried(Effect):
    """One knob (0..1) for how 'fried' the mic sounds: from clean, through a
    squashed/hot broadcast sound, to a blown-out, over-driven cheap mic.

    It stacks heavier compression, a narrowing band and tanh saturation as the
    amount rises. 0 (or anything under 0.1%) is a true pass-through."""

    def __init__(self, sr, amount: float = 0.5):
        self.amount = a = max(0.0, min(1.0, float(amount)))
        self.comp = Compressor(sr, threshold_db=-18 - 18 * a, ratio=1.5 + 10 * a,
                               makeup_db=4 * a, attack_ms=3.0, release_ms=60.0)
        self.hp = Biquad(sr, "highpass", 100 + 150 * a)
        self.lp = Biquad(sr, "lowpass", max(2000.0, 10000 - 5500 * a))
        self.drive = F32(10 ** ((3 + 20 * a) / 20))
        self.out = F32(10 ** (-3 / 20))

    def process(self, x):
        if self.amount <= 1e-3 or not len(x):
            return x
        y = self.comp.process(x)
        y = self.hp.process(y)
        y = np.tanh(y * self.drive).astype(F32)
        y = self.lp.process(y)
        return (y * self.out).astype(F32)
