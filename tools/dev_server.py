#!/usr/bin/env python3
"""Mock backend for the UI. Lets you work on HTML/CSS/JS in a normal browser, no Rust needed.

    python tools/dev_server.py [port]      # default 8765
    open http://127.0.0.1:8765

What is fake here:
  - no sound, no global hotkeys (key binding reads keys from the browser page)
  - config is kept in tools/dev_config.json
"""

import json
import math
import os
import sys
import time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI_DIR = os.path.join(ROOT, "ui")
CONFIG_PATH = os.path.join(ROOT, "tools", "dev_config.json")


def default_config():
    return {
        "volume": 100,
        "primary_device_volume": 100,
        "secondary_device_volume": 100,
        "mic_volume": 100,
        "gate_on": False,
        "duck_on": False,
        "limiter_on": True,
        "gate_threshold_db": -50,
        "meters_on": True,
        "fx_on": False,
        "ai_voice_on": False,
        "ai_voice": "",
        "fx_preset": "clean",
        "overlap": False,
        "primary_output": "Default Output Device",
        "secondary_output": "Disabled",
        "mic_input": "Disabled",
        "mic_output": "Disabled",
        "live_output": "Auto (VB-Cable)",
        "presets": [
            {
                "name": "New Preset",
                "keys": [],
                "sounds": [],
                "selected_sound": 0,
            }
        ],
        "selected_preset": 0,
        "sound_shortcuts": {
            "pause": {"keys": []},
            "continue": {"keys": []},
            "stop": {"keys": []},
        },
    }


