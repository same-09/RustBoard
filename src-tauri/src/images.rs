//! Sound pictures: read an image file and hand it to the UI as a data URL.
//! No extra crate: the base64 encoder is below.

use std::fs;
use std::path::Path;

/// Biggest picture we accept (the UI keeps it in memory).
const MAX_BYTES: u64 = 8 * 1024 * 1024;

fn mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// png, jpg, webp, gif or bmp.
pub fn is_picture(path: &Path) -> bool {
    mime(path).is_some()
}

pub fn data_url(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let mime = mime(p).ok_or("This is not a picture (use png, jpg, webp, gif or bmp).")?;
    let size = fs::metadata(p).map_err(|e| format!("Cannot open '{path}': {e}"))?.len();
    if size > MAX_BYTES {
        return Err("This picture is too big (8 MB at most).".into());
    }
    let bytes = fs::read(p).map_err(|e| format!("Cannot read '{path}': {e}"))?;
    Ok(format!("data:{mime};base64,{}", base64(&bytes)))
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn only_pictures_are_read() {
        assert_eq!(mime(Path::new("a/b.PNG")), Some("image/png"));
        assert_eq!(mime(Path::new("cover.jpeg")), Some("image/jpeg"));
        assert_eq!(mime(Path::new("secret.txt")), None);
        assert!(data_url("C:/Windows/win.ini").is_err());
    }
}
