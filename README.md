# Rustboard (Rust + HTML/CSS/JS + Python)

Repo: https://github.com/same-09/RustBoard

Port of the old C# / WPF Rustboard. Same idea: global keyboard shortcuts that
play sounds, and "Go Live" sends them to Discord through a virtual cable.

| Part | Language | Where |
|---|---|---|
| Backend: global keys, audio, config | Rust ([Tauri 2](https://tauri.app)) | `src-tauri/src` |
| UI | HTML, CSS, JS (no framework, no build step) | `ui/` |
| Tools: config converter, mock backend for UI work | Python 3 (stdlib only) | `tools/` |

## Run

Needs Rust and the [Tauri prerequisites](https://tauri.app/start/prerequisites/).

```
cargo install tauri-cli --version "^2"
cd src-tauri
cargo tauri dev        # run
cargo tauri build      # installer
```

> The Rust code was written without a compiler at hand. Expect to fix a typo
> or two on the first `cargo check`. The UI and the Python tools were tested.

## Release (no key, no secrets)

```
git tag v0.0.1
git push origin v0.0.1
```

The workflow (`.github/workflows/build.yml`) builds `Rustboard_x.y.z_x64-setup.exe` (double-click: installer with
Start menu + desktop shortcut) and attaches it to a GitHub Release. Pushes to `main` only test the build.

**Updates:** on start the app asks GitHub for the newest Release; if it is newer than the running version, a bar says
"New version ... is available" with an **Update** button. The app downloads the setup from our own Release (checks
SHA-256 when GitHub gives one), starts it and quits. Nothing needs signing, so no `TAURI_SIGNING_PRIVATE_KEY` secret.
Without a signature, trust comes only from HTTPS + our GitHub repo.

## Move your old settings

```
python tools/convert_config.py
```

Reads `C:\ProgramData\Rustboard\config.xml`, writes the new `config.json`
into the app data folder. Keys with no match (mouse buttons, F13+, media keys)
are skipped with a warning.

## Work on the UI without Rust

```
python tools/dev_server.py
# open http://127.0.0.1:8765
```

Mock backend: no sound, no global hotkeys (keys are read from the page).

## Layout

```
src-tauri/src/
  main.rs      Tauri commands + setup
  config.rs    config model, load/save (config.json)
  audio.rs     audio thread: 2 outputs, mixer, mic passthrough (+ resampler), Go Live, VB-Cable detection
  hotkeys.rs   global key hook (rdev)
  logic.rs     shortcut matching, preset switch
  state.rs     shared app state
ui/            index.html, style.css, js/{main,api,keys}.js
tools/         convert_config.py, dev_server.py
```

## Differences from the C# app

- **Not ported:** minimize to tray, window size memory, About window.
- **Audio formats:** mp3, wav, ogg, flac, m4a/aac, aiff. **wma is not supported.**
- **Mic passthrough:** needs devices with 32-bit float audio. Mic and output may run at different sample rates
  (simple linear resampler, 40 ms jitter buffer).
- **Key names** are `rdev` names (`KeyA`, `Kp1`, `F5`...), not Windows VKeys.
- **Linux:** the key hook needs X11 (no Wayland). **macOS:** allow Accessibility.
- Cloning a preset clears its switch keys (two presets on one key fight).

Config: `<app data>/com.rustboard.app/config.json`
(Windows `%APPDATA%`, Linux `~/.local/share`, macOS `~/Library/Application Support`).

## Go Live: sounds + your voice in Discord (VB-CABLE)

1. Install [VB-CABLE](https://vb-audio.com/Cable/) (free), restart Windows.
2. Rustboard: press **Go Live** (top bar). Sounds (Secondary output) and your mic (Settings > Microphone input, or the
   default mic) both go to `CABLE Input`. Your headphones (Primary output) keep playing the sounds as before.
3. Discord: Settings > Voice & Video > Input Device = `CABLE Output`, Noise Suppression off.
4. Press Go Live again to stop; your manual output/mic settings come back untouched.

Settings > *Discord (VB-Cable)* shows whether a cable was found and lets you pick another device for Go Live
(default: `Auto`, which prefers VB-CABLE, then other virtual cables). While live, sounds are 3 dB lower so
sounds + mic do not clip on the cable.

## Installer look

The setup `.exe` is NSIS (made by Tauri). Its images are `src-tauri/installer/sidebar.bmp` (164x314) and `header.bmp` (150x57),
set in `tauri.conf.json` > `bundle.windows.nsis`. The pages and buttons stay the Windows style; NSIS cannot draw HTML.
The HTML mock in `installer-ui/` needs its own Tauri installer app to be used.
