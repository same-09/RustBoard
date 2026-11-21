//! `.rbpack`: one file with a preset, its sounds, its pictures and (if wanted) the settings.
//! Easy to send to a friend. Importing adds a new preset; nothing is replaced.
//!
//! Not inside: audio devices, the phone-remote link, the AI engine folder and AI voice models
//! (those belong to your PC, or are big and have their own licence).

use crate::config::{Config, Preset};
use crate::pack_file::{self, Pack};
use crate::{images, library};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const FORMAT: &str = "rbpack";
const VERSION: u32 = 1;
/// All files together, so one wrong click does not fill the disk.
const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
const MAX_FILES: usize = 2000;

/// One file inside the pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    /// what the preset points at while it is inside the pack ("f0", "f1", ...)
    id: String,
    /// original file name
    name: String,
    /// "sound" or "image"
    kind: String,
    size: u64,
}

/// The settings that make sense on another PC. Field names are the same as in `Config`,
/// so the UI can copy them over as they are.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: u32,
    pub mic_volume: u32,
    pub gate_on: bool,
    pub gate_threshold_db: i32,
    pub duck_on: bool,
    pub limiter_on: bool,
    pub fx_on: bool,
    pub fx_preset: String,
    pub fx_keys: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_config(&Config::default())
    }
}

impl Settings {
    fn from_config(c: &Config) -> Settings {
        Settings {
            volume: c.volume,
            mic_volume: c.mic_volume,
            gate_on: c.gate_on,
            gate_threshold_db: c.gate_threshold_db,
            duck_on: c.duck_on,
            limiter_on: c.limiter_on,
            fx_on: c.fx_on,
            fx_preset: c.fx_preset.clone(),
            fx_keys: c.fx_keys.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default)]
    app: String,
    #[serde(default)]
    name: String,
    preset: Preset,
    #[serde(default)]
    settings: Option<Settings>,
    #[serde(default)]
    files: Vec<Entry>,
}

/* ---------- export ---------- */

#[derive(Debug, Serialize)]
pub struct ExportOut {
    pub path: String,
    pub files: u32,
    /// sound or picture files that no longer exist (left out)
    pub missing: u32,
    pub bytes: u64,
}

/// Collects the files while the preset is rewritten to point at ids.
struct Collector {
    entries: Vec<Entry>,
    paths: Vec<PathBuf>,
    ids: HashMap<String, String>,
    missing: u32,
    total: u64,
}

impl Collector {
    /// Id for this file, or `None` if it does not exist.
    fn add(&mut self, path: &str, kind: &str) -> Result<Option<String>, String> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(id) = self.ids.get(path) {
            return Ok(Some(id.clone()));
        }
        let size = match fs::metadata(path) {
            Ok(m) if m.is_file() => m.len(),
            _ => {
                self.missing += 1;
                return Ok(None);
            }
        };
        if self.entries.len() >= MAX_FILES {
            return Err(format!("Too many files in one pack ({MAX_FILES} at most)."));
        }
        self.total += size;
        if self.total > MAX_TOTAL {
            return Err("The files are too big for one pack (1 GB at most).".into());
        }
        let id = format!("f{}", self.entries.len());
        let name = pack_file::safe_name(path);
        self.entries.push(Entry { id: id.clone(), name, kind: kind.to_string(), size });
        self.paths.push(PathBuf::from(path));
        self.ids.insert(path.to_string(), id.clone());
        Ok(Some(id))
    }
}

/// Write preset number `index` to `dest`.
pub fn export(cfg: &Config, index: usize, with_settings: bool, dest: &Path) -> Result<ExportOut, String> {
    let mut preset = cfg.presets.get(index).ok_or("Preset not found.")?.clone();
    let mut col = Collector { entries: Vec::new(), paths: Vec::new(), ids: HashMap::new(), missing: 0, total: 0 };
    for s in &mut preset.sounds {
        let files = std::mem::take(&mut s.files);
        for f in &files {
            if let Some(id) = col.add(f, "sound")? {
                s.files.push(id);
            }
        }
        let image = std::mem::take(&mut s.image);
        s.image = col.add(&image, "image")?.unwrap_or_default();
        s.error = None;
    }
    preset.sounds.retain(|s| !s.files.is_empty());
    preset.selected_sound = 0;
    if preset.sounds.is_empty() {
        return Err("Nothing to export: this preset has no sound files that exist.".into());
    }
    let manifest = Manifest {
        format: FORMAT.into(),
        version: VERSION,
        app: format!("RustBoard {}", env!("CARGO_PKG_VERSION")),
        name: preset.name.clone(),
        preset,
        settings: with_settings.then(|| Settings::from_config(cfg)),
        files: col.entries.clone(),
    };
    let json = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let files: Vec<(&Path, u64)> = col.paths.iter().zip(&col.entries).map(|(p, e)| (p.as_path(), e.size)).collect();
    pack_file::write(dest, &json, &files)?;
    Ok(ExportOut { path: dest.display().to_string(), files: files.len() as u32, missing: col.missing, bytes: col.total })
}

