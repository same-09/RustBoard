//! AI Voice (your voice becomes another voice, live): everything AROUND the converter.
//! - `detect`: is the graphics card strong enough? (NVIDIA, 6 GB or more)
//! - `list` / `import` / `remove`: the voice models the user brings from voice-models.com
//!   (a .zip with a .pth model and often an .index file, or a bare .pth / .onnx).
//!
//! The converter itself (model -> live audio) is NOT in this build: `ENGINE_READY` is false and
//! the UI says so. Voices and settings are saved and ready for it.

use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::Path;

/// Folder inside `Documents/RustBoard/` that holds one sub-folder per imported voice.
pub const FOLDER: &str = "Voices";
/// Set to true when the live converter is built in.
pub const ENGINE_READY: bool = false;

/// Below this much video memory, live conversion is likely to stutter.
const MIN_VRAM_MB: u32 = 6000;
/// What we keep from a zip. Everything else (images, readme, ...) is ignored.
const KEEP_EXT: [&str; 3] = ["pth", "onnx", "index"];
const MODEL_EXT: [&str; 2] = ["pth", "onnx"];
/// One file may be this big. A zip may hold at most three times this.
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Gpu {
    pub name: String,
    pub vram_mb: u32,
    /// "strong" (NVIDIA, 6 GB+), "weak" (anything else we found), "none" (no card found)
    pub level: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Voice {
    /// folder name, also what the config stores
    pub id: String,
    pub name: String,
    pub size_mb: u32,
    pub has_index: bool,
}

/* ---------- graphics card ---------- */

pub fn classify(name: &str, vram_mb: u32) -> &'static str {
    let n = name.to_lowercase();
    let nvidia = ["nvidia", "geforce", "rtx", "gtx", "quadro"].iter().any(|k| n.contains(k));
    if nvidia && vram_mb >= MIN_VRAM_MB {
        "strong"
    } else {
        "weak"
    }
}

/// One line of `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`.
fn parse_nvidia(line: &str) -> Option<(String, u32)> {
    let (name, mem) = line.rsplit_once(',')?;
    let mb = mem.trim().parse::<u32>().ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_string(), mb))
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new(program);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    let out = c.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Takes a moment (starts nvidia-smi / PowerShell): call it from a worker thread.
pub fn detect() -> Gpu {
    if let Some(out) = run("nvidia-smi", &["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]) {
        if let Some((name, mb)) = out.lines().filter_map(parse_nvidia).max_by_key(|(_, mb)| *mb) {
            return Gpu { level: classify(&name, mb), name, vram_mb: mb };
        }
    }
    // not NVIDIA (or no driver tool): only the name is known, and there is no CUDA
    let names = run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }"],
    );
    if let Some(out) = names {
        let first = out
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.to_lowercase().contains("basic render"));
        if let Some(name) = first {
            return Gpu { name: name.to_string(), vram_mb: 0, level: "weak" };
        }
    }
    Gpu { name: String::new(), vram_mb: 0, level: "none" }
}

/* ---------- voices ---------- */

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default()
}

/// A folder name that is safe on Windows: letters, digits, "-" and "_" only.
fn safe_id(stem: &str) -> String {
    let s: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join("_");
    let s: String = s.chars().take(60).collect();
    if s.is_empty() { "voice".to_string() } else { s }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

pub fn list(dir: &Path) -> Vec<Voice> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(id) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let (mut size, mut model, mut index) = (0u64, false, false);
        if let Ok(files) = fs::read_dir(&p) {
            for f in files.flatten() {
                let fp = f.path();
                let ext = ext_of(&fp);
                if KEEP_EXT.contains(&ext.as_str()) {
                    size += f.metadata().map(|m| m.len()).unwrap_or(0);
                }
                model |= MODEL_EXT.contains(&ext.as_str());
                index |= ext == "index";
            }
        }
        if model {
            let name = id.replace(&['_', '-'][..], " ");
            out.push(Voice { id, name, size_mb: (size / 1_048_576) as u32, has_index: index });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn has_model(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|f| MODEL_EXT.contains(&ext_of(&f.path()).as_str())))
        .unwrap_or(false)
}

