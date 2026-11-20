//! Shortcut logic. Port of ShortcutManager from ViewModel.

use crate::audio::AudioCmd;
use crate::config::Config;
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};

/// Pick the shortcut whose keys contain `key` and are all pressed.
/// Bigger combos win. On a tie the last one wins (same as the C# OrderBy+Last).
fn resolve(key: &str, pressed: &[String], items: &[&[String]]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None; // (index, key count)
    for (i, keys) in items.iter().enumerate() {
        if keys.iter().any(|k| k == key) && keys.iter().all(|k| pressed.contains(k)) {
            match best {
                Some((_, n)) if n > keys.len() => {}
                _ => best = Some((i, keys.len())),
            }
        }
    }
    best.map(|(i, _)| i)
}

/// Called on every key-down (also on auto-repeat). `first` is true for the first down only.
pub fn on_key_down(app: &AppHandle, key: &str, pressed: &[String], first: bool) {
    let state = app.state::<AppState>();
    if state.binding.load(Ordering::SeqCst) {
        return; // user is binding keys: do not trigger anything
    }

    let mut changed = false;
    let snapshot: Option<Config>;
    {
        let mut cfg = match state.config.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // sound control shortcuts
        let hit = {
            let ss = &cfg.sound_shortcuts;
            resolve(key, pressed, &[&ss.pause.keys[..], &ss.resume.keys[..], &ss.stop.keys[..]])
        };
        match hit {
            Some(0) => state.send(AudioCmd::Pause),
            Some(1) => state.send(AudioCmd::Resume),
            Some(2) => state.send(AudioCmd::Stop),
            _ => {}
        }

        // play sound + switch preset only on first press (no auto-repeat)
        if first {
            let empty: &[String] = &[];
            let hit = {
                let p = &cfg.presets[cfg.selected_preset];
                let list: Vec<&[String]> = p
                    .sounds
                    .iter()
                    .map(|s| if s.files.is_empty() { empty } else { &s.keys[..] })
                    .collect();
                resolve(key, pressed, &list)
            };
            if let Some(i) = hit {
                let sound = cfg.presets[cfg.selected_preset].sounds[i].clone();
                cfg.preset_mut_selected().selected_sound = i;
                state.play(&sound);
                changed = true;
            }

            let hit = {
                let list: Vec<&[String]> = cfg.presets.iter().map(|p| &p.keys[..]).collect();
                resolve(key, pressed, &list)
            };
            if let Some(i) = hit {
                cfg.selected_preset = i;
                changed = true;
            }
        }

        snapshot = if changed { Some(cfg.clone()) } else { None };
    }

    if let Some(snap) = snapshot {
        state.persist(&snap);
        let _ = app.emit("config-changed", snap);
    }
}

/// Called on key-up. While binding, the first release finishes the binding:
/// every key that was held becomes the combination.
pub fn on_key_up(app: &AppHandle, pressed: &[String]) {
    let state = app.state::<AppState>();
    if state.binding.swap(false, Ordering::SeqCst) {
        let _ = app.emit("keys-bound", pressed.to_vec());
    }
}

impl Config {
    pub fn preset_mut_selected(&mut self) -> &mut crate::config::Preset {
        let i = self.selected_preset;
        &mut self.presets[i]
    }
}
