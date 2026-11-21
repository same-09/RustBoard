"""Rustboard <-> engine bridge.  Run with the voice engine's Python (the one Rustboard
installed in Documents/RustBoard/AI, or the .venv of an engine folder).

    python rb_bridge.py convert <engine_dir> <voice_dir> <in.f32> <in_rate> <out.f32> [pitch]
        One clip: raw mono float32 in, raw mono float32 out. Prints one JSON line:
        {"ok": true, "rate": N}  or  {"ok": false, "error": "..."}

    python rb_bridge.py prepare <engine_dir>
        Download the base models once (about 550 MB; their names go to stderr as they come).
        Prints one JSON line: {"ok": true}  or  {"ok": false, "error": "..."}

    python rb_bridge.py live <engine_dir> <voice_dir> [mode] [pitch]
        Live stream. First a JSON line: {"ready": true, "sr": N, "block": B, "mode": "..."}
        (or {"ready": false, "error": "..."}), then forever: read B float32 samples
        from stdin, write B float32 samples to stdout. Ends when stdin closes.

stdout carries audio, so everything else a library prints is sent to stderr.
"""
import json
import os
import sys

# keep the real stdout for our own use; anything else that prints goes to stderr
_OUT = os.fdopen(os.dup(1), "wb", buffering=0)
sys.stdout = sys.stderr


def say(obj):
    _OUT.write((json.dumps(obj) + "\n").encode("utf-8"))
    _OUT.flush()


def fail(msg, live=False):
    say({"ready": False, "error": msg} if live else {"ok": False, "error": msg})
    sys.exit(0)


def setup(root):
    if not os.path.isdir(os.path.join(root, "rustboard")):
        raise RuntimeError("Folder has no 'rustboard' package: " + root)
    sys.path.insert(0, root)
    import numpy as np  # noqa: F401
    try:
        import torch  # noqa: F401
    except ImportError:
        raise RuntimeError("PyTorch is missing in the voice engine's Python. Press Install AI voice in Rustboard (Settings > AI Voice).")
    from rustboard.ai import assets
    if assets.missing():
        assets.download()  # base models, ~550 MB, once


def find_pth(voice_dir):
    from rustboard.ai import assets
    try:
        return assets.find_voice(voice_dir)
    except SystemExit as e:
        raise RuntimeError(str(e))


def convert(root, voice_dir, src, rate, dst, pitch):
    import numpy as np
    setup(root)
    from rustboard.ai import preview
    pth, _idx = find_pth(voice_dir)
    x = np.fromfile(src, dtype="<f4")
    if len(x) == 0:
        raise RuntimeError("Empty recording.")
    x = preview._resample(x, int(rate), preview.DEMO_SR)
    peak = float(np.max(np.abs(x))) or 1.0
    x = x * min(0.9 / peak, 8.0)
    y, sr = preview.get_previewer().convert(pth, x, pitch)
    np.asarray(y, dtype="<f4").tofile(dst)
    say({"ok": True, "rate": int(sr)})


def live(root, voice_dir, mode, pitch):
    import numpy as np
    setup(root)
    import torch
    from rustboard.ai import hwcheck
    from rustboard.ai.engine import AIVoiceEngine, StreamSettings
    from rustboard.ai.rvc import RVCVoice, SharedModels, VoiceSettings

    cuda = torch.cuda.is_available()
    dev = torch.device("cuda:0" if cuda else "cpu")
    if mode not in hwcheck.MODES:
        mode = "Balanced" if cuda else "Laptop"
    block, f0, context = hwcheck.MODES[mode]
    pth, _idx = find_pth(voice_dir)
    shared = SharedModels(dev, cuda)
    voice = RVCVoice(pth, shared, VoiceSettings(pitch=pitch, f0_method=f0))
    eng = AIVoiceEngine(voice, 0, 0, StreamSettings(block_time=block, context_time=context))
    eng.prewarm()
    n = eng.block_frame
    say({"ready": True, "sr": int(eng.sr), "block": int(n), "mode": mode,
         "device": "cuda" if cuda else "cpu"})

    stdin = os.fdopen(os.dup(0), "rb", buffering=0)
    need = n * 4
    out = np.zeros((n, 1), np.float32)
    while True:
        buf = b""
        while len(buf) < need:
            chunk = stdin.read(need - len(buf))
            if not chunk:
                return
            buf += chunk
        x = np.frombuffer(buf, dtype="<f4").astype(np.float32)
        try:
            eng._process(x, out)
            y = out[:, 0]
        except Exception as e:  # keep the stream alive, send silence
            print("bridge error:", repr(e), file=sys.stderr)
            y = np.zeros(n, np.float32)
        _OUT.write(np.ascontiguousarray(y, dtype="<f4").tobytes())


def main(argv):
    if len(argv) < 2:
        fail("no command")
    cmd = argv[1]
    try:
        if cmd == "convert":
            root, voice, src, rate, dst = argv[2:7]
            pitch = float(argv[7]) if len(argv) > 7 else 0.0
            convert(root, voice, src, rate, dst, pitch)
        elif cmd == "prepare":
            setup(argv[2])
            say({"ok": True})
        elif cmd == "live":
            root, voice = argv[2:4]
            mode = argv[4] if len(argv) > 4 else ""
            pitch = float(argv[5]) if len(argv) > 5 else 0.0
            live(root, voice, mode, pitch)
        else:
            fail("unknown command " + cmd)
    except SystemExit:
        raise
    except BaseException as e:  # noqa: BLE001
        fail(f"{type(e).__name__}: {e}", live=(cmd == "live"))


if __name__ == "__main__":
    main(sys.argv)
