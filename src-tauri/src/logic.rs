//! Shortcut logic. Port of ShortcutManager + counter commands from ViewModel.

use crate::audio::AudioCmd;
use crate::config::Config;
use crate::counter;
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

#[derive(Clone, Copy)]
enum CounterAction {
    Next,
    Previous,
    Increment,
    Decrement,
    Reset,
}

fn apply_counter(cfg: &mut Config, action: CounterAction) {
    let p = &mut cfg.presets[cfg.selected_preset];
    let n = p.counters.len();
    if n == 0 {
        return;
    }
    if p.selected_counter >= n {
        p.selected_counter = 0;
    }
    match action {
        CounterAction::Next => p.selected_counter = (p.selected_counter + 1) % n,
        CounterAction::Previous => p.selected_counter = (p.selected_counter + n - 1) % n,
        CounterAction::Increment => {
            let c = &mut p.counters[p.selected_counter];
            c.count = c.count.saturating_add(c.increment);
            counter::write_to_file(c);
        }
        CounterAction::Decrement => {
            let c = &mut p.counters[p.selected_counter];
            c.count = c.count.saturating_sub(c.increment);
            counter::write_to_file(c);
        }
        CounterAction::Reset => {
            let c = &mut p.counters[p.selected_counter];
            c.count = 0;
            counter::write_to_file(c);
        }
    }
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
        let enable = cfg.enable;

        // counter shortcuts (repeat allowed, so holding + keeps counting)
        if enable.counters_on() {
            let hit = {
                let cs = &cfg.counter_shortcuts;
                resolve(
                    key,
                    pressed,
                    &[
                        &cs.next.keys[..],
                        &cs.previous.keys[..],
                        &cs.increment.keys[..],
                        &cs.decrement.keys[..],
                        &cs.reset.keys[..],
                    ],
                )
            };
            let action = hit.map(|i| match i {
                0 => CounterAction::Next,
                1 => CounterAction::Previous,
                2 => CounterAction::Increment,
                3 => CounterAction::Decrement,
                _ => CounterAction::Reset,
            });
            if let Some(a) = action {
                apply_counter(&mut cfg, a);
                changed = true;
            }
        }

        // sound control shortcuts
        if enable.sounds_on() {
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
        }

        // play sound + switch preset only on first press (no auto-repeat)
        if first {
            if enable.sounds_on() {
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
            }

            let hit = {
                let list: Vec<&[String]> = cfg.presets.iter().map(|p| &p.keys[..]).collect();
                resolve(key, pressed, &list)
            };
            if let Some(i) = hit {
                cfg.selected_preset = i;
                for c in cfg.presets[i].counters.iter_mut() {
                    counter::read_from_file(c);
                }
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
