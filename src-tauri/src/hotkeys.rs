//! Global keyboard hook. Works when the window is not focused.
//! Replaces the old RawInput / Win32 code.
//!
//! Key names are the rdev `Key` names ("KeyA", "F5", "KpPlus", "ControlLeft"...).

use crate::logic;
use rdev::{listen, Event, EventType, Key};
use std::thread;
use tauri::AppHandle;

fn key_name(key: Key) -> Option<String> {
    match key {
        Key::Unknown(_) => None,
        k => Some(format!("{:?}", k)),
    }
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        let mut pressed: Vec<String> = Vec::new();

        let result = listen(move |event: Event| match event.event_type {
            EventType::KeyPress(key) => {
                if let Some(name) = key_name(key) {
                    let first = !pressed.contains(&name);
                    if first {
                        pressed.push(name.clone());
                    }
                    logic::on_key_down(&app, &name, &pressed, first);
                }
            }
            EventType::KeyRelease(key) => {
                if let Some(name) = key_name(key) {
                    // call before removing: the released key is part of the combo
                    logic::on_key_up(&app, &pressed);
                    pressed.retain(|k| k != &name);
                }
            }
            _ => {}
        });

        if let Err(e) = result {
            eprintln!("keyboard hook failed: {:?}", e);
        }
    });
}
