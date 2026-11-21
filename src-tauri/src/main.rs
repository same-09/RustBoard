// no console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod fx;
mod hotkeys;
mod images;
mod library;
mod links;
mod logic;
mod remote;
mod voice_ai;
mod voice_test;
mod state;
mod updater;

use audio::{AudioCmd, AudioSettings, Devices, Levels, LevelsOut, LiveStatus, SoundLevelOut};
use config::Config;
use state::AppState;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
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

/// Re-check sound files for the whole config.
fn refresh(cfg: &mut Config) {
    for p in cfg.presets.iter_mut() {
        for s in p.sounds.iter_mut() {
            s.error = missing_files(&s.files);
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
fn set_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<Config, String> {
    let mut new = config;
    new.normalize();
    let mut cur = state.config.lock().map_err(|e| e.to_string())?;

    for p in new.presets.iter_mut() {
        for s in p.sounds.iter_mut() {
            s.error = missing_files(&s.files);
        }
    }

    state.send(AudioCmd::Configure(AudioSettings::from_config(&new)));
    state.persist(&new);
    *cur = new.clone();
    drop(cur);
    remote::apply(&app, new.remote_on, new.remote_port);
    Ok(new)
}

/// Phone remote: link, QR picture and whether the server runs.
#[tauri::command]
fn remote_info(state: State<'_, AppState>) -> Result<remote::Info, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?;
    Ok(remote::info(&cfg))
}

/// Make a new secret for the phone link. Old links stop working.
#[tauri::command]
fn remote_new_token(state: State<'_, AppState>) -> Result<Config, String> {
    let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
    cfg.remote_token = config::new_token();
    state.persist(&cfg);
    Ok(cfg.clone())
}

/// Switch preset from the UI.
#[tauri::command]
fn select_preset(state: State<'_, AppState>, index: usize) -> Result<Config, String> {
    let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
    if index >= cfg.presets.len() {
        return Err("preset not found".into());
    }
    cfg.selected_preset = index;
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
fn list_devices() -> Result<Devices, String> {
    // Not through the audio thread: it can be busy opening streams, and then Settings
    // waits behind it. cpal lists devices fine from any thread.
    Ok(audio::list_devices())
}

/// Go Live on/off: sounds and your mic both go to the virtual cable (VB-CABLE), so Discord
/// hears them together. Waits for the audio thread, so it runs on a worker thread.
#[tauri::command(async)]
fn set_live(state: State<'_, AppState>, on: bool) -> Result<LiveStatus, String> {
    let (tx, rx) = mpsc::channel();
    state.send(AudioCmd::Live(on, tx));
    rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
}

/// Mic level meters (IN before the gate, OUT after gate + mic volume), loudest since the last call.
/// The UI asks about 12 times a second, and only while the Settings meters are visible.
#[tauri::command]
fn get_levels(state: State<'_, AppState>) -> LevelsOut {
    state.levels.take()
}

/// What the soundboard plays right now, as 5 frequency bands (bass .. treble) + the file of the
/// sound that was started last. The UI asks about 30 times a second and moves the white bars
/// on the tile of that sound.
#[tauri::command]
fn get_sound_level(state: State<'_, AppState>) -> SoundLevelOut {
    state.levels.take_sound()
}

/// A sound picture as a data URL (png, jpg, webp, gif, bmp; 8 MB at most).
#[tauri::command(async)]
fn image_data(path: String) -> Result<String, String> {
    images::data_url(&path)
}

/// Version of this build (the release workflow sets it from the git tag).
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `Documents/RustBoard`: where the copies of sounds and voices live.
/// (Falls back to the app data folder if Windows has no Documents folder.)
fn base_dir(app: &tauri::AppHandle, state: &AppState) -> PathBuf {
    app.path()
        .document_dir()
        .ok()
        .map(|d| d.join("RustBoard"))
        .or_else(|| state.path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("RustBoard"))
}

/// `Documents/RustBoard/Soundboard`: the copies of every sound file you add.
fn soundboard_dir(app: &tauri::AppHandle, state: &AppState) -> PathBuf {
    base_dir(app, state).join(library::FOLDER)
}

/// `Documents/RustBoard/Voices`: one folder per imported AI voice.
fn voices_dir(app: &tauri::AppHandle, state: &AppState) -> PathBuf {
    base_dir(app, state).join(voice_ai::FOLDER)
}

/// Copy picked sound files into the Soundboard folder. Returns the new paths for the config.
/// Runs on a worker thread: copying big files must not freeze the window.
#[tauri::command(async)]
fn import_files(app: tauri::AppHandle, state: State<'_, AppState>, paths: Vec<String>) -> Result<library::ImportOut, String> {
    let list: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    library::import(&soundboard_dir(&app, &state), &list)
}

/// "Import folder": every sound file in the folder (and its sub-folders) is copied into the
/// Soundboard folder. The UI makes one sound for each file.
#[tauri::command(async)]
fn import_folder(app: tauri::AppHandle, state: State<'_, AppState>, folder: String) -> Result<library::ImportOut, String> {
    let list = library::scan(Path::new(&folder))?;
    if list.is_empty() {
        return Err("No sound files in this folder (mp3, wav, ogg, flac, m4a, aac, aiff).".into());
    }
    library::import(&soundboard_dir(&app, &state), &list)
}

#[derive(serde::Serialize)]
struct AiStatus {
    gpu: voice_ai::Gpu,
    engine_ready: bool,
}

/// Graphics card check for AI Voice. Starts helper programs, so it runs on a worker thread.
#[tauri::command(async)]
fn ai_status() -> AiStatus {
    AiStatus { gpu: voice_ai::detect(), engine_ready: voice_ai::ENGINE_READY }
}

#[tauri::command]
fn voice_list(app: tauri::AppHandle, state: State<'_, AppState>) -> Vec<voice_ai::Voice> {
    voice_ai::list(&voices_dir(&app, &state))
}

/// Add a voice from a .zip (voice-models.com) or a .pth / .onnx file. Copying is slow: worker thread.
#[tauri::command(async)]
fn voice_import(app: tauri::AppHandle, state: State<'_, AppState>, path: String) -> Result<voice_ai::Voice, String> {
    voice_ai::import(&voices_dir(&app, &state), Path::new(&path))
}

#[tauri::command]
fn voice_remove(app: tauri::AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    voice_ai::remove(&voices_dir(&app, &state), &id)
}

/// "Test voice": listen to the mic for a few seconds, change the voice, time it.
/// Takes 4 seconds or more, so it runs on a worker thread.
#[tauri::command(async)]
fn ai_test(app: tauri::AppHandle, state: State<'_, AppState>, voice: String) -> Result<voice_test::TestOut, String> {
    let dir = voice_ai::path(&voices_dir(&app, &state), &voice)?;
    let mic = state.config.lock().map_err(|e| e.to_string())?.mic_input.clone();
    voice_test::run(&dir, &mic)
}

/// Play one of the two test clips ("original" or "converted") like a sound.
#[tauri::command]
fn ai_test_play(state: State<'_, AppState>, which: String) -> Result<(), String> {
    let path = voice_test::clip_path(&which).ok_or_else(|| "Unknown clip.".to_string())?;
    if !path.is_file() {
        return Err("Press Test voice first.".into());
    }
    state.send(AudioCmd::Play { file: path.to_string_lossy().into_owned(), volume: 1.0, looped: false, fx: 0 });
    Ok(())
}

/// Show the Soundboard folder in the file manager.
#[tauri::command]
fn open_soundboard_folder(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let dir = soundboard_dir(&app, &state);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
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

/// Download the new setup from our GitHub Release, start it and quit.
/// Progress: "update-progress" {done,total}. Failure: "update-error" (string).
#[tauri::command]
fn install_update(app: tauri::AppHandle, url: String, digest: Option<String>) -> Result<(), String> {
    updater::run(app, url, digest)
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
            let levels = Arc::new(Levels::default());
            let tx = audio::spawn(Arc::clone(&levels), move |msg| {
                let _ = handle.emit("audio-error", msg);
            });
            let _ = tx.send(AudioCmd::Configure(AudioSettings::from_config(&cfg)));

            app.manage(AppState {
                config: Mutex::new(cfg),
                audio: Mutex::new(tx),
                levels,
                binding: std::sync::atomic::AtomicBool::new(false),
                path,
            });

            hotkeys::spawn(app.handle().clone());
            {
                let st = app.state::<AppState>();
                let (on, port) = st.config.lock().map(|c| (c.remote_on, c.remote_port)).unwrap_or((false, 0));
                remote::apply(app.handle(), on, port);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_config,
            remote_info,
            remote_new_token,
            import_files,
            import_folder,
            open_soundboard_folder,
            ai_status,
            voice_list,
            voice_import,
            voice_remove,
            ai_test,
            ai_test_play,
            select_preset,
            play_sound,
            audio_pause,
            audio_resume,
            audio_stop,
            list_devices,
            set_live,
            get_levels,
            get_sound_level,
            image_data,
            app_version,
            open_url,
            install_update,
            begin_bind,
            cancel_bind
        ])
        .run(tauri::generate_context!())
        .expect("error while running Rustboard");
}
