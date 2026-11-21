//! The `.rbpack` file container. No extra crate.
//!
//! ```text
//! "RBPACK" 0x01 '\n'     8 bytes, tells the file kind
//! manifest length        4 bytes, little endian
//! manifest               JSON text (what is inside, see pack.rs)
//! file 0, file 1, ...    raw bytes, one after the other; sizes are in the manifest
//! ```

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"RBPACK\x01\n";
/// A manifest bigger than this is not ours (a preset is a few KB).
pub const MAX_MANIFEST: u32 = 8 * 1024 * 1024;

/// Write `manifest`, then every file of `files` (path, expected size). Goes to a temp file first,
/// so a failed export never leaves half a pack behind. A file that changed size while we
/// copy it makes the export fail.
pub fn write(dest: &Path, manifest: &[u8], files: &[(&Path, u64)]) -> Result<(), String> {
    if manifest.len() as u64 > MAX_MANIFEST as u64 {
        return Err("The preset is too big.".into());
    }
    let tmp = dest.with_extension("rbpack.tmp");
    let result = (|| -> io::Result<()> {
        let mut out = io::BufWriter::new(File::create(&tmp)?);
        out.write_all(MAGIC)?;
        out.write_all(&(manifest.len() as u32).to_le_bytes())?;
        out.write_all(manifest)?;
        for (path, size) in files {
            let mut src = File::open(path)?;
            let copied = io::copy(&mut (&mut src).take(*size), &mut out)?;
            let mut extra = [0u8; 1];
            if copied != *size || src.read(&mut extra)? != 0 {
                return Err(io::Error::new(io::ErrorKind::Other, format!("'{}' changed while saving", path.display())));
            }
        }
        out.flush()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Cannot write the pack: {e}"));
    }
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot save '{}': {e}", dest.display())
    })
}


/// A file name that is safe to create: no folders, no characters Windows refuses,
/// no trailing dots or spaces, not a reserved device name. Never empty.
pub fn safe_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = last
        .chars()
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    s = s.trim_matches(|c| c == '.' || c == ' ').to_string();
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if s.is_empty() || reserved.contains(&stem.as_str()) {
        s = format!("file_{s}");
    }
    if s.chars().count() > 120 {
        // keep the extension when cutting
        let ext: String = s.rsplit('.').next().filter(|e| e.len() <= 5 && s.contains('.')).map(|e| format!(".{e}")).unwrap_or_default();
        let keep: String = s.chars().take(100).collect();
        s = format!("{keep}{ext}");
    }
    s
}

/// `name` with the `.rbpack` ending.
pub fn with_ext(path: &str) -> String {
    if path.to_ascii_lowercase().ends_with(".rbpack") { path.to_string() } else { format!("{path}.rbpack") }
}

/// An opened pack: the manifest text and where the files start.
pub struct Pack {
    file: File,
    pub manifest: Vec<u8>,
    data_start: u64,
    len: u64,
}

