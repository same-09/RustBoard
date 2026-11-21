"""Voice presets = an ordered chain of effects.

Each preset is a function (sample_rate) -> list of effects. The engine wraps
every preset with a noise gate in front and a limiter at the end, so presets
only describe the 'character'. Add your own by appending to PRESETS.
"""
from __future__ import annotations

from typing import Callable

from . import effects as fx

PresetFactory = Callable[[int], list]

PRESETS: dict[str, tuple[str, PresetFactory]] = {
    "clean": ("Your normal voice (pass-through)", lambda sr: []),
    "deep": ("Lower, bigger voice", lambda sr: [
        fx.PitchShift(sr, 0.78),
        fx.Biquad(sr, "peak", 140, q=0.8, gain_db=3),
    ]),
    "chipmunk": ("Fast and squeaky", lambda sr: [
        fx.PitchShift(sr, 1.6, window_ms=30),
        fx.Biquad(sr, "highpass", 150),
    ]),
    "demon": ("Very low, gritty, dark", lambda sr: [
        fx.PitchShift(sr, 0.6, window_ms=55),
        fx.Distortion(drive_db=10, out_db=-4),
        fx.Biquad(sr, "lowpass", 3500),
        fx.Reverb(sr, decay_s=1.5, mix=0.18),
    ]),
    "giant": ("Huge voice in a big hall", lambda sr: [
        fx.PitchShift(sr, 0.65, window_ms=55),
        fx.Reverb(sr, decay_s=2.5, mix=0.3),
    ]),
    "robot": ("Metallic robot", lambda sr: [
        fx.RingMod(sr, 50, mix=1.0),
        fx.Echo(sr, delay_ms=11, feedback=0.55, mix=0.6, damping=0.0),
    ]),
    "alien": ("Warbly sci-fi alien", lambda sr: [
        fx.PitchShift(sr, 1.25),
        fx.RingMod(sr, 230, mix=0.5),
        fx.Echo(sr, delay_ms=90, feedback=0.35, mix=0.3),
    ]),
    "radio": ("Walkie-talkie / old radio", lambda sr: [
        fx.Biquad(sr, "highpass", 450),
        fx.Biquad(sr, "lowpass", 2800),
        fx.Distortion(drive_db=14, out_db=-8),
        fx.Gain(4),
    ]),
    "megaphone": ("Loud, honky megaphone", lambda sr: [
        fx.Biquad(sr, "bandpass", 1500, q=0.9),
        fx.Distortion(drive_db=18, out_db=-6),
        fx.Gain(6),
    ]),
    "cave": ("Echoing cave", lambda sr: [
        fx.Echo(sr, delay_ms=280, feedback=0.45, mix=0.4, damping=0.4),
        fx.Reverb(sr, decay_s=2.0, mix=0.35),
    ]),
    "8bit": ("Crunchy retro console", lambda sr: [
        fx.Bitcrush(sr, bits=5, target_sr=7000),
        fx.Biquad(sr, "highpass", 200),
    ]),
}


def build_chain(name: str, sr: int, gate_db: float = -50.0, gain_db: float = 0.0,
                fried: float = 0.0) -> fx.Chain:
    if name not in PRESETS:
        raise KeyError(f"unknown preset {name!r}; choose from {', '.join(PRESETS)}")
    chain: list = []
    if gain_db:
        chain.append(fx.Gain(gain_db))
    if gate_db is not None and gate_db > -120:
        chain.append(fx.NoiseGate(sr, threshold_db=gate_db))
    if fried and fried > 0:          # mic conditioning: shape the raw mic before the preset
        chain.append(fx.Fried(sr, fried))
    chain += PRESETS[name][1](sr)
    chain.append(fx.Limiter(sr, ceiling_db=-1.0))
    return fx.Chain(chain)
