//! The Soundboard folder: every sound file you add is copied into
//! `Documents/RustBoard/Soundboard/`, so a sound keeps working when the original file is
//! moved or deleted. Also reads a whole folder at once ("Import folder").

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Name of the folder that holds the copies.
pub const FOLDER: &str = "Soundboard";
const AUDIO_EXT: [&str; 8] = ["mp3", "wav", "ogg", "flac", "m4a", "aac", "aiff", "aif"];
/// A folder import stops here, so a wrong click on a huge drive does not run for minutes.
const MAX_FILES: usize = 500;
/// How deep "Import folder" looks into sub-folders (0 = only the folder itself).
const MAX_DEPTH: usize = 2;

#[derive(Clone, Debug, Serialize)]
pub struct Imported {
    /// where the copy lives now (this goes into the config)
    pub path: String,
    /// file name without the extension, used as the sound name
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportOut {
    pub files: Vec<Imported>,
    /// not audio, not readable, or could not be copied
    pub skipped: u32,
    /// the Soundboard folder the copies went to
    pub folder: String,
}

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Audio files in `folder` and in its sub-folders (up to `MAX_DEPTH`), sorted by path.
pub fn scan(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    walk(folder, 0, &mut out)?;
    out.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    Ok(out)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("Cannot read folder '{}': {}", dir.display(), e))?;
    for entry in rd.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        let p = entry.path();
        if p.is_dir() {
            if depth < MAX_DEPTH {
                let _ = walk(&p, depth + 1, out); // a sub-folder we cannot read is skipped
            }
        } else if is_audio(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => match (fs::read(a), fs::read(b)) {
            (Ok(p), Ok(q)) => p == q,
            _ => false,
        },
        _ => false,
    }
}

/// Where the copy of `src` goes. Same name and same content = reuse that file (no second copy).
/// Same name, other content = "name (2).mp3". The bool says whether a copy is still needed.
fn target_for(dir: &Path, src: &Path) -> Result<(PathBuf, bool), String> {
    let name = src.file_name().ok_or("bad file name")?;
    let first = dir.join(name);
    if !first.exists() {
        return Ok((first, true));
    }
    if same_file(src, &first) {
        return Ok((first, false));
    }
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("sound");
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 2..1000 {
        let file = if ext.is_empty() { format!("{stem} ({n})") } else { format!("{stem} ({n}).{ext}") };
        let cand = dir.join(file);
        if !cand.exists() {
            return Ok((cand, true));
        }
        if same_file(src, &cand) {
            return Ok((cand, false));
        }
    }
    Err(format!("Too many files named '{stem}'."))
}

/// Copy the audio files into the Soundboard folder. Files already inside it stay where they are.
/// One bad file does not stop the rest: it is counted in `skipped`.
pub fn import(dir: &Path, paths: &[PathBuf]) -> Result<ImportOut, String> {
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create '{}': {}", dir.display(), e))?;
    let mut out = ImportOut { folder: dir.display().to_string(), ..Default::default() };
    for src in paths {
        if !src.is_file() || !is_audio(src) {
            out.skipped += 1;
            continue;
        }
        let target = if src.parent() == Some(dir) {
            src.clone()
        } else {
            match target_for(dir, src) {
                Ok((t, needs_copy)) => {
                    if needs_copy && fs::copy(src, &t).is_err() {
                        out.skipped += 1;
                        continue;
                    }
                    t
                }
                Err(_) => {
                    out.skipped += 1;
                    continue;
                }
            }
        };
        let name = target.file_stem().and_then(|s| s.to_str()).unwrap_or("sound").to_string();
        out.files.push(Imported { path: target.display().to_string(), name });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustboard-lib-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn audio_extensions() {
        assert!(is_audio(Path::new("a.MP3")));
        assert!(is_audio(Path::new("x/y.flac")));
        assert!(!is_audio(Path::new("notes.txt")));
        assert!(!is_audio(Path::new("noext")));
    }

    #[test]
    fn scan_finds_audio_in_sub_folders_only_to_the_depth_limit() {
        let root = temp("scan");
        fs::write(root.join("b.mp3"), b"1").unwrap();
        fs::write(root.join("a.wav"), b"2").unwrap();
        fs::write(root.join("readme.txt"), b"3").unwrap();
        let deep = root.join("one").join("two").join("three");
        fs::create_dir_all(&deep).unwrap();
        fs::write(root.join("one").join("c.ogg"), b"4").unwrap();
        fs::write(deep.join("too_deep.mp3"), b"5").unwrap();
        let names: Vec<String> = scan(&root)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.wav", "b.mp3", "c.ogg"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn import_copies_renames_and_skips() {
        let src = temp("src");
        let dst = temp("dst").join(FOLDER);
        fs::write(src.join("boom.mp3"), b"AAAA").unwrap();
        fs::write(src.join("notes.txt"), b"x").unwrap();
        let a = import(&dst, &[src.join("boom.mp3"), src.join("notes.txt")]).unwrap();
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.skipped, 1);
        assert_eq!(a.files[0].name, "boom");
        assert!(dst.join("boom.mp3").is_file());

        // same file again: no second copy
        let b = import(&dst, &[src.join("boom.mp3")]).unwrap();
        assert_eq!(b.files[0].path, a.files[0].path);

        // other file with the same name: gets "(2)"
        let other = temp("other");
        fs::write(other.join("boom.mp3"), b"BBBB").unwrap();
        let c = import(&dst, &[other.join("boom.mp3")]).unwrap();
        assert!(c.files[0].path.ends_with("boom (2).mp3"), "{}", c.files[0].path);
        assert_eq!(fs::read(dst.join("boom.mp3")).unwrap(), b"AAAA"); // the first one is untouched

        // a file that is already in the Soundboard folder stays where it is
        let d = import(&dst, &[dst.join("boom.mp3")]).unwrap();
        assert_eq!(d.files[0].path, a.files[0].path);
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 2);
    }
}
