//! Config model + load/save. Port of ConfigurationModel / Preset / Sound.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DISABLED: &str = "Disabled";
pub const DEFAULT_OUT: &str = "Default Output Device";
/// "Go Live" output: find the virtual cable (VB-CABLE) on its own.
pub const AUTO_CABLE: &str = "Auto (VB-Cable)";
pub const GATE_MIN_DB: i32 = -70;
pub const GATE_MAX_DB: i32 = -20;

fn d_100() -> u32 {
    100
}
fn d_true() -> bool {
    true
}
/// Gate opens above this mic level (dBFS, RMS). Quiet room noise sits near -60.
fn d_gate_db() -> i32 {
    -50
}
fn d_fx_preset() -> String {
    "clean".to_string()
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
fn d_remote_port() -> u16 {
    8787
}

/// Secret in the phone-remote link. Random, 32 hex characters.
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    if getrandom::getrandom(&mut b).is_err() {
        // should never happen; still better than an empty token
        let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        b = n.to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
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
    /// Effect on this sound: an id from `fx::IDS` ("clean" = none).
    #[serde(default = "d_fx_preset")]
    pub fx: String,
    /// Picture file shown on the sound tile. Empty = generated cover.
    #[serde(default)]
    pub image: String,
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
            fx: d_fx_preset(),
            image: String::new(),
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
    pub sounds: Vec<Sound>,
    #[serde(default)]
    pub selected_sound: usize,
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
    /// Noise gate on the mic: mutes it between words (keyboard, fan). Off by default.
    #[serde(default)]
    pub gate_on: bool,
    /// Gate threshold in dBFS (RMS), `GATE_MIN_DB..=GATE_MAX_DB`.
    #[serde(default = "d_gate_db")]
    pub gate_threshold_db: i32,
    /// Ducking: sounds get quieter while you speak, so friends hear you over them. Off by default.
    #[serde(default)]
    pub duck_on: bool,
    /// Limiter: stops loud peaks (sounds and mic) from clipping. On by default.
    #[serde(default = "d_true")]
    pub limiter_on: bool,
    /// Show the IN / OUT level meters in Settings.
    #[serde(default = "d_true")]
    pub meters_on: bool,
    /// Mic sound effects (voice changer) on the mic passthrough. Off by default.
    #[serde(default)]
    pub fx_on: bool,
    /// Which effect: an id from `fx::IDS` ("clean", "deep", "robot", ...).
    #[serde(default = "d_fx_preset")]
    pub fx_preset: String,
    /// Optional hotkey per mic effect: effect id -> keys. No entry = no hotkey.
    #[serde(default)]
    pub fx_keys: BTreeMap<String, Vec<String>>,
    /// AI Voice: your voice becomes another voice (an imported model), live. Off by default.
    #[serde(default)]
    pub ai_voice_on: bool,
    /// Which imported voice: its folder name in `Documents/RustBoard/Voices`. Empty = none.
    #[serde(default)]
    pub ai_voice: String,
    /// Folder of an engine (has `rustboard/` and `.venv/`). Empty = look in the usual places.
    #[serde(default)]
    pub ai_engine_dir: String,
    /// Text to speech: the Windows voice (empty = the Windows default) and its speed (-10..10).
    #[serde(default)]
    pub tts_voice: String,
    #[serde(default)]
    pub tts_rate: i32,
    /// Phone remote: a small web page on the local network that plays sounds. Off by default.
    #[serde(default)]
    pub remote_on: bool,
    #[serde(default = "d_remote_port")]
    pub remote_port: u16,
    /// Secret part of the link. Whoever has the link can use the remote.
    #[serde(default)]
    pub remote_token: String,
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
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub selected_preset: usize,
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
            gate_on: false,
            duck_on: false,
            limiter_on: true,
            gate_threshold_db: d_gate_db(),
            meters_on: true,
            fx_on: false,
            fx_preset: d_fx_preset(),
            fx_keys: BTreeMap::new(),
            ai_voice_on: false,
            ai_voice: String::new(),
            ai_engine_dir: String::new(),
            tts_voice: String::new(),
            tts_rate: 0,
            remote_on: false,
            remote_port: d_remote_port(),
            remote_token: String::new(),
            overlap: false,
            primary_output: d_primary(),
            secondary_output: d_disabled(),
            mic_input: d_disabled(),
            mic_output: d_disabled(),
            live_output: d_auto_cable(),
            presets: Vec::new(),
            selected_preset: 0,
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
            if p.selected_sound >= p.sounds.len() {
                p.selected_sound = 0;
            }
            for s in &mut p.sounds {
                s.volume = s.volume.min(100);
                if !crate::fx::is_known(&s.fx) {
                    s.fx = d_fx_preset();
                }
            }
        }
        self.volume = self.volume.min(100);
        self.primary_device_volume = self.primary_device_volume.min(100);
        self.secondary_device_volume = self.secondary_device_volume.min(100);
        self.mic_volume = self.mic_volume.min(100);
        self.gate_threshold_db = self.gate_threshold_db.clamp(GATE_MIN_DB, GATE_MAX_DB);
        if !crate::fx::is_known(&self.fx_preset) {
            self.fx_preset = d_fx_preset();
        }
        self.tts_rate = self.tts_rate.clamp(crate::tts::RATE_MIN, crate::tts::RATE_MAX);
        self.fx_keys.retain(|id, keys| crate::fx::is_known(id) && !keys.is_empty());
        if self.remote_port < 1024 {
            self.remote_port = d_remote_port();
        }
        if self.remote_token.len() < 16 {
            self.remote_token = new_token();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_gets_gate_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(!c.gate_on);
        assert_eq!(c.gate_threshold_db, -50);
        assert!(c.meters_on);
        assert!(!c.duck_on, "ducking is off until the user turns it on");
        assert!(c.limiter_on, "the limiter is on by default");
    }

    #[test]
    fn old_config_gets_effect_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(!c.fx_on);
        assert!(!c.ai_voice_on);
        assert_eq!(c.fx_preset, "clean");
    }

    #[test]
    fn old_sound_has_no_effect_and_bad_effect_is_fixed() {
        let s: Sound = serde_json::from_str("{}").unwrap();
        assert_eq!(s.fx, "clean");
        let mut c = Config::default();
        c.presets[0].sounds.push(Sound { fx: "bogus".into(), ..Sound::default() });
        c.presets[0].sounds.push(Sound { fx: "cave".into(), ..Sound::default() });
        c.normalize();
        assert_eq!(c.presets[0].sounds[0].fx, "clean");
        assert_eq!(c.presets[0].sounds[1].fx, "cave");
    }

    #[test]
    fn unknown_effect_falls_back_to_clean() {
        let mut c = Config::default();
        c.fx_preset = "does-not-exist".into();
        c.normalize();
        assert_eq!(c.fx_preset, "clean");
        c.fx_preset = "robot".into();
        c.normalize();
        assert_eq!(c.fx_preset, "robot");
    }

    #[test]
    fn effect_hotkeys_are_optional_and_cleaned() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.fx_keys.is_empty());
        let mut c = Config::default();
        c.fx_keys.insert("robot".into(), vec!["F5".into()]);
        c.fx_keys.insert("bogus".into(), vec!["F6".into()]);
        c.fx_keys.insert("cave".into(), vec![]);
        c.normalize();
        assert_eq!(c.fx_keys.len(), 1);
        assert!(c.fx_keys.contains_key("robot"));
    }

    #[test]
    fn remote_is_off_with_a_token_and_a_safe_port() {
        let mut c: Config = serde_json::from_str("{}").unwrap();
        assert!(!c.remote_on);
        c.remote_port = 80;
        c.normalize();
        assert_eq!(c.remote_port, 8787);
        assert_eq!(c.remote_token.len(), 32);
        let t = c.remote_token.clone();
        c.normalize();
        assert_eq!(c.remote_token, t, "token must stay the same");
        assert_ne!(new_token(), new_token());
    }

    #[test]
    fn gate_threshold_is_clamped() {
        let mut c = Config::default();
        c.gate_threshold_db = -300;
        c.normalize();
        assert_eq!(c.gate_threshold_db, GATE_MIN_DB);
        c.gate_threshold_db = 5;
        c.normalize();
        assert_eq!(c.gate_threshold_db, GATE_MAX_DB);
    }
}
