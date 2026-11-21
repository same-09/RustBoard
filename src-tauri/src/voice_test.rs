//! "Test voice" for AI Voice: record a few seconds from the mic, run them through the chosen
//! voice, and keep both clips so the user can hear "my voice" and "new voice" back to back.
//! It also times the converter. That is the honest answer to "is my PC fast enough for live?":
//! converting 4 seconds of audio must take clearly less than 4 seconds.
//!
//! The clips are plain .wav files in the temp folder and are played with the normal sound
//! player, so they come out of the Primary output like any sound.

use crate::audio::{self, DEFAULT_IN};
use crate::config::DISABLED;
use rodio::cpal::traits::{DeviceTrait, StreamTrait};
use rodio::cpal::{self, SampleFormat};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the test listens.
pub const SECONDS: u32 = 4;
const ORIGINAL: &str = "my-voice.wav";
const CONVERTED: &str = "new-voice.wav";
/// Below this peak (about -50 dB) we call the mic silent.
const SILENT_PEAK: f32 = 0.003;

/// The "Cancel" button sets this. `run` clears it when a new test starts.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Stop the test that is running (recording or converting). Does nothing when none runs.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct TestOut {
    /// the recording was saved (always true when this struct is returned)
    pub original: bool,
    /// the converted clip was saved
    pub converted: bool,
    /// why the voice could not be converted (the recording still works)
    pub error: Option<String>,
    pub recorded_secs: f32,
    pub convert_secs: f32,
    /// convert_secs / recorded_secs. Under 1.0 the PC keeps up; 0 when nothing was converted.
    pub speed: f32,
    /// "fast", "tight", "slow" or "" (nothing converted)
    pub verdict: &'static str,
    pub peak_db: f32,
}

/// Where the clips live. Only these two names exist, the UI never sends a path.
fn dir() -> PathBuf {
    std::env::temp_dir().join("rustboard-ai-test")
}

pub fn clip_path(which: &str) -> Option<PathBuf> {
    match which {
        "original" => Some(dir().join(ORIGINAL)),
        "converted" => Some(dir().join(CONVERTED)),
        _ => None,
    }
}

/// Fast enough for live? Chunks must be converted faster than they are spoken, with room to spare.
pub fn verdict(speed: f32) -> &'static str {
    if speed <= 0.0 {
        ""
    } else if speed < 0.5 {
        "fast"
    } else if speed < 0.9 {
        "tight"
    } else {
        "slow"
    }
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn to_db(x: f32) -> f32 {
    if x <= 1e-5 {
        -100.0
    } else {
        20.0 * x.log10()
    }
}

/// 16-bit mono PCM .wav, 44-byte header.
pub fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    v.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
    v.extend_from_slice(&16u16.to_le_bytes()); // bits
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let x = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

/// Mix interleaved frames down to mono and append them.
fn mix(buf: &Arc<Mutex<Vec<f32>>>, samples: impl Iterator<Item = f32>, channels: usize) {
    let ch = channels.max(1);
    let (mut sum, mut n) = (0.0f32, 0usize);
    if let Ok(mut b) = buf.lock() {
        for s in samples {
            sum += s;
            n += 1;
            if n == ch {
                b.push(sum / ch as f32);
                sum = 0.0;
                n = 0;
            }
        }
    }
}

/// Listen to the mic for `seconds`. Returns mono samples and their sample rate.
pub fn record(input_name: &str, seconds: u32) -> Result<(Vec<f32>, u32), String> {
    let host = cpal::default_host();
    let dev = audio::find_input(&host, input_name)
        .ok_or_else(|| format!("Mic '{input_name}' not found. Pick another one in Settings."))?;
    let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;
    let format = cfg.sample_format();
    let conf: cpal::StreamConfig = cfg.into();
    let want = rate as usize * seconds as usize;
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(want)));

    let on_err = |e: cpal::StreamError| eprintln!("test recording error: {e}");
    let stream = match format {
        SampleFormat::F32 => {
            let b = Arc::clone(&buf);
            dev.build_input_stream(
                &conf,
                move |d: &[f32], _: &cpal::InputCallbackInfo| mix(&b, d.iter().copied(), channels),
                on_err,
                None,
            )
        }
        SampleFormat::I16 => {
            let b = Arc::clone(&buf);
            dev.build_input_stream(
                &conf,
                move |d: &[i16], _: &cpal::InputCallbackInfo| {
                    mix(&b, d.iter().map(|s| *s as f32 / 32768.0), channels)
                },
                on_err,
                None,
            )
        }
        SampleFormat::U16 => {
            let b = Arc::clone(&buf);
            dev.build_input_stream(
                &conf,
                move |d: &[u16], _: &cpal::InputCallbackInfo| {
                    mix(&b, d.iter().map(|s| (*s as f32 - 32768.0) / 32768.0), channels)
                },
                on_err,
                None,
            )
        }
        other => return Err(format!("This mic uses a sample format ({other:?}) the test cannot read.")),
    }
    .map_err(|e| format!("Cannot open the mic: {e}"))?;

    stream.play().map_err(|e| format!("Cannot start the mic: {e}"))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(seconds as u64) {
        if CANCEL.load(Ordering::Relaxed) {
            return Err(crate::voice_ai::CANCELLED.to_string()); // `stream` is dropped here: the mic closes
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream);

    let mut samples = buf.lock().map(|b| b.clone()).unwrap_or_default();
    if samples.is_empty() {
        return Err("The mic gave no audio. Check that Windows lets apps use the microphone (Settings > Privacy > Microphone).".into());
    }
    samples.truncate(want);
    Ok((samples, rate))
}

