//! Text-to-speech: type text, the PC reads it out loud. Uses the voices Windows already has
//! (SAPI, through PowerShell), so there is nothing to download and no graphics card is needed.
//! The speech becomes a small .wav file that is played like a sound, so it goes to the same
//! places as the soundboard (headphones, and Discord when Go Live is on).

use crate::tts_util;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// A fresh .wav path in `Documents/RustBoard/tts`. Old speech files are deleted
/// (one may still be playing: then Windows refuses and it stays).
fn new_wav_path(work: &Path) -> Result<PathBuf, String> {
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
    Ok(dir.join(format!("speech-{stamp}.wav")))
}

/// Turn `text` into a .wav file and return its path. `rate`: -10 (slow) .. 10 (fast).
pub fn synth(work: &Path, text: &str, voice: &str, rate: i32) -> Result<PathBuf, String> {
    let text = clean_text(text)?;
    let rate = rate.clamp(RATE_MIN, RATE_MAX);
    let wav = new_wav_path(work)?;
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

/* ---------- Google Cloud Text-to-Speech (the official one, needs an API key) ---------- */

const GOOGLE: &str = "https://texttospeech.googleapis.com/v1";

#[derive(Deserialize)]
struct GVoices {
    #[serde(default)]
    voices: Vec<GVoice>,
}

#[derive(Deserialize)]
struct GVoice {
    name: String,
    #[serde(default, rename = "languageCodes")]
    language_codes: Vec<String>,
}

#[derive(Deserialize)]
struct GSynth {
    #[serde(rename = "audioContent")]
    audio_content: String,
}

#[derive(Deserialize)]
struct GErr {
    error: Option<GErrIn>,
}

#[derive(Deserialize)]
struct GErrIn {
    message: Option<String>,
}

/// An API key is letters, digits, "-" and "_". Anything else is refused, so a pasted
/// key can never change the address we call.
pub fn check_key(key: &str) -> Result<&str, String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("Paste your Google API key first.".into());
    }
    if k.len() > 200 || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("This does not look like a Google API key.".into());
    }
    Ok(k)
}

fn read_body(r: ureq::Response) -> Result<String, String> {
    let mut s = String::new();
    r.into_reader()
        .take(40 * 1024 * 1024)
        .read_to_string(&mut s)
        .map_err(|e| format!("Cannot read the answer of Google: {e}"))?;
    Ok(s)
}

/// One call to Google. Errors never contain the address (the key is in it).
fn google_call(path: &str, key: &str, body: Option<&str>) -> Result<String, String> {
    let url = format!("{GOOGLE}/{path}?key={key}");
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
    let res = match body {
        Some(b) => agent.post(&url).set("content-type", "application/json").send_string(b),
        None => agent.get(&url).call(),
    };
    match res {
        Ok(r) => read_body(r),
        Err(ureq::Error::Status(code, r)) => {
            let text = read_body(r).unwrap_or_default();
            let msg = serde_json::from_str::<GErr>(&text).ok().and_then(|e| e.error).and_then(|e| e.message).unwrap_or_default();
            Err(tts_util::explain(code, &msg))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("Cannot reach Google ({:?}). Check your internet.", t.kind())),
    }
}

/// The voices Google offers for this key (also a way to check that the key works).
pub fn google_voices(key: &str) -> Result<Vec<Voice>, String> {
    let key = check_key(key)?;
    let text = google_call("voices", key, None)?;
    let list: GVoices = serde_json::from_str(&text).map_err(|_| "Google sent an answer we do not understand.".to_string())?;
    let mut v: Vec<Voice> = list
        .voices
        .into_iter()
        .map(|g| {
            let lang = g.language_codes.first().cloned().or_else(|| tts_util::lang_of(&g.name)).unwrap_or_default();
            Voice { name: g.name, lang }
        })
        .collect();
    // Indonesian first, then English, then the rest
    v.sort_by_key(|x| (!x.lang.starts_with("id"), !x.lang.starts_with("en"), x.lang.clone(), x.name.clone()));
    Ok(v)
}

/// Turn `text` into a .wav file with a Google voice and return its path.
pub fn google_synth(work: &Path, key: &str, voice: &str, text: &str, rate: i32) -> Result<PathBuf, String> {
    let key = check_key(key)?;
    let text = clean_text(text)?;
    let body = tts_util::synth_body(&text, voice, rate)?;
    let answer = google_call("text:synthesize", key, Some(&body))?;
    let g: GSynth = serde_json::from_str(&answer).map_err(|_| "Google sent an answer we do not understand.".to_string())?;
    let audio = tts_util::base64_decode(&g.audio_content).ok_or("Google sent broken audio.")?;
    if audio.len() < 44 || &audio[..4] != b"RIFF" {
        return Err("Google sent audio in a format we cannot play.".into());
    }
    let wav = new_wav_path(work)?;
    fs::write(&wav, audio).map_err(|e| format!("Cannot save the speech: {e}"))?;
    Ok(wav)
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
    fn only_real_looking_keys_are_used() {
        assert_eq!(check_key("  AIzaSyA-b_c123 ").unwrap(), "AIzaSyA-b_c123");
        assert!(check_key("").is_err());
        assert!(check_key("   ").is_err());
        assert!(check_key("abc&voice=x").is_err());
        assert!(check_key("a b").is_err());
        assert!(check_key("abc/../x").is_err());
        assert!(check_key(&"a".repeat(201)).is_err());
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
