"""Finding the real microphone and the virtual cable.

On Windows the output goes to VB-CABLE's playback side ("CABLE Input");
Discord/OBS/games then pick its recording side ("CABLE Output") as the mic.
We prefer the WASAPI host API: lowest latency of the non-exclusive APIs and,
with auto_convert, it resamples for us so device rates never have to match.
"""
from __future__ import annotations

import sys

import sounddevice as sd

PREFERRED_HOSTAPIS = {
    "win32": ["Windows WASAPI", "MME"],
    "darwin": ["Core Audio"],
    "linux": ["PulseAudio", "JACK Audio Connection Kit", "ALSA"],
}
CABLE_NAMES = ["CABLE Input", "VB-Audio Virtual Cable", "BlackHole", "rustboard_sink"]
# anything that acts as a virtual mic when we play into it (other cables count too)
VIRTUAL_HINTS = CABLE_NAMES + ["Virtual Audio Cable", "Voicemeeter", "VoiceMeeter"]


def is_virtual(name: str) -> bool:
    n = (name or "").lower()
    return any(h.lower() in n for h in VIRTUAL_HINTS)


def has_virtual_cable(devices) -> bool:
    """True if some playback device is a virtual cable (other apps can then hear us as a mic)."""
    return any(d["max_output_channels"] > 0 and is_virtual(d["name"]) for d in devices)


def changes(before: list[str], after: list[str]) -> tuple[list[str], list[str]]:
    """(plugged in, unplugged) device names between two scans, in listing order."""
    return [n for n in after if n not in before], [n for n in before if n not in after]


def rescan() -> None:
    """Make PortAudio list devices again (e.g. after installing a cable) without restarting."""
    sd._terminate()
    sd._initialize()


# One-time setup per platform: (what to install, download page, steps)
SETUP = {
    "win32": ("VB-CABLE", "https://vb-audio.com/Cable/", [
        "Download VB-CABLE (free) and unzip it.",
        "Right-click VBCABLE_Setup_x64.exe → Run as administrator → Install Driver.",
        "Restart your PC.",
        "Open RustBoard: the output is set to \"CABLE Input\" automatically.",
        "In Discord: Settings → Voice & Video → Input Device → \"CABLE Output\". "
        "Turn off Discord's noise suppression (Krisp), it fights the effects.",
    ]),
    "darwin": ("BlackHole", "https://existential.audio/blackhole/", [
        "Install BlackHole 2ch (free), or run: brew install blackhole-2ch",
        "Restart RustBoard: the output is set to \"BlackHole 2ch\" automatically.",
        "In Discord: Settings → Voice & Video → Input Device → \"BlackHole 2ch\".",
    ]),
    "linux": ("a virtual sink", "https://wiki.archlinux.org/title/PipeWire", [
        "Run: pactl load-module module-null-sink sink_name=rustboard_sink",
        "Click \"Check again\" and pick rustboard_sink as the output.",
        "In Discord/OBS, pick \"Monitor of rustboard_sink\" as the mic.",
    ]),
}


def setup_info(platform: str = sys.platform):
    return SETUP.get(platform, SETUP["linux"])


def hostapi_name(index: int) -> str:
    return sd.query_hostapis(index)["name"]


def preferred_hostapi() -> int | None:
    names = PREFERRED_HOSTAPIS.get(sys.platform, [])
    apis = sd.query_hostapis()
    for want in names:
        for i, api in enumerate(apis):
            if api["name"] == want:
                return i
    return None


def list_devices() -> None:
    pref = preferred_hostapi()
    print(f"{'#':>3}  {'in':>2} {'out':>3}  {'host API':<16} name")
    for i, d in enumerate(sd.query_devices()):
        star = "*" if d["hostapi"] == pref else " "
        print(f"{i:>3}{star} {d['max_input_channels']:>2} {d['max_output_channels']:>3}  "
              f"{hostapi_name(d['hostapi'])[:16]:<16} {d['name']}")
    print("\n* = preferred host API. Use --input / --output with a number or part of a name.")


def find_device(query: str | int | None, kind: str) -> int:
    """kind is 'input' or 'output'. query: index, name substring, or None for a sensible default."""
    devices = sd.query_devices()
    pref = preferred_hostapi()
    ch_key = "max_input_channels" if kind == "input" else "max_output_channels"

    if query is not None and str(query).isdigit():
        idx = int(query)
        if devices[idx][ch_key] < 1:
            raise SystemExit(f"device {idx} has no {kind} channels")
        return idx

    def matches(names: list[str]) -> list[int]:
        hits = [i for i, d in enumerate(devices)
                if d[ch_key] > 0 and any(n.lower() in d["name"].lower() for n in names)]
        hits.sort(key=lambda i: devices[i]["hostapi"] != pref)  # preferred host API first
        return hits

    if query is not None:
        hits = matches([str(query)])
        if not hits:
            raise SystemExit(f"no {kind} device matching {query!r}. Run: python -m rustboard devices")
        return hits[0]

    if kind == "output":
        hits = matches(CABLE_NAMES)
        if not hits:
            raise SystemExit(
                "No virtual cable found. Install VB-CABLE (https://vb-audio.com/Cable/), reboot, "
                "or pass --output explicitly.")
        return hits[0]

    if pref is not None:
        idx = sd.query_hostapis(pref)["default_input_device"]
        if idx >= 0:
            return idx
    return sd.default.device[0]


def wasapi_settings(device: int):
    """Shared-mode WASAPI with automatic sample-rate/channel conversion, else None."""
    if hostapi_name(sd.query_devices(device)["hostapi"]) == "Windows WASAPI":
        return sd.WasapiSettings(auto_convert=True)
    return None