def load_config():
    try:
        with open(CONFIG_PATH, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default_config()


def save_config(cfg):
    with open(CONFIG_PATH, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)


STATE = {"config": load_config()}


def normalize(cfg):
    cfg.setdefault("live_output", "Auto (VB-Cable)")
    cfg.setdefault("gate_on", False)
    cfg.setdefault("duck_on", False)
    cfg.setdefault("limiter_on", True)
    cfg.setdefault("gate_threshold_db", -50)
    cfg.setdefault("meters_on", True)
    cfg.setdefault("fx_on", False)
    cfg.setdefault("ai_voice_on", False)
    cfg.setdefault("ai_voice", "")
    cfg.setdefault("fx_preset", "clean")
    cfg["gate_threshold_db"] = max(-70, min(-20, int(cfg["gate_threshold_db"])))
    if not cfg["presets"]:
        cfg["presets"].append(default_config()["presets"][0])
    if cfg["selected_preset"] >= len(cfg["presets"]):
        cfg["selected_preset"] = 0
    for p in cfg["presets"]:
        if p["selected_sound"] >= len(p["sounds"]):
            p["selected_sound"] = 0
        for s in p["sounds"]:
            missing = [f for f in s["files"] if not os.path.exists(f)]
            s["error"] = (
                "Following files do not exist:\n" + "\n".join(missing) if missing else None
            )
    return cfg


# ---- commands (same names as the Tauri commands in src-tauri/src/main.rs) ----

def get_state(_args):
    return STATE["config"]


def set_config(args):
    cfg = normalize(args["config"])
    STATE["config"] = cfg
    save_config(cfg)
    return cfg


def select_preset(args):
    cfg = STATE["config"]
    index = args["index"]
    if not 0 <= index < len(cfg["presets"]):
        raise ValueError("preset not found")
    cfg["selected_preset"] = index
    save_config(cfg)
    return cfg


def play_sound(args):
    cfg = STATE["config"]
    try:
        sound = cfg["presets"][args["preset"]]["sounds"][args["sound"]]
    except (IndexError, KeyError):
        raise ValueError("sound not found")
    print(f"[mock] play: {sound['name']} ({len(sound['files'])} file(s), loop={sound['loop']})")
    if sound["files"]:
        PLAYING["file"] = sound["files"][0]
        PLAYING["until"] = time.time() + (3600 if sound.get("loop") else 4)
    return None


PLAYING = {"file": "", "until": 0.0}


def get_sound_level(_args):
    """Fake music: five bands that bounce while the last played sound 'plays'."""
    t = time.time()
    on = t < PLAYING["until"]
    bands = [0.0] * 5
    if on:
        beat = max(0.0, math.sin(t * 7.0)) ** 3
        for i in range(5):
            wobble = 0.5 + 0.5 * math.sin(t * (5 + 3 * i) + i * 1.7)
            base = (0.28 if i == 0 else 0.12 / (1 + i * 0.4)) * (0.35 + 0.65 * (beat if i < 2 else wobble))
            bands[i] = base
    return {"file": PLAYING["file"], "bands": bands}


AUDIO_EXT = (".mp3", ".wav", ".ogg", ".flac", ".m4a", ".aac", ".aiff", ".aif")
MOCK_FOLDER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock-Soundboard")


def _mock_import(paths):
    files, skipped = [], 0
    for p in paths:
        if p.lower().endswith(AUDIO_EXT):
            files.append({"path": p, "name": os.path.splitext(os.path.basename(p))[0]})
        else:
            skipped += 1
    return {"files": files, "skipped": skipped, "folder": MOCK_FOLDER}


def import_files(args):
    return _mock_import(args.get("paths", []))


def import_folder(args):
    folder = args.get("folder", "")
    if not os.path.isdir(folder):
        raise ValueError("Cannot read folder '%s'." % folder)
    found = sorted(os.path.join(r, f) for r, _d, fs in os.walk(folder) for f in fs if f.lower().endswith(AUDIO_EXT))
    if not found:
        raise ValueError("No sound files in this folder (mp3, wav, ogg, flac, m4a, aac, aiff).")
    return _mock_import(found)


def make_noop(label):
    def run(_args):
        print(f"[mock] {label}")
        return None

    return run


def list_devices(_args):
    return {
        "outputs": ["Disabled", "Default Output Device", "Speakers (Mock)", "CABLE Input (Mock)"],
        "inputs": ["Disabled", "Default Input Device", "Microphone (Mock)"],
        "cable": "CABLE Input (Mock)",
    }


LIVE = {"on": False}


def set_live(args):
    LIVE["on"] = bool(args.get("on"))
    print(f"[mock] live = {LIVE['on']}")
    return {"on": LIVE["on"], "output": "CABLE Input (Mock)" if LIVE["on"] else "",
            "sounds": LIVE["on"], "mic": LIVE["on"], "headphones": True}


def get_levels(_args):
    """Fake mic: a voice that comes and goes, plus a little room noise."""
    cfg = STATE["config"]
    t = time.time()
    speaking = math.sin(t * 1.3) > 0.1
    noise = -58 + 3 * math.sin(t * 7)
    voice = -24 + 6 * math.sin(t * 9) if speaking else noise
    gate_open = (not cfg.get("gate_on")) or voice >= cfg.get("gate_threshold_db", -50)
    out = voice if gate_open else -100
    return {"input_db": voice, "output_db": out, "active": True, "gate_open": gate_open,
            "ai_state": 2 if cfg.get("ai_voice_on") else 0, "ai_text": "Balanced · cuda"}


def image_data(args):
    import base64
    import mimetypes
    path = args.get("path", "")
    mime = mimetypes.guess_type(path)[0] or ""
    if not mime.startswith("image/"):
        raise ValueError("This is not a picture.")
    with open(path, "rb") as f:
        return "data:%s;base64,%s" % (mime, base64.b64encode(f.read()).decode("ascii"))


# AI Voice mock: RB_MOCK_GPU=weak python tools/dev_server.py  shows the weak-card warning
_VOICES = []


# Install mock: RB_MOCK_ENGINE=missing shows "not installed"; Install then runs a fake 24 s install.
_INSTALL = {"t0": None, "cancelled": False, "installed": False}
_INSTALL_STEPS = [("Getting Python", 4), ("Getting pip", 2), ("Installing audio packages", 6),
                  ("Installing PyTorch (the big one)", 50), ("Installing AI packages", 18),
                  ("Downloading the base voice models", 20)]
_INSTALL_SECONDS = 24.0


def ai_status(_args):
    if os.environ.get("RB_MOCK_GPU") == "weak":
        gpu = {"name": "NVIDIA GeForce GTX 1050", "vram_mb": 2048, "level": "weak"}
    else:
        gpu = {"name": "NVIDIA GeForce RTX 3060", "vram_mb": 12288, "level": "strong"}
    if os.environ.get("RB_MOCK_ENGINE") == "missing" and not _INSTALL["installed"]:
        return {"gpu": gpu, "engine_ready": False, "engine_dir": "", "engine_own": False,
                "engine_error": "The AI voice engine is not installed yet. Press Install AI voice."}
    own = _INSTALL["installed"]
    return {"gpu": gpu, "engine_ready": True, "engine_own": own, "engine_error": "",
            "engine_dir": "C:\\Users\\me\\Documents\\RustBoard\\AI\\code" if own else "C:\\Users\\me\\RustBoard-Engine"}


def ai_install(_args):
    st = ai_install_status({})
    if st["running"]:
        raise ValueError("The install is already running.")
    _INSTALL.update(t0=time.time(), cancelled=False)


def ai_install_status(_args):
    steps = len(_INSTALL_STEPS)
    idle = {"running": False, "done": False, "step": 0, "steps": steps, "title": "", "line": "",
            "percent": 0, "error": ""}
    if _INSTALL["t0"] is None:
        return idle
    frac = (time.time() - _INSTALL["t0"]) / _INSTALL_SECONDS
    if _INSTALL["cancelled"]:
        return dict(idle, error="Cancelled.", percent=int(min(frac, 1) * 100))
    if frac >= 1:
        _INSTALL["installed"] = True
        return dict(idle, done=True, percent=100, step=steps, title="Done")
    pct, before = frac * 100, 0
    for i, (title, share) in enumerate(_INSTALL_STEPS):
        if pct < before + share:
            sub = (pct - before) / share
            line = ("Downloading torch 2.7.1+cu128: %.1f GB of 3.2 GB" % (sub * 3.2) if "PyTorch" in title
                    else "Collecting package %d ..." % int((pct - before) * 3))
            return dict(idle, running=True, step=i + 1, title=title, percent=int(pct), line=line)
        before += share
    return idle


def ai_install_cancel(_args):
    if _INSTALL["t0"] is not None:
        _INSTALL["cancelled"] = True


def ai_remove_engine(_args):
    _INSTALL.update(t0=None, installed=False)


def voice_list(_args):
    return list(_VOICES)


def voice_import(args):
    stem = os.path.splitext(os.path.basename(args["path"]))[0] or "voice"
    vid = "".join(c if c.isalnum() or c in "-_" else "_" for c in stem)[:60]
    voice = {"id": vid, "name": vid.replace("_", " "), "size_mb": 55, "has_index": True}
    _VOICES[:] = [v for v in _VOICES if v["id"] != vid] + [voice]
    return voice


import threading
_TEST_CANCEL = threading.Event()


def ai_test_cancel(_args):
    _TEST_CANCEL.set()


def ai_test(args):
    # real app: 4 s of mic + conversion. Mock: RB_MOCK_ENGINE=1 pretends the engine works.
    import time
    _TEST_CANCEL.clear()
    end = time.time() + float(os.environ.get("RB_MOCK_TEST_SECS", "4.5"))
    while time.time() < end:
        if _TEST_CANCEL.is_set():
            raise ValueError("Test cancelled.")
        time.sleep(0.05)
    if os.environ.get("RB_MOCK_ENGINE") == "1":
        return {"original": True, "converted": True, "error": None, "recorded_secs": 4.0, "convert_secs": 2.9,
                "speed": 0.73, "verdict": "tight", "peak_db": -12.0}
    return {"original": True, "converted": False,
            "error": "The live voice engine is not in this version yet, so the voice cannot be changed. Your recording still works.",
            "recorded_secs": 4.0, "convert_secs": 0.0, "speed": 0.0, "verdict": "", "peak_db": -12.0}


def voice_remove(args):
    _VOICES[:] = [v for v in _VOICES if v["id"] != args["id"]]


def remote_info(_args):
    # a fake QR picture (checkerboard): the real one comes from Rust
    cells = "".join('<rect x="%d" y="%d" width="1" height="1"/>' % (x, y) for x in range(21) for y in range(21) if (x * 7 + y * 3 + x * y) % 3 == 0)
    qr = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="-2 -2 25 25" shape-rendering="crispEdges"><rect x="-2" y="-2" width="25" height="25" fill="#fff"/><g fill="#000">%s</g></svg>' % cells
    return {"running": True, "url": "http://192.168.1.6:8787/?k=d5e70dbe7fa4056731ad599e83c", "qr": qr}


COMMANDS = {
    "remote_info": remote_info,
    "ai_status": ai_status,
    "ai_install": ai_install,
    "ai_install_status": ai_install_status,
    "ai_install_cancel": ai_install_cancel,
    "ai_remove_engine": ai_remove_engine,
    "voice_list": voice_list,
    "voice_import": voice_import,
    "voice_remove": voice_remove,
    "ai_test": ai_test,
    "ai_test_cancel": ai_test_cancel,
    "ai_test_play": make_noop("play test clip"),
    "image_data": image_data,
    "get_levels": get_levels,
    "import_files": import_files,
    "import_folder": import_folder,
    "open_soundboard_folder": make_noop("open Soundboard folder"),
    "get_sound_level": get_sound_level,
    "get_state": get_state,
    "set_config": set_config,
    "select_preset": select_preset,
    "play_sound": play_sound,
    "audio_pause": make_noop("pause"),
    "audio_resume": make_noop("resume"),
    "audio_stop": make_noop("stop"),
    "list_devices": list_devices,
    "set_live": set_live,
    "app_version": lambda _a: "0.0.0-mock",
    "open_url": make_noop("open_url"),
    "begin_bind": make_noop("begin bind"),
    "cancel_bind": make_noop("cancel bind"),
}


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=UI_DIR, **kwargs)

    def do_POST(self):
        # read the body first, even for errors (else the client sees a dropped connection)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""

        if not self.path.startswith("/api/"):
            self.send_error(404)
            return
        name = self.path[len("/api/"):]
        fn = COMMANDS.get(name)
        if fn is None:
            self.send_error(404, f"unknown command: {name}")
            return
        try:
            result = fn(json.loads(raw or b"{}"))
            body = json.dumps(result).encode("utf-8") if result is not None else b""
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
        except Exception as e:  # keep the dev server alive
            body = str(e).encode("utf-8")
            self.send_response(400)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, fmt, *args):
        # keep the console quiet: skip log lines of API calls
        if not getattr(self, "path", "").startswith("/api/"):
            super().log_message(fmt, *args)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"Rustboard UI (mock backend): http://127.0.0.1:{port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