/* ---------- look inside, import ---------- */

fn open(path: &Path) -> Result<(Manifest, Pack), String> {
    let pack = Pack::open(path)?;
    let m: Manifest = serde_json::from_slice(&pack.manifest).map_err(|_| "This pack is damaged.".to_string())?;
    if m.format != FORMAT {
        return Err("This is not a RustBoard pack.".into());
    }
    if m.version > VERSION {
        return Err("This pack was made by a newer RustBoard. Update the app first.".into());
    }
    if m.files.len() > MAX_FILES {
        return Err("This pack is damaged.".into());
    }
    let sizes: Vec<u64> = m.files.iter().map(|e| e.size).collect();
    if sizes.iter().try_fold(0u64, |a, s| a.checked_add(*s)).map(|t| t > MAX_TOTAL).unwrap_or(true) {
        return Err("This pack is too big or damaged.".into());
    }
    pack.check_sizes(&sizes)?;
    Ok((m, pack))
}

#[derive(Debug, Serialize)]
pub struct Peek {
    pub name: String,
    pub sounds: u32,
    pub files: u32,
    pub bytes: u64,
    pub has_settings: bool,
    pub app: String,
}

/// What is in the pack (nothing is copied yet), so the UI can ask before importing.
pub fn peek(path: &Path) -> Result<Peek, String> {
    let (m, _) = open(path)?;
    Ok(Peek {
        name: m.name,
        sounds: m.preset.sounds.len() as u32,
        files: m.files.len() as u32,
        bytes: m.files.iter().map(|e| e.size).sum(),
        has_settings: m.settings.is_some(),
        app: m.app,
    })
}

#[derive(Debug, Serialize)]
pub struct ImportOut {
    /// the new preset: its files point at the copies on this PC
    pub preset: Preset,
    pub settings: Option<Settings>,
    pub files: u32,
    /// files or sounds that could not be used
    pub skipped: u32,
}

/// Copy the sounds into `sound_dir` and the pictures into `picture_dir` (same name and same
/// content = no second copy), and give back the preset. The UI adds it to the config.
pub fn import(path: &Path, sound_dir: &Path, picture_dir: &Path) -> Result<ImportOut, String> {
    let (m, mut pack) = open(path)?;
    let sizes: Vec<u64> = m.files.iter().map(|e| e.size).collect();
    let tmp = std::env::temp_dir().join(format!("rustboard-pack-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = copy_files(&m, &mut pack, &sizes, &tmp, sound_dir, picture_dir);
    let _ = fs::remove_dir_all(&tmp);
    let (map, mut skipped) = result?;

    let mut preset = m.preset;
    for s in &mut preset.sounds {
        // only files we copied ourselves: a pack can never point at other files on this PC
        s.files = s.files.iter().filter_map(|id| map.get(id).cloned()).collect();
        s.image = map.get(&s.image).cloned().unwrap_or_default();
        s.error = None;
    }
    let before = preset.sounds.len();
    preset.sounds.retain(|s| !s.files.is_empty());
    skipped += (before - preset.sounds.len()) as u32;
    if preset.sounds.is_empty() {
        return Err("This pack has no usable sounds.".into());
    }
    preset.selected_sound = 0;
    preset.keys.clear(); // the key that switches to a preset is yours to choose
    if preset.name.trim().is_empty() {
        preset.name = "Imported".into();
    }
    Ok(ImportOut { preset, settings: m.settings, files: map.len() as u32, skipped })
}

/// Returns id -> path of the copy, and how many files were skipped.
fn copy_files(
    m: &Manifest,
    pack: &mut Pack,
    sizes: &[u64],
    tmp: &Path,
    sound_dir: &Path,
    picture_dir: &Path,
) -> Result<(HashMap<String, String>, u32), String> {
    let mut map = HashMap::new();
    let mut skipped = 0u32;
    for (i, e) in m.files.iter().enumerate() {
        let name = pack_file::safe_name(&e.name);
        let (ok, dir) = match e.kind.as_str() {
            "sound" => (library::is_audio(Path::new(&name)), sound_dir),
            "image" => (images::is_picture(Path::new(&name)), picture_dir),
            _ => (false, sound_dir),
        };
        if !ok {
            skipped += 1;
            continue;
        }
        let sub = tmp.join(i.to_string());
        fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
        let file = sub.join(&name);
        pack.extract(sizes, i, &file)?;
        match library::copy_in(dir, &file) {
            Ok(copy) => {
                map.insert(e.id.clone(), copy.to_string_lossy().into_owned());
            }
            Err(_) => skipped += 1,
        }
    }
    Ok((map, skipped))
}
