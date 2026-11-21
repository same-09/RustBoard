//! Text-to-speech: type text, the PC reads it out loud. Uses the voices Windows already has
//! (SAPI, through PowerShell), so there is nothing to download and no graphics card is needed.
//! The speech becomes a small .wav file that is played like a sound, so it goes to the same
//! places as the soundboard (headphones, and Discord when Go Live is on).

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Longest text we read at once.
pub const MAX_CHARS: usize = 1000;
pub const RATE_MIN: i32 = -10;
pub const RATE_MAX: i32 = 10;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Voice {
    pub name: String,
    /// e.g. "en-US", "id-ID"
    pub lang: String,
}

/// The text goes in through environment variables, never into the script itself,
/// so nothing a user types can run as a command.
const SPEAK_PS: &str = "$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
if ($env:RB_TTS_VOICE) { try { $s.SelectVoice($env:RB_TTS_VOICE) } catch { } }
$s.Rate = [int]$env:RB_TTS_RATE
$s.SetOutputToWaveFile($env:RB_TTS_OUT)
$s.Speak($env:RB_TTS_TEXT)
$s.Dispose()
";

const LIST_PS: &str = "$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$s.GetInstalledVoices() | Where-Object { $_.Enabled } | ForEach-Object { $_.VoiceInfo.Name + '|' + $_.VoiceInfo.Culture.Name }
$s.Dispose()
";

/// Lines "Name|lang" from the voice list script.
pub fn parse_voices(out: &str) -> Vec<Voice> {
    out.lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches('\u{feff}');
            let (name, lang) = l.split_once('|')?;
            let name = name.trim();
            (!name.is_empty()).then(|| Voice { name: name.to_string(), lang: lang.trim().to_string() })
        })
        .collect()
}

/// Clean text: trimmed, no control characters, at most `MAX_CHARS`.
pub fn clean_text(text: &str) -> Result<String, String> {
    let t: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = t.trim();
    if t.is_empty() {
        return Err("Type something first.".into());
    }
    if t.chars().count() > MAX_CHARS {
        return Err(format!("Too long: {MAX_CHARS} characters at most."));
    }
    Ok(t.to_string())
}

#[cfg(windows)]
fn powershell(script_name: &str, script: &str, work: &Path, env: &[(&str, &str)]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    fs::create_dir_all(work).map_err(|e| format!("Cannot make '{}': {e}", work.display()))?;
    let file = work.join(script_name);
    fs::write(&file, script).map_err(|e| format!("Cannot write the speech script: {e}"))?;
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&file);
    for (k, v) in env {
        c.env(k, v);
    }
    c.creation_flags(0x0800_0000); // no console window
    let out = c.output().map_err(|e| format!("Cannot start PowerShell: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(format!("Windows speech failed: {first}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(windows))]
fn powershell(_: &str, _: &str, _: &Path, _: &[(&str, &str)]) -> Result<String, String> {
    Err("Text to speech uses the voices of Windows, so it works on Windows only.".into())
}

/// Voices installed in Windows. Starts PowerShell (about a second): call from a worker thread.
pub fn voices(work: &Path) -> Result<Vec<Voice>, String> {
    let out = powershell("tts-voices.ps1", LIST_PS, work, &[])?;
    let v = parse_voices(&out);
    if v.is_empty() {
        return Err("Windows has no speech voices installed.".into());
    }
    Ok(v)
}

/// Turn `text` into a .wav file and return its path. `rate`: -10 (slow) .. 10 (fast).
pub fn synth(work: &Path, text: &str, voice: &str, rate: i32) -> Result<PathBuf, String> {
    let text = clean_text(text)?;
    let rate = rate.clamp(RATE_MIN, RATE_MAX);
    let dir = work.join("tts");
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot make '{}': {e}", dir.display()))?;
    // old speech files go (one may still be playing: then Windows refuses and it stays)
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("wav") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let wav = dir.join(format!("speech-{stamp}.wav"));
    let rate_s = rate.to_string();
    let out_s = wav.to_string_lossy().into_owned();
    let env: [(&str, &str); 4] = [
        ("RB_TTS_TEXT", text.as_str()),
        ("RB_TTS_VOICE", voice),
        ("RB_TTS_RATE", rate_s.as_str()),
        ("RB_TTS_OUT", out_s.as_str()),
    ];
    powershell("tts-speak.ps1", SPEAK_PS, work, &env)?;
    match fs::metadata(&wav) {
        Ok(m) if m.len() > 44 => Ok(wav),
        _ => Err("Windows made no sound from this text.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_lines_are_parsed() {
        let v = parse_voices("\u{feff}Microsoft David Desktop|en-US\r\nMicrosoft Andika|id-ID\n\nbroken line\n|xx\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], Voice { name: "Microsoft David Desktop".into(), lang: "en-US".into() });
        assert_eq!(v[1].lang, "id-ID");
    }

    #[test]
    fn text_is_cleaned_and_limited() {
        assert_eq!(clean_text("  halo\ndunia\t!  ").unwrap(), "halo dunia !");
        assert!(clean_text("   \n ").is_err());
        assert!(clean_text(&"a".repeat(MAX_CHARS)).is_ok());
        assert!(clean_text(&"a".repeat(MAX_CHARS + 1)).is_err());
        assert!(clean_text(&"é".repeat(MAX_CHARS)).is_ok(), "limit counts characters, not bytes");
    }

    #[test]
    fn text_never_reaches_the_script() {
        // the scripts are fixed text: only environment variables carry user input
        assert!(!SPEAK_PS.contains("{0}") && SPEAK_PS.contains("$env:RB_TTS_TEXT"));
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_say_windows_only() {
        let d = std::env::temp_dir();
        assert!(voices(&d).unwrap_err().contains("Windows"));
        assert!(synth(&d, "hi", "", 0).unwrap_err().contains("Windows"));
        assert!(synth(&d, " ", "", 0).unwrap_err().contains("Type something"));
    }
}
