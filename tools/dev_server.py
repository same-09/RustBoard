#!/usr/bin/env python3
"""Mock backend for the UI. Lets you work on HTML/CSS/JS in a normal browser, no Rust needed.

    python tools/dev_server.py [port]      # default 8765
    open http://127.0.0.1:8765

What is fake here:
  - no sound, no global hotkeys (key binding reads keys from the browser page)
  - config is kept in tools/dev_config.json
"""

import json
import os
import sys
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
    return None


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


COMMANDS = {
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