/// The whole test: record, save, convert, time it, save the result.
/// Takes `SECONDS` plus the conversion time: call it from a worker thread.
pub fn run(engine: &crate::voice_ai::Engine, voice_dir: &Path, input_name: &str) -> Result<TestOut, String> {
    CANCEL.store(false, Ordering::Relaxed);
    let out_dir = dir();
    fs::create_dir_all(&out_dir).map_err(|e| format!("Cannot make the temp folder: {e}"))?;
    let _ = fs::remove_file(out_dir.join(ORIGINAL));
    let _ = fs::remove_file(out_dir.join(CONVERTED));

    let mic = if input_name == DISABLED { DEFAULT_IN } else { input_name };
    let (samples, rate) = record(mic, SECONDS)?;
    let p = peak(&samples);
    if p < SILENT_PEAK {
        return Err("The mic is silent. Speak louder, or pick the right mic in Settings.".into());
    }
    fs::write(out_dir.join(ORIGINAL), wav_bytes(&samples, rate)).map_err(|e| format!("Cannot save the recording: {e}"))?;
    let recorded_secs = samples.len() as f32 / rate as f32;

    let t0 = Instant::now();
    let result = crate::voice_ai::convert(engine, voice_dir, &samples, rate, &CANCEL);
    let convert_secs = t0.elapsed().as_secs_f32();

    let mut out = TestOut {
        original: true,
        converted: false,
        error: None,
        recorded_secs,
        convert_secs,
        speed: 0.0,
        verdict: "",
        peak_db: to_db(p),
    };
    match result {
        Ok(c) => {
            fs::write(out_dir.join(CONVERTED), wav_bytes(&c.samples, c.rate)).map_err(|e| format!("Cannot save the new voice: {e}"))?;
            out.converted = true;
            out.speed = convert_secs / recorded_secs.max(0.01);
            out.verdict = verdict(out.speed);
        }
        Err(e) if e == crate::voice_ai::CANCELLED => return Err(e),
        Err(e) => out.error = Some(e),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_samples() {
        let w = wav_bytes(&[0.0, 1.0, -1.0, 2.0], 16_000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]), 8);
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 16_000);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), 32767); // 1.0
        assert_eq!(i16::from_le_bytes([w[48], w[49]]), -32767); // -1.0
        assert_eq!(i16::from_le_bytes([w[50], w[51]]), 32767); // 2.0 is clamped
    }

    #[test]
    fn verdict_steps() {
        assert_eq!(verdict(0.0), "");
        assert_eq!(verdict(0.3), "fast");
        assert_eq!(verdict(0.7), "tight");
        assert_eq!(verdict(1.5), "slow");
    }

    #[test]
    fn stereo_is_mixed_to_mono() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        mix(&buf, [1.0f32, 0.0, 0.5, 0.5].into_iter(), 2);
        assert_eq!(*buf.lock().unwrap(), vec![0.5, 0.5]);
    }

    #[test]
    fn peak_and_db() {
        assert_eq!(peak(&[0.1, -0.4, 0.2]), 0.4);
        assert_eq!(to_db(0.0), -100.0);
        assert!(to_db(1.0).abs() < 1e-6);
    }

    #[test]
    fn only_two_clip_names() {
        assert!(clip_path("original").is_some());
        assert!(clip_path("converted").is_some());
        assert!(clip_path("../evil").is_none());
    }
}
