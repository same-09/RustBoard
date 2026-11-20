use crate::audio::{AudioCmd, Levels};
use crate::config::{self, Config, Sound};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct AppState {
    pub config: Mutex<Config>,
    pub audio: Mutex<Sender<AudioCmd>>,
    /// mic level meters, written by the audio thread
    pub levels: Arc<Levels>,
    /// true while the UI waits for the user to press a key combination
    pub binding: AtomicBool,
    pub path: PathBuf,
}

impl AppState {
    pub fn send(&self, cmd: AudioCmd) {
        if let Ok(tx) = self.audio.lock() {
            let _ = tx.send(cmd);
        }
    }

    pub fn persist(&self, cfg: &Config) {
        if let Err(e) = config::save(&self.path, cfg) {
            eprintln!("cannot save config: {e}");
        }
    }

    /// Play one random file of the sound (same as SoundManager.Play).
    pub fn play(&self, sound: &Sound) {
        if sound.files.is_empty() {
            return;
        }
        let file = sound.files[pick(sound.files.len())].clone();
        self.send(AudioCmd::Play {
            file,
            volume: sound.volume as f32 / 100.0,
            looped: sound.looped,
        });
    }
}

/// Small random index without extra crates.
fn pick(n: usize) -> usize {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    nanos % n.max(1)
}
