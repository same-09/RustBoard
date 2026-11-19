//! Config model + load/save. Port of ConfigurationModel / Preset / Sound / Counter.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DISABLED: &str = "Disabled";
pub const DEFAULT_OUT: &str = "Default Output Device";
/// "Go Live" output: find the virtual cable (VB-CABLE) on its own.
pub const AUTO_CABLE: &str = "Auto (VB-Cable)";

fn d_100() -> u32 {
    100
}
fn d_one() -> i32 {
    1
}
fn d_format() -> String {
    "{0}".to_string()
}
fn d_primary() -> String {
    DEFAULT_OUT.to_string()
}
fn d_disabled() -> String {
    DISABLED.to_string()
}
fn d_auto_cable() -> String {
    AUTO_CABLE.to_string()
}
fn d_enable() -> DisplayOption {
    DisplayOption::Both
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Shortcut {
    #[serde(default)]
    pub keys: Vec<String>,
}

impl Shortcut {
    pub fn with(keys: &[&str]) -> Self {
        Shortcut {
            keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Counter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub file: String,
    #[serde(default = "d_format")]
    pub format: String,
    #[serde(default = "d_one")]
    pub increment: i32,
    #[serde(default)]
    pub count: i32,
    #[serde(default)]
    pub error: Option<String>,
}

impl Default for Counter {
    fn default() -> Self {
        Counter {
            name: String::new(),
            file: String::new(),
            format: d_format(),
            increment: 1,
            count: 0,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sound {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default = "d_100")]
    pub volume: u32,
    #[serde(default, rename = "loop")]
    pub looped: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl Default for Sound {
    fn default() -> Self {
        Sound {
            name: String::new(),
            files: Vec::new(),
            keys: Vec::new(),
            volume: 100,
            looped: false,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Preset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub counters: Vec<Counter>,
    #[serde(default)]
    pub sounds: Vec<Sound>,
    #[serde(default)]
    pub selected_counter: usize,
    #[serde(default)]
    pub selected_sound: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterShortcuts {
    pub next: Shortcut,
    pub previous: Shortcut,
    pub increment: Shortcut,
    pub decrement: Shortcut,
    pub reset: Shortcut,
}

impl Default for CounterShortcuts {
    fn default() -> Self {
        CounterShortcuts {
            next: Shortcut::with(&["KpMultiply"]),
            previous: Shortcut::default(),
            increment: Shortcut::with(&["KpPlus"]),
            decrement: Shortcut::with(&["KpMinus"]),
            reset: Shortcut::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SoundShortcuts {
    #[serde(default)]
    pub pause: Shortcut,
    #[serde(default, rename = "continue")]
    pub resume: Shortcut,
    #[serde(default)]
    pub stop: Shortcut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayOption {
    Both,
    Counters,
    Sounds,
}

impl DisplayOption {
    pub fn counters_on(self) -> bool {
        matches!(self, DisplayOption::Both | DisplayOption::Counters)
    }
    pub fn sounds_on(self) -> bool {
        matches!(self, DisplayOption::Both | DisplayOption::Sounds)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "d_100")]
    pub volume: u32,
    #[serde(default = "d_100")]
    pub primary_device_volume: u32,
    #[serde(default = "d_100")]
    pub secondary_device_volume: u32,
    #[serde(default = "d_100")]
    pub mic_volume: u32,
    #[serde(default)]
    pub overlap: bool,
    #[serde(default = "d_primary")]
    pub primary_output: String,
    #[serde(default = "d_disabled")]
    pub secondary_output: String,
    #[serde(default = "d_disabled")]
    pub mic_input: String,
    #[serde(default = "d_disabled")]
    pub mic_output: String,
    /// Where "Go Live" sends sounds + mic. `AUTO_CABLE` = detect VB-CABLE.
    #[serde(default = "d_auto_cable")]
    pub live_output: String,
    #[serde(default = "d_enable")]
    pub enable: DisplayOption,
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub selected_preset: usize,
    #[serde(default)]
    pub counter_shortcuts: CounterShortcuts,
    #[serde(default)]
    pub sound_shortcuts: SoundShortcuts,
}

impl Default for Config {
    fn default() -> Self {
        let mut c = Config {
            volume: 100,
            primary_device_volume: 100,
            secondary_device_volume: 100,
            mic_volume: 100,
            overlap: false,
            primary_output: d_primary(),
            secondary_output: d_disabled(),
            mic_input: d_disabled(),
            mic_output: d_disabled(),
            live_output: d_auto_cable(),
            enable: DisplayOption::Both,
            presets: Vec::new(),
            selected_preset: 0,
            counter_shortcuts: CounterShortcuts::default(),
            sound_shortcuts: SoundShortcuts::default(),
        };
        c.normalize();
        c
    }
}

impl Config {
    /// Keep invariants: at least one preset, indexes in range, volumes 0..=100.
    pub fn normalize(&mut self) {
        if self.presets.is_empty() {
            self.presets.push(Preset {
                name: "New Preset".into(),
                ..Preset::default()
            });
        }
        if self.selected_preset >= self.presets.len() {
            self.selected_preset = 0;
        }
        for p in &mut self.presets {
            if p.selected_counter >= p.counters.len() {
                p.selected_counter = 0;
            }
            if p.selected_sound >= p.sounds.len() {
                p.selected_sound = 0;
            }
            for s in &mut p.sounds {
                s.volume = s.volume.min(100);
            }
        }
        self.volume = self.volume.min(100);
        self.primary_device_volume = self.primary_device_volume.min(100);
        self.secondary_device_volume = self.secondary_device_volume.min(100);
        self.mic_volume = self.mic_volume.min(100);
    }
}

pub fn load(path: &Path) -> Config {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    match serde_json::from_str::<Config>(&text) {
        Ok(mut c) => {
            c.normalize();
            c
        }
        Err(e) => {
            eprintln!("config is corrupted: {e}");
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let moved = path.with_file_name(format!("config_corrupted_{ts}.json"));
            let _ = fs::rename(path, moved);
            Config::default()
        }
    }
}

/// Write to temp file, then rename over the real one (no half-written config).
pub fn save(path: &Path, cfg: &Config) -> Result<(), String> {
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}
