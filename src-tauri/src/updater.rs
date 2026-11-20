//! In-app update without a signing key.
//! UI gives the URL of `Rustboard_x.y.z_x64-setup.exe` from our own GitHub Release.
//! We download it to a temp folder (progress events), check SHA-256 when GitHub gave one,
//! start the installer and quit, so the installer can replace the files.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use tauri::{AppHandle, Emitter};

const PREFIX: &str = "https://github.com/same-09/RustBoard/releases/download/";

/// Only a setup .exe from our own Releases. No spaces, quotes or path tricks.
pub fn allowed(url: &str) -> bool {
    url.starts_with(PREFIX)
        && url.ends_with("-setup.exe")
        && !url.contains("..")
        && !url.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'')
}

/// "sha256:abcd..." (as the GitHub API writes it) or bare hex -> lowercase hex.
fn clean_digest(d: &str) -> Option<String> {
    let hex = d.strip_prefix("sha256:").unwrap_or(d).trim().to_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

fn download(app: &AppHandle, url: &str, digest: Option<String>) -> Result<std::path::PathBuf, String> {
    let resp = ureq::get(url).call().map_err(|e| format!("Download failed: {e}"))?;
    let total: u64 = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);

    let path = std::env::temp_dir().join("Rustboard-update-setup.exe");
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut hasher = Sha256::new();
    let (mut buf, mut done, mut last) = ([0u8; 64 * 1024], 0u64, 0u64);

    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if done - last >= 256 * 1024 || done == total {
            last = done;
            let _ = app.emit("update-progress", serde_json::json!({ "done": done, "total": total }));
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    if let Some(want) = digest {
        let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != want {
            let _ = std::fs::remove_file(&path);
            return Err("The download is damaged (checksum does not match). Try again.".into());
        }
    }
    Ok(path)
}

/// Runs in a thread. Success: installer starts and the app exits. Failure: "update-error" event.
pub fn run(app: AppHandle, url: String, digest: Option<String>) -> Result<(), String> {
    if !allowed(&url) {
        return Err("This update link is not allowed.".into());
    }
    let digest = digest.as_deref().and_then(clean_digest);

    std::thread::spawn(move || {
        let result = download(&app, &url, digest).and_then(|path| {
            // /P = passive (progress only), /R = start Rustboard again when done (Tauri NSIS flags)
            std::process::Command::new(&path)
                .args(["/P", "/R"])
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("Cannot start the installer: {e}"))
        });
        match result {
            Ok(()) => app.exit(0),
            Err(e) => {
                let _ = app.emit("update-error", e);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{allowed, clean_digest};

    #[test]
    fn only_our_setup_exe() {
        assert!(allowed("https://github.com/same-09/RustBoard/releases/download/v0.0.2/Rustboard_0.0.2_x64-setup.exe"));
        assert!(!allowed("https://github.com/same-09/RustBoard/releases/download/v0.0.2/other.exe"));
        assert!(!allowed("https://evil.example/Rustboard_0.0.2_x64-setup.exe"));
        assert!(!allowed("https://github.com/other/RustBoard/releases/download/v1/a-setup.exe"));
        assert!(!allowed("https://github.com/same-09/RustBoard/releases/download/../x-setup.exe"));
    }

    #[test]
    fn digest_forms() {
        let h = "a".repeat(64);
        assert_eq!(clean_digest(&format!("sha256:{h}")), Some(h.clone()));
        assert_eq!(clean_digest(&h.to_uppercase()), Some(h));
        assert_eq!(clean_digest("nope"), None);
    }
}
