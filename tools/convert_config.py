#!/usr/bin/env python3
"""Convert an old Rustboard (C# / WPF) config.xml into the new config.json.

Usage:
    python tools/convert_config.py                       # default paths
    python tools/convert_config.py old.xml new.json      # custom paths

Old file: C:\\ProgramData\\Rustboard\\config.xml
New file: <app data dir>/com.rustboard.app/config.json
"""

import json
import os
import sys
import xml.etree.ElementTree as ET

# Old VKey enum name -> new key name (names of the Rust `rdev::Key` enum).
VKEY_MAP = {
    "BACK": "Backspace", "TAB": "Tab", "RETURN": "Return", "PAUSE": "Pause",
    "CAPITAL": "CapsLock", "ESCAPE": "Escape", "SPACE": "Space",
    "PAGE_UP": "PageUp", "PAGE_DOWN": "PageDown", "END": "End", "HOME": "Home",
    "LEFT": "LeftArrow", "UP": "UpArrow", "RIGHT": "RightArrow", "DOWN": "DownArrow",
    "SNAPSHOT": "PrintScreen", "INSERT": "Insert", "DELETE": "Delete",
    "LWIN": "MetaLeft", "RWIN": "MetaRight",
    "MULTIPLY": "KpMultiply", "ADD": "KpPlus", "SUBTRACT": "KpMinus",
    "DECIMAL": "KpDelete", "DIVIDE": "KpDivide",
    "NUMLOCK": "NumLock", "SCROLL": "ScrollLock",
    "SHIFT": "ShiftLeft", "LSHIFT": "ShiftLeft", "RSHIFT": "ShiftRight",
    "CONTROL": "ControlLeft", "LCONTROL": "ControlLeft", "RCONTROL": "ControlRight",
    "MENU": "Alt", "LMENU": "Alt", "RMENU": "AltGr",
    "OEM_1": "SemiColon", "OEM_PLUS": "Equal", "OEM_COMMA": "Comma",
    "OEM_MINUS": "Minus", "OEM_PERIOD": "Dot", "OEM_2": "Slash",
    "OEM_3": "BackQuote", "OEM_4": "LeftBracket", "OEM_5": "BackSlash",
    "OEM_6": "RightBracket", "OEM_7": "Quote", "OEM_102": "IntlBackslash",
}
for _i in range(10):
    VKEY_MAP[f"KEY_{_i}"] = f"Num{_i}"
    VKEY_MAP[f"NUMPAD{_i}"] = f"Kp{_i}"
for _c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    VKEY_MAP[f"KEY_{_c}"] = f"Key{_c}"
for _f in range(1, 13):  # rdev only knows F1..F12
    VKEY_MAP[f"F{_f}"] = f"F{_f}"

warnings = []


def local(tag):
    """Tag name without XML namespace."""
    return tag.rsplit("}", 1)[-1]


def child(node, name):
    if node is None:
        return None
    for c in node:
        if local(c.tag) == name:
            return c
    return None


def text(node, name, default=""):
    c = child(node, name)
    if c is None or c.text is None:
        return default
    return c.text


def as_int(node, name, default):
    try:
        return int(text(node, name, str(default)))
    except ValueError:
        return default


def as_bool(node, name, default=False):
    return text(node, name, str(default).lower()).strip().lower() == "true"


def keys_of(node, where):
    """Read <Keys><VKey>..</VKey></Keys> and map every key."""
    result = []
    keys = child(node, "Keys")
    if keys is None:
        return result
    for k in keys:
        old = (k.text or "").strip()
        new = VKEY_MAP.get(old)
        if new is None:
            warnings.append(f"{where}: key '{old}' has no match, skipped")
        elif new not in result:
            result.append(new)
    return result


def convert_sound(node):
    name = text(node, "Name")
    files_node = child(node, "Files")
    files = [(f.text or "") for f in files_node] if files_node is not None else []
    return {
        "name": name,
        "files": [f for f in files if f],
        "keys": keys_of(node, f"sound '{name}'"),
        "volume": as_int(node, "Volume", 100),
        "loop": as_bool(node, "Loop"),
        "error": None,
    }


def convert_preset(node):
    name = text(node, "Name", "Preset")
    sounds = child(node, "SoundCollection")
    return {
        "name": name,
        "keys": keys_of(node, f"preset '{name}'"),
        "sounds": [convert_sound(s) for s in sounds] if sounds is not None else [],
        "selected_sound": 0,
    }


def convert(root):
    ss = child(root, "SoundShortcuts")

    def sc(parent, name, where):
        return {"keys": keys_of(child(parent, name), where)}

    presets_node = child(root, "PresetCollection")
    presets = [convert_preset(p) for p in presets_node] if presets_node is not None else []

    return {
        "volume": as_int(root, "Volume", 100),
        "primary_device_volume": as_int(root, "PrimaryDeviceVolume", 100),
        "secondary_device_volume": as_int(root, "SecondaryDeviceVolume", 100),
        "mic_volume": as_int(root, "MicVolume", 100),
        "overlap": as_bool(root, "Overlap"),
        "primary_output": text(root, "PrimaryOutput") or "Default Output Device",
        "secondary_output": text(root, "SecondaryOutput") or "Disabled",
        "mic_input": text(root, "MicInput") or "Disabled",
        "mic_output": text(root, "MicOutput") or "Disabled",
        "presets": presets,
        "selected_preset": as_int(root, "SelectedPresetIndex", 0),
        "sound_shortcuts": {
            "pause": sc(ss, "Pause", "sound shortcut Pause"),
            "continue": sc(ss, "Continue", "sound shortcut Continue"),
            "stop": sc(ss, "Stop", "sound shortcut Stop"),
        },
    }


def default_input():
    base = os.environ.get("ProgramData", r"C:\ProgramData")
    return os.path.join(base, "Rustboard", "config.xml")


def default_output():
    ident = "com.rustboard.app"
    if sys.platform.startswith("win"):
        base = os.environ.get("APPDATA", os.path.expanduser("~"))
    elif sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    else:
        base = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
    return os.path.join(base, ident, "config.json")


def main(argv):
    src = argv[1] if len(argv) > 1 else default_input()
    dst = argv[2] if len(argv) > 2 else default_output()

    if not os.path.exists(src):
        print(f"Input not found: {src}")
        return 1
    if os.path.exists(dst):
        print(f"Output already exists: {dst}")
        print("Move it away first (the new app would lose its settings otherwise).")
        return 1

    try:
        root = ET.parse(src).getroot()
    except ET.ParseError as e:
        print(f"Cannot read XML: {e}")
        return 1

    cfg = convert(root)

    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2, ensure_ascii=False)

    n_s = sum(len(p["sounds"]) for p in cfg["presets"])
    print(f"Wrote {dst}")
    print(f"  {len(cfg['presets'])} preset(s), {n_s} sound(s)")
    for w in warnings:
        print("  warning:", w)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
