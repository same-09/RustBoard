// no console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod counter;
mod hotkeys;
mod links;
mod logic;
mod state;

use audio::{AudioCmd, AudioSettings, Devices, LiveStatus};
use config::Config;
use state::AppState;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter, Manager, State};

/// Names of sound files that do not exist (shown as an error in the UI).
fn missing_files(files: &[String]) -> Option<String> {
    let missing: Vec<&str> = files
        .iter()
        .filter(|f| !Path::new(f.as_str()).exists())
        .map(|f| f.as_str())
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!("Following files do not exist:\n{}", missing.join("\n")))
    }
}

/// Re-read counter files and re-check sound files for the whole config.
fn refresh(cfg: &mut Config) {
    for p in cfg.presets.iter_mut() {
        for c in p.counters.iter_mut() {
            counter::read_from_file(c);
        }
        for s in p.sounds.iter_mut() {
            s.error = missing_files(&s.files);
        }
    }
}

/// Compare new counters with old ones and sync the text files, like the
/// property setters in Counter.cs did.
fn sync_counters(old: &Config, new: &mut Config) {
    for (pi, p) in new.presets.iter_mut().enumerate() {
        for (ci, c) in p.counters.iter_mut().enumerate() {
            let prev = old.presets.get(pi).and_then(|op| op.counters.get(ci));
            match prev {
                Some(o) if o.file == c.file && o.format == c.format && o.count == c.count => {
                    c.error = o.error.clone();
                }
                Some(o) if o.file != c.file => {
                    // new file: take its number if it matches, then write
                    counter::read_from_file(c);
                    if c.error.is_none() {
                        counter::write_to_file(c);
                    }
                }
                _ => counter::write_to_file(c),
            }
        }
    }
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> Result<Config, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?;
    Ok(cfg.clone())
}

/// UI sends the whole config after each edit. Returns it back with fresh errors.
#[tauri::command]
fn set_config(state: State<'_, AppState>, config: Config) -> Result<Config, String> {
    let mut new = config;
    new.normalize();
    let mut cur = state.config.lock().map_err(|e| e.to_string())?;

    sync_counters(&cur, &mut new);
    for p in new.presets.iter_mut() {
        for s in p.sounds.iter_mut() {
            s.error = missing_files(&s.files);
        }
    }

    state.send(AudioCmd::Configure(AudioSettings::from_config(&new)));
    state.persist(&new);
    *cur = new.clone();
    Ok(new)
}

/// Switch preset from the UI. Re-reads counter files, like the old PresetSelected command.
#[tauri::command]
fn select_preset(state: State<'_, AppState>, index: usize) -> Result<Config, String> {
    let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
    if index >= cfg.presets.len() {
        return Err("preset not found".into());
    }
    cfg.selected_preset = index;
    for c in cfg.presets[index].counters.iter_mut() {
        counter::read_from_file(c);
    }
    state.persist(&cfg);
    Ok(cfg.clone())
}

#[tauri::command]
fn play_sound(state: State<'_, AppState>, preset: usize, sound: usize) -> Result<(), String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?;
    let s = cfg
        .presets
        .get(preset)
        .and_then(|p| p.sounds.get(sound))
        .ok_or("sound not found")?;
    state.play(s);
    Ok(())
}

#[tauri::command]
fn audio_pause(state: State<'_, AppState>) {
    state.send(AudioCmd::Pause);
}

#[tauri::command]
fn audio_resume(state: State<'_, AppState>) {
    state.send(AudioCmd::Resume);
}

#[tauri::command]
fn audio_stop(state: State<'_, AppState>) {
    state.send(AudioCmd::Stop);
}

/// `async` flag = runs on a worker thread, so waiting for the audio thread does not block the UI.
#[tauri::command(async)]
fn list_devices(state: State<'_, AppState>) -> Result<Devices, String> {
    let (tx, rx) = mpsc::channel();
    state.send(AudioCmd::Devices(tx));
    rx.recv_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())
}

/// Go Live on/off: sounds and your mic both go to the virtual cable (VB-CABLE), so Discord
/// hears them together. Waits for the audio thread, so it runs on a worker thread.
#[tauri::command(async)]
fn set_live(state: State<'_, AppState>, on: bool) -> Result<LiveStatus, String> {
    let (tx, rx) = mpsc::channel();
    state.send(AudioCmd::Live(on, tx));
    rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
}

/// Version of this build (the release workflow sets it from the git tag).
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Open one of our own links (new release, VB-CABLE site) in the default browser.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !links::allowed(&url) {
        return Err("This link is not allowed.".into());
    }
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(&url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// After this, the next key release sends a "keys-bound" event with the held keys.
#[tauri::command]
fn begin_bind(state: State<'_, AppState>) {
    state.binding.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn cancel_bind(state: State<'_, AppState>) {
    state.binding.store(false, Ordering::SeqCst);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("config.json");

            let mut cfg = config::load(&path);
            refresh(&mut cfg);

            // audio thread reports errors to the UI as "audio-error" events
            let handle = app.handle().clone();
            let tx = audio::spawn(move |msg| {
                let _ = handle.emit("audio-error", msg);
            });
            let _ = tx.send(AudioCmd::Configure(AudioSettings::from_config(&cfg)));

            app.manage(AppState {
                config: Mutex::new(cfg),
                audio: Mutex::new(tx),
                binding: std::sync::atomic::AtomicBool::new(false),
                path,
            });

            hotkeys::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_config,
            select_preset,
            play_sound,
            audio_pause,
            audio_resume,
            audio_stop,
            list_devices,
            set_live,
            app_version,
            open_url,
            begin_bind,
            cancel_bind
        ])
        .run(tauri::generate_context!())
        .expect("error while running Rustboard");
}