impl Pack {
    pub fn open(path: &Path) -> Result<Pack, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open '{}': {e}", path.display()))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| "This is not a RustBoard pack.".to_string())?;
        if &head[..8] != MAGIC {
            return Err("This is not a RustBoard pack (.rbpack).".into());
        }
        let n = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        if n > MAX_MANIFEST || 12 + n as u64 > len {
            return Err("This pack is damaged.".into());
        }
        let mut manifest = vec![0u8; n as usize];
        file.read_exact(&mut manifest).map_err(|_| "This pack is damaged.".to_string())?;
        Ok(Pack { file, manifest, data_start: 12 + n as u64, len })
    }

    /// The files must fit in what is left of the pack.
    pub fn check_sizes(&self, sizes: &[u64]) -> Result<(), String> {
        let mut total = 0u64;
        for s in sizes {
            total = total.checked_add(*s).ok_or("This pack is damaged.")?;
        }
        if self.data_start.checked_add(total).map(|end| end > self.len).unwrap_or(true) {
            return Err("This pack is cut short (the download may be broken).".into());
        }
        Ok(())
    }

    /// Copy file number `index` to `dest`. `sizes` = sizes of all files, in order.
    pub fn extract(&mut self, sizes: &[u64], index: usize, dest: &Path) -> Result<(), String> {
        let size = *sizes.get(index).ok_or("This pack is damaged.")?;
        let skip: u64 = sizes[..index].iter().sum();
        self.file.seek(SeekFrom::Start(self.data_start + skip)).map_err(|e| e.to_string())?;
        let mut out = File::create(dest).map_err(|e| format!("Cannot write '{}': {e}", dest.display()))?;
        let copied = io::copy(&mut (&mut self.file).take(size), &mut out).map_err(|e| e.to_string())?;
        if copied != size {
            let _ = fs::remove_file(dest);
            return Err("This pack is cut short (the download may be broken).".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rb-pack-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let d = temp("rt");
        fs::write(d.join("a.bin"), b"AAAA").unwrap();
        fs::write(d.join("b.bin"), b"").unwrap();
        fs::write(d.join("c.bin"), b"CC").unwrap();
        let pack = d.join("x.rbpack");
        let (a, b, c) = (d.join("a.bin"), d.join("b.bin"), d.join("c.bin"));
        write(&pack, b"{\"hi\":1}", &[(&a, 4), (&b, 0), (&c, 2)]).unwrap();
        assert!(!pack.with_extension("rbpack.tmp").exists());
        let mut p = Pack::open(&pack).unwrap();
        assert_eq!(p.manifest, b"{\"hi\":1}");
        let sizes = [4u64, 0, 2];
        p.check_sizes(&sizes).unwrap();
        p.extract(&sizes, 2, &d.join("o2")).unwrap();
        p.extract(&sizes, 0, &d.join("o0")).unwrap();
        p.extract(&sizes, 1, &d.join("o1")).unwrap();
        assert_eq!(fs::read(d.join("o0")).unwrap(), b"AAAA");
        assert_eq!(fs::read(d.join("o1")).unwrap(), b"");
        assert_eq!(fs::read(d.join("o2")).unwrap(), b"CC");
        assert!(p.check_sizes(&[4, 0, 3]).is_err(), "claims more than the file holds");
        assert!(p.check_sizes(&[u64::MAX, 2]).is_err(), "overflow");
        assert!(p.extract(&sizes, 3, &d.join("o3")).is_err());
        let _ = fs::remove_dir_all(&d);
    }


    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("boom.mp3"), "boom.mp3");
        assert_eq!(safe_name("..\\..\\Windows\\win.ini"), "win.ini");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("a<b>c:d.mp3"), "a_b_c_d.mp3");
        assert_eq!(safe_name(""), "file_");
        assert_eq!(safe_name(".."), "file_");
        assert_eq!(safe_name("C:\\x\\"), "file_");
        assert_eq!(safe_name("con.mp3"), "file_con.mp3");
        assert_eq!(safe_name("NUL"), "file_NUL");
        assert_eq!(safe_name("end. "), "end");
        let long = format!("{}.mp3", "x".repeat(300));
        let n = safe_name(&long);
        assert!(n.chars().count() < 110 && n.ends_with(".mp3"), "{n}");
        assert_eq!(with_ext("a/b"), "a/b.rbpack");
        assert_eq!(with_ext("a/b.RBPACK"), "a/b.RBPACK");
    }

    #[test]
    fn rejects_other_files_and_cut_packs() {
        let d = temp("bad");
        fs::write(d.join("t.txt"), b"hello world, not a pack").unwrap();
        assert!(Pack::open(&d.join("t.txt")).is_err());
        fs::write(d.join("e"), b"").unwrap();
        assert!(Pack::open(&d.join("e")).is_err());
        assert!(Pack::open(&d.join("missing")).is_err());
        // manifest length lies
        let mut lie = MAGIC.to_vec();
        lie.extend_from_slice(&1000u32.to_le_bytes());
        lie.extend_from_slice(b"{}");
        fs::write(d.join("lie"), &lie).unwrap();
        assert!(Pack::open(&d.join("lie")).is_err());
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        fs::write(d.join("huge"), &huge).unwrap();
        assert!(Pack::open(&d.join("huge")).is_err());
        // a real pack with its tail cut off
        fs::write(d.join("a.bin"), b"AAAAAAAA").unwrap();
        let a = d.join("a.bin");
        write(&d.join("ok.rbpack"), b"{}", &[(&a, 8)]).unwrap();
        let bytes = fs::read(d.join("ok.rbpack")).unwrap();
        fs::write(d.join("cut.rbpack"), &bytes[..bytes.len() - 3]).unwrap();
        let p = Pack::open(&d.join("cut.rbpack")).unwrap();
        assert!(p.check_sizes(&[8]).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_that_changes_size_fails_and_leaves_nothing() {
        let d = temp("chg");
        fs::write(d.join("a.bin"), b"AAAA").unwrap();
        let a = d.join("a.bin");
        let pack = d.join("x.rbpack");
        assert!(write(&pack, b"{}", &[(&a, 3)]).is_err()); // longer than promised
        assert!(write(&pack, b"{}", &[(&a, 9)]).is_err()); // shorter than promised
        assert!(!pack.exists());
        assert!(!pack.with_extension("rbpack.tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