fn copy_one(src: &Path, target: &Path) -> Result<(), String> {
    let name = src.file_name().ok_or("Bad file name.")?;
    let len = fs::metadata(src).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err("This file is too big (limit 2 GB).".into());
    }
    fs::copy(src, target.join(name)).map(|_| ()).map_err(|e| format!("Cannot copy the file: {e}"))
}

fn extract_zip(src: &Path, target: &Path) -> Result<(), String> {
    let file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("This is not a valid zip file: {e}"))?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        // only the file name is used (no folders, no ".."), so nothing can land outside `target`
        let name = Path::new(entry.name()).file_name().and_then(|n| n.to_str()).map(str::to_string);
        let Some(name) = name else { continue };
        if !KEEP_EXT.contains(&ext_of(Path::new(&name)).as_str()) {
            continue;
        }
        if entry.size() > MAX_FILE {
            return Err("A file in this zip is too big (limit 2 GB).".into());
        }
        let mut out = fs::File::create(target.join(&name)).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_FILE), &mut out)
            .map_err(|e| format!("Cannot unzip: {e}"))?;
        total += copied;
        if total > 3 * MAX_FILE {
            return Err("This zip is too big.".into());
        }
    }
    Ok(())
}

/// Add a voice from a .zip (voice-models.com) or a bare .pth / .onnx file.
pub fn import(dir: &Path, src: &Path) -> Result<Voice, String> {
    if !src.is_file() {
        return Err("File not found.".into());
    }
    let ext = ext_of(src);
    if !matches!(ext.as_str(), "zip" | "pth" | "onnx") {
        return Err("Pick the .zip you downloaded from voice-models.com, or a .pth / .onnx model file.".into());
    }
    let base = safe_id(src.file_stem().and_then(|s| s.to_str()).unwrap_or("voice"));
    let mut id = base.clone();
    let mut n = 2;
    while dir.join(&id).exists() {
        id = format!("{base}_{n}");
        n += 1;
    }
    let target = dir.join(&id);
    fs::create_dir_all(&target).map_err(|e| format!("Cannot create the Voices folder: {e}"))?;

    let result = if ext == "zip" { extract_zip(src, &target) } else { copy_one(src, &target) }.and_then(|_| {
        if has_model(&target) { Ok(()) } else { Err("No voice model (.pth or .onnx) found in this file.".to_string()) }
    });
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&target); // the folder we just made, nothing else
        return Err(e);
    }
    list(dir).into_iter().find(|v| v.id == id).ok_or_else(|| "Import failed.".to_string())
}

pub fn remove(dir: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Bad voice name.".into());
    }
    let p = dir.join(id);
    if !p.is_dir() {
        return Ok(());
    }
    fs::remove_dir_all(&p).map_err(|e| format!("Cannot remove the voice: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_needs_nvidia_and_6gb() {
        assert_eq!(classify("NVIDIA GeForce RTX 3060", 12288), "strong");
        assert_eq!(classify("NVIDIA GeForce GTX 1060", 6144), "strong");
        assert_eq!(classify("NVIDIA GeForce GTX 1050", 2048), "weak");
        assert_eq!(classify("AMD Radeon RX 6800", 16384), "weak");
        assert_eq!(classify("Intel(R) UHD Graphics", 0), "weak");
    }

    #[test]
    fn nvidia_smi_line() {
        assert_eq!(parse_nvidia("NVIDIA GeForce RTX 3060, 12288"), Some(("NVIDIA GeForce RTX 3060".to_string(), 12288)));
        assert_eq!(parse_nvidia("no comma here"), None);
        assert_eq!(parse_nvidia("name, notanumber"), None);
    }

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(safe_id("My Voice (v2) [RVC]"), "My_Voice_v2_RVC");
        assert_eq!(safe_id("../../evil"), "evil");
        assert_eq!(safe_id("???"), "voice");
        assert!(valid_id("My_Voice-2"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
        assert!(!valid_id("a/b"));
    }

    #[test]
    fn remove_rejects_paths() {
        let dir = std::env::temp_dir();
        assert!(remove(&dir, "..").is_err());
        assert!(remove(&dir, "a\\b").is_err());
    }
}
