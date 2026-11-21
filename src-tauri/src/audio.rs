//! Audio engine. Runs on its own thread (cpal streams are not Send).
//! Port of AudioPlaybackEngine, MicPassthrough, SoundManager.

use crate::config::{Config, AUTO_CABLE, DEFAULT_OUT, DISABLED};
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

pub const DEFAULT_IN: &str = "Default Input Device";

// ---- ducking + limiter ----
/// Ducking: while you speak, sounds on the mic output drop to this gain (about -10 dB).
const DUCK_GAIN: f32 = 0.3;
/// How fast the sound goes down when you start to speak, and comes back when you stop.
const DUCK_ATTACK_S: f32 = 0.04;
const DUCK_RELEASE_S: f32 = 0.45;
/// Your mic counts as "speaking" above this level (linear RMS, after the gate) ...
const SPEAK_THRESHOLD: f32 = 0.015;
/// ... and a short pause between words does not end it.
const SPEAK_HOLD_MS: u128 = 400;
/// Limiter ceilings. Sounds and mic are mixed by the virtual cable, so the two ceilings
/// together stay near 1.0: a loud sound plus a loud voice should not clip.
const SOUND_CEILING: f32 = 0.95;
const MIC_CEILING: f32 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSettings {
    pub volume: f32,
    pub primary_output: String,
    pub primary_volume: f32,
    pub secondary_output: String,
    pub secondary_volume: f32,
    pub overlap: bool,
    pub mic_input: String,
    pub mic_output: String,
    pub mic_volume: f32,
    pub live_output: String,
    pub gate_on: bool,
    /// gate opens above this mic level (linear RMS, 0..1)
    pub gate_threshold: f32,
    /// mic sound effect: 0 = off, else the preset index in `fx::IDS`
    pub fx: u32,
    /// sounds get quieter while you speak (on the output that carries your mic)
    pub duck_on: bool,
    /// stops loud peaks (sounds and mic) from clipping
    pub limiter_on: bool,
    /// AI Voice wanted right now (on, and the voice exists). The mic goes through the engine.
    pub ai: Option<crate::voice_ai::Plan>,
}

impl AudioSettings {
    pub fn from_config(c: &Config) -> Self {
        AudioSettings {
            volume: c.volume as f32 / 100.0,
            primary_output: c.primary_output.clone(),
            primary_volume: c.primary_device_volume as f32 / 100.0,
            secondary_output: c.secondary_output.clone(),
            secondary_volume: c.secondary_device_volume as f32 / 100.0,
            overlap: c.overlap,
            mic_input: c.mic_input.clone(),
            mic_output: c.mic_output.clone(),
            mic_volume: c.mic_volume as f32 / 100.0,
            live_output: c.live_output.clone(),
            gate_on: c.gate_on,
            gate_threshold: 10f32.powf(c.gate_threshold_db as f32 / 20.0),
            fx: crate::fx::active_index(c.fx_on, &c.fx_preset),
            duck_on: c.duck_on,
            limiter_on: c.limiter_on,
            ai: None, // main.rs fills it in: it knows the RustBoard folder
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Devices {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
    /// The virtual cable that "Auto" would pick right now (None = not installed).
    pub cable: Option<String>,
}

/// What "Go Live" did, for the UI.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LiveStatus {
    pub on: bool,
    /// device that carries sounds + mic to Discord
    pub output: String,
    pub sounds: bool,
    pub mic: bool,
    /// your own headphones (Primary output) are open, so you hear the sounds too
    pub headphones: bool,
}

pub enum AudioCmd {
    Configure(AudioSettings),
    /// `fx` = effect on this sound: 0 = none, else the preset index in `fx::IDS`
    Play { file: String, volume: f32, looped: bool, fx: u32 },
    Pause,
    Resume,
    Stop,
    Devices(Sender<Devices>),
    /// Go Live on/off. The reply says what is running, or why it could not start.
    Live(bool, Sender<Result<LiveStatus, String>>),
}

/// Same curve as the old app: (100^v - 1) / 99
fn curve(v: f32) -> f32 {
    (100f32.powf(v.clamp(0.0, 1.0)) - 1.0) / 99.0
}

// ---------- mic levels + noise gate ----------

/// Mic level meters. The mic callback writes, the UI reads through `get_levels`.
/// Values are the loudest block RMS since the last read. Positive f32 bit patterns sort
/// like the numbers, so `fetch_max` on the bits keeps the maximum without a lock.
#[derive(Default)]
pub struct Levels {
    input: AtomicU32,
    output: AtomicU32,
    active: AtomicBool,
    gate_open: AtomicBool,
    /// your voice is on the mic right now (with a short hold), written by the mic thread
    speaking: AtomicBool,
    /// settings the sound and mic threads read while they run
    duck_on: AtomicBool,
    limiter_on: AtomicBool,
    /// what the soundboard itself plays: loudest RMS per frequency band since the last read
    bands: [AtomicU32; BANDS],
    /// file of the sound that was started last (the UI matches it to a tile)
    sound_file: Mutex<String>,
    /// AI Voice engine: 0 = not used, 1 = loading, 2 = live, 3 = failed
    ai_state: AtomicU32,
    /// the mode it runs in ("Balanced · cuda") or why it failed
    ai_text: Mutex<String>,
}

/// Frequency bands the tiles dance to: bass, low mid, mid, high mid, treble.
pub const BANDS: usize = 5;
/// Band edges (Hz) for the one-pole splitters in `Tap`.
const BAND_EDGES: [f32; BANDS - 1] = [120.0, 400.0, 1200.0, 3600.0];

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoundLevelOut {
    /// file of the sound that was started last
    pub file: String,
    /// linear RMS per band (bass .. treble), loudest since the last call; 0 = silence
    pub bands: [f32; BANDS],
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LevelsOut {
    /// mic level before the gate, dBFS (-100 = silence)
    pub input_db: f32,
    /// level that leaves toward Discord / the cable: after gate and mic volume
    pub output_db: f32,
    /// the mic is running
    pub active: bool,
    pub gate_open: bool,
    /// AI Voice engine: 0 = not used, 1 = loading, 2 = live, 3 = failed
    pub ai_state: u32,
    pub ai_text: String,
}

fn to_db(v: f32) -> f32 {
    if v <= 1e-5 {
        -100.0
    } else {
        20.0 * v.log10()
    }
}

impl Levels {
    fn note(&self, input: f32, output: f32, gate_open: bool) {
        if input.is_finite() {
            self.input.fetch_max(input.to_bits(), Ordering::Relaxed);
        }
        if output.is_finite() {
            self.output.fetch_max(output.to_bits(), Ordering::Relaxed);
        }
        self.gate_open.store(gate_open, Ordering::Relaxed);
    }

    fn note_bands(&self, b: &[f32; BANDS]) {
        for (slot, v) in self.bands.iter().zip(b.iter()) {
            if v.is_finite() && *v > 0.0 {
                slot.fetch_max(v.to_bits(), Ordering::Relaxed);
            }
        }
    }

    fn set_sound_file(&self, file: &str) {
        if let Ok(mut f) = self.sound_file.lock() {
            if f.as_str() != file {
                *f = file.to_string();
            }
        }
    }

    /// Loudest band levels of the soundboard output since the last call, then start over.
    pub fn take_sound(&self) -> SoundLevelOut {
        let mut bands = [0.0f32; BANDS];
        for (out, slot) in bands.iter_mut().zip(self.bands.iter()) {
            *out = f32::from_bits(slot.swap(0, Ordering::Relaxed));
        }
        let file = self.sound_file.lock().map(|f| f.clone()).unwrap_or_default();
        SoundLevelOut { file, bands }
    }

    /// Peak (of block RMS) since the last call, then start over.
    pub fn take(&self) -> LevelsOut {
        LevelsOut {
            input_db: to_db(f32::from_bits(self.input.swap(0, Ordering::Relaxed))),
            output_db: to_db(f32::from_bits(self.output.swap(0, Ordering::Relaxed))),
            active: self.active.load(Ordering::Relaxed),
            gate_open: self.gate_open.load(Ordering::Relaxed),
            ai_state: self.ai_state.load(Ordering::Relaxed),
            ai_text: self.ai_text.lock().map(|t| t.clone()).unwrap_or_default(),
        }
    }

    fn set_ai(&self, s: &crate::voice_ai::AiState) {
        use crate::voice_ai::AiState;
        let (n, text) = match s {
            AiState::Loading => (1, "Loading the voice engine… the first time takes a while.".to_string()),
            AiState::Live(t) => (2, t.clone()),
            AiState::Failed(e) => (3, e.clone()),
        };
        if let Ok(mut t) = self.ai_text.lock() {
            *t = text;
        }
        self.ai_state.store(n, Ordering::Relaxed);
    }

    fn clear_ai(&self) {
        self.ai_state.store(0, Ordering::Relaxed);
    }
}

fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    (block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32).sqrt()
}

/// Noise gate: mutes the mic between words so keyboard and fan noise do not reach Discord.
/// Level is measured per block (about 10 ms). It opens fast (5 ms), stays open 200 ms after the
/// last loud block (no chopped word endings), then fades out (80 ms). Once open it needs the level
/// to fall 4 dB below the threshold before it counts as quiet, so it does not flutter.
struct Gate {
    g: f32,
    open: bool,
    hold_left: usize,
    hold: usize,
    attack: f32,
    release: f32,
}

impl Gate {
    fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f32;
        Gate {
            g: 0.0,
            open: false,
            hold_left: 0,
            hold: (sr * 0.2) as usize,
            attack: 1.0 - (-1.0 / (sr * 0.005)).exp(),
            release: 1.0 - (-1.0 / (sr * 0.08)).exp(),
        }
    }

    /// Gate switched off: pass everything, and be open when it is switched on again.
    fn reset(&mut self) {
        self.g = 1.0;
        self.open = true;
        self.hold_left = self.hold;
    }

    fn is_open(&self) -> bool {
        self.open
    }

    /// `level` = RMS of `block`, `threshold` = linear RMS.
    fn process(&mut self, block: &mut [f32], level: f32, threshold: f32) {
        if block.is_empty() {
            return;
        }
        let limit = if self.open { threshold * 0.63 } else { threshold };
        if level >= limit {
            self.open = true;
            self.hold_left = self.hold;
        } else if self.hold_left > 0 {
            self.hold_left = self.hold_left.saturating_sub(block.len());
        } else {
            self.open = false;
        }
        let target = if self.open { 1.0 } else { 0.0 };
        let k = if target > self.g { self.attack } else { self.release };
        for x in block.iter_mut() {
            self.g += (target - self.g) * k;
            *x *= self.g;
        }
    }
}

// ---------- output ----------

struct Output {
    /// the device name as chosen in Settings (compared with the mic output for ducking)
    name: String,
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sinks: Vec<(Sink, f32)>,
    gain: f32,
    /// extra fixed attenuation (1.0 = none), used while live so sounds + mic fit on the cable
    headroom: f32,
    /// the sounds report their band levels here (for the dancing tiles)
    levels: Arc<Levels>,
}

impl Output {
    fn open(name: &str, fall_back_to_default: bool, levels: Arc<Levels>) -> Result<Output, String> {
        let host = cpal::default_host();
        let mut device = find_output(&host, name);
        if device.is_none() && fall_back_to_default {
            device = host.default_output_device();
        }
        let device = device.ok_or_else(|| format!("Output device '{}' not found.", name))?;
        let (stream, handle) =
            OutputStream::try_from_device(&device).map_err(|e| format!("Cannot open '{}': {}", name, e))?;
        Ok(Output {
            name: name.to_string(),
            _stream: stream,
            handle,
            sinks: Vec::new(),
            gain: 1.0,
            headroom: 1.0,
            levels,
        })
    }

    fn level(&self, volume: f32) -> f32 {
        curve(volume) * curve(self.gain) * self.headroom
    }

    fn update_levels(&self) {
        for (sink, vol) in &self.sinks {
            sink.set_volume(self.level(*vol));
        }
    }

    fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
        self.update_levels();
    }

    fn set_headroom(&mut self, headroom: f32) {
        self.headroom = headroom;
        self.update_levels();
    }

    fn play(&mut self, file: &str, volume: f32, looped: bool, overlap: bool, fx: u32, duck: bool) -> Result<(), String> {
        self.sinks.retain(|(s, _)| !s.empty());
        if !overlap {
            self.stop();
        }
        let f = File::open(file).map_err(|e| format!("Cannot open '{}': {}", file, e))?;
        let source = Decoder::new(BufReader::new(f)).map_err(|e| format!("Cannot decode '{}': {}", file, e))?;
        let sink = Sink::try_new(&self.handle).map_err(|e| e.to_string())?;
        sink.set_volume(self.level(volume));
        // every source goes through a `Tap`, which only measures: the sound is not changed
        let lv = &self.levels;
        if fx == 0 {
            if looped {
                sink.append(Tap::new(source.buffered().repeat_infinite().convert_samples::<f32>(), Arc::clone(lv), duck));
            } else {
                sink.append(Tap::new(source.convert_samples::<f32>(), Arc::clone(lv), duck));
            }
        } else if looped {
            let fxs = FxSource::new(source.buffered().repeat_infinite().convert_samples::<f32>(), fx);
            sink.append(Tap::new(fxs, Arc::clone(lv), duck));
        } else {
            sink.append(Tap::new(FxSource::new(source.convert_samples::<f32>(), fx), Arc::clone(lv), duck));
        }
        self.sinks.push((sink, volume));
        Ok(())
    }

    fn pause(&self) {
        for (s, _) in &self.sinks {
            s.pause();
        }
    }

    fn resume(&self) {
        for (s, _) in &self.sinks {
            s.play();
        }
    }

    fn stop(&mut self) {
        for (s, _) in self.sinks.drain(..) {
            s.stop();
        }
    }
}

/// A sound file with a sound effect on it (see `fx::FxStream`).
struct FxSource<S: Source<Item = f32>> {
    stream: crate::fx::FxStream<S>,
    channels: u16,
    rate: u32,
}

impl<S: Source<Item = f32>> FxSource<S> {
    fn new(inner: S, fx: u32) -> Self {
        let channels = inner.channels();
        let rate = inner.sample_rate();
        FxSource { stream: crate::fx::FxStream::new(inner, fx, channels, rate), channels, rate }
    }
}

impl<S: Source<Item = f32>> Iterator for FxSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        self.stream.next()
    }
}

impl<S: Source<Item = f32>> Source for FxSource<S> {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

/// Peak limiter: nothing gets above `ceiling`. The gain drops at once for a peak and comes
/// back slowly (about 80 ms), so loud parts get a little quieter instead of crackling.
struct Limiter {
    gain: f32,
    release: f32,
}

impl Limiter {
    fn new(rate: u32) -> Self {
        Limiter { gain: 1.0, release: 1.0 - (-1.0 / (0.08 * rate.max(8000) as f32)).exp() }
    }

    fn process(&mut self, x: f32, ceiling: f32) -> f32 {
        let a = x.abs();
        let need = if a > ceiling { ceiling / a } else { 1.0 };
        // down at once for a peak; up slowly, but never above what this sample allows
        let released = self.gain + (1.0 - self.gain) * self.release;
        self.gain = released.min(need);
        x * self.gain
    }
}

/// Passes a sound through and measures it: the RMS of five frequency bands per
/// ~20 ms block goes to `Levels` (the UI polls it and moves the white bars on the tile).
/// The bands come from one-pole low-pass splitters on the mono mix. Cheap, no FFT.
struct Tap<S: Source<Item = f32>> {
    inner: S,
    levels: Arc<Levels>,
    channels: u16,
    rate: u32,
    alpha: [f32; BANDS - 1],
    lp: [f32; BANDS - 1],
    frame_sum: f32,
    frame_pos: u16,
    acc: [f32; BANDS],
    frames: u32,
    block: u32,
    /// this sound goes to the output that carries the mic, so it ducks while you speak
    duck: bool,
    duck_gain: f32,
    duck_attack: f32,
    duck_release: f32,
    limiter: Limiter,
}

impl<S: Source<Item = f32>> Tap<S> {
    fn new(inner: S, levels: Arc<Levels>, duck: bool) -> Self {
        let channels = inner.channels().max(1);
        let rate = inner.sample_rate().max(8000);
        let mut alpha = [0.0f32; BANDS - 1];
        for (a, hz) in alpha.iter_mut().zip(BAND_EDGES.iter()) {
            *a = 1.0 - (-2.0 * std::f32::consts::PI * hz / rate as f32).exp();
        }
        Tap {
            inner,
            levels,
            channels,
            rate,
            alpha,
            lp: [0.0; BANDS - 1],
            frame_sum: 0.0,
            frame_pos: 0,
            acc: [0.0; BANDS],
            frames: 0,
            block: (rate / 50).max(256),
            duck,
            duck_gain: 1.0,
            duck_attack: 1.0 - (-1.0 / (DUCK_ATTACK_S * rate as f32)).exp(),
            duck_release: 1.0 - (-1.0 / (DUCK_RELEASE_S * rate as f32)).exp(),
            limiter: Limiter::new(rate),
        }
    }

    /// Once per frame: move the ducking gain toward "quiet" while you speak, else toward 1.
    fn update_duck(&mut self) {
        let target = if self.duck
            && self.levels.duck_on.load(Ordering::Relaxed)
            && self.levels.speaking.load(Ordering::Relaxed)
        {
            DUCK_GAIN
        } else {
            1.0
        };
        let c = if target < self.duck_gain { self.duck_attack } else { self.duck_release };
        self.duck_gain += (target - self.duck_gain) * c;
        if target == 1.0 && (1.0 - self.duck_gain) < 1e-4 {
            self.duck_gain = 1.0; // settled: exactly 1, so the sound passes untouched
        }
    }

    fn feed(&mut self, x: f32) {
        let mut prev = 0.0f32;
        for i in 0..BANDS - 1 {
            self.lp[i] += self.alpha[i] * (x - self.lp[i]);
            let band = self.lp[i] - prev;
            self.acc[i] += band * band;
            prev = self.lp[i];
        }
        let top = x - prev;
        self.acc[BANDS - 1] += top * top;
        self.frames += 1;
        if self.frames >= self.block {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.frames == 0 {
            return;
        }
        let n = self.frames as f32;
        let mut out = [0.0f32; BANDS];
        for (o, a) in out.iter_mut().zip(self.acc.iter()) {
            *o = (a / n).sqrt();
        }
        self.levels.note_bands(&out);
        self.acc = [0.0; BANDS];
        self.frames = 0;
    }
}

impl<S: Source<Item = f32>> Iterator for Tap<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let s = self.inner.next();
        match s {
            Some(x) => {
                if self.frame_pos == 0 {
                    self.update_duck();
                }
                // the bars on the tile see the sound as it is, before ducking and limiting
                self.frame_sum += x;
                self.frame_pos += 1;
                if self.frame_pos >= self.channels {
                    let mono = self.frame_sum / self.channels as f32;
                    self.frame_sum = 0.0;
                    self.frame_pos = 0;
                    self.feed(mono);
                }
                let mut y = x * self.duck_gain;
                if self.levels.limiter_on.load(Ordering::Relaxed) {
                    y = self.limiter.process(y, SOUND_CEILING);
                }
                Some(y)
            }
            None => {
                self.flush();
                None
            }
        }
    }
}

impl<S: Source<Item = f32>> Source for Tap<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.inner.total_duration()
    }
}

// ---------- mic passthrough ----------

/// Linear resampler: pulls mono samples recorded at `in_rate` out of the queue at
/// `out_rate`. Cheap and fine for speech, so mic and cable can keep their own native rate.
struct Resampler {
    step: f64, // input samples per output sample
    pos: f64,  // fraction between q[0] and q[1], always in 0..1
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Resampler {
            step: in_rate as f64 / out_rate.max(1) as f64,
            pos: 0.0,
        }
    }

    /// Next output sample, or None when the queue has run dry.
    fn next(&mut self, q: &mut VecDeque<f32>) -> Option<f32> {
        if q.len() < 2 {
            return None;
        }
        let out = q[0] + (q[1] - q[0]) * self.pos as f32;
        self.pos += self.step;
        let whole = self.pos as usize;
        for _ in 0..whole.min(q.len()) {
            q.pop_front();
        }
        self.pos -= whole as f64;
        Some(out)
    }
}

struct Mic {
    _input: cpal::Stream,
    _output: cpal::Stream,
    volume: Arc<AtomicU32>,
    gate_on: Arc<AtomicBool>,
    gate_threshold: Arc<AtomicU32>,
    fx: Arc<AtomicU32>,
    levels: Arc<Levels>,
    /// the AI voice helper; stopped when the mic stops
    _ai: Option<Arc<crate::voice_ai::Live>>,
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.levels.clear_ai();
        self.levels.active.store(false, Ordering::Relaxed);
        self.levels.speaking.store(false, Ordering::Relaxed);
    }
}

impl Mic {
    fn start(
        input_name: &str,
        output_name: &str,
        volume: f32,
        gate_on: bool,
        gate_threshold: f32,
        fx: u32,
        levels: Arc<Levels>,
        ai: Option<crate::voice_ai::Plan>,
    ) -> Result<Mic, String> {
        let host = cpal::default_host();
        let in_dev = find_input(&host, input_name).ok_or_else(|| format!("Mic '{}' not found.", input_name))?;
        let out_dev = find_output(&host, output_name)
            .ok_or_else(|| format!("Mic output '{}' not found.", output_name))?;

        let in_cfg = in_dev.default_input_config().map_err(|e| e.to_string())?;
        let out_cfg = out_dev.default_output_config().map_err(|e| e.to_string())?;
        if in_cfg.sample_format() != cpal::SampleFormat::F32 || out_cfg.sample_format() != cpal::SampleFormat::F32 {
            return Err("Mic passthrough needs devices that support 32-bit float audio.".into());
        }

        let in_channels = in_cfg.channels() as usize;
        let out_channels = out_cfg.channels() as usize;
        let in_rate = in_cfg.sample_rate().0;
        let out_rate = out_cfg.sample_rate().0;
        // each device keeps its own native rate; the output callback resamples
        let in_conf: cpal::StreamConfig = in_cfg.into();
        let out_conf: cpal::StreamConfig = out_cfg.into();

        // Jitter buffer: start playing after ~40 ms of mic is queued, never hold more than ~150 ms.
        // AI voice comes in blocks of ~120 ms, so it needs a deeper buffer.
        let (max_ms, prime_ms) = if ai.is_some() { (700, 220) } else { (150, 40) };
        let max_samples = (in_rate as usize) * max_ms / 1000;
        let prime_samples = (in_rate as usize) * prime_ms / 1000;
        let buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(max_samples)));
        let vol = Arc::new(AtomicU32::new(volume.to_bits()));
        let gate_on = Arc::new(AtomicBool::new(gate_on));
        let gate_threshold = Arc::new(AtomicU32::new(gate_threshold.to_bits()));
        let fx = Arc::new(AtomicU32::new(fx));

        let in_buf = Arc::clone(&buffer);
        let in_vol = Arc::clone(&vol);
        let in_gate_on = Arc::clone(&gate_on);
        let in_gate_thr = Arc::clone(&gate_threshold);
        let in_fx = Arc::clone(&fx);
        let in_levels = Arc::clone(&levels);
        let mut gate = Gate::new(in_rate);
        // effect chain: rebuilt only when the choice changes (the old chain is dropped then)
        let mut fx_now: u32 = 0;
        let mut fx_chain = crate::fx::build(0, in_rate);
        let mut mono: Vec<f32> = Vec::with_capacity(4096);
        // AI voice: the helper runs in the background; until it is ready the mic stays silent
        let live: Option<Arc<crate::voice_ai::Live>> = ai.map(|plan| {
            let lv = Arc::clone(&levels);
            Arc::new(crate::voice_ai::Live::start(plan, move |s| lv.set_ai(&s)))
        });
        let in_live = live.clone();
        let mut ai_rs: Option<(Resampler, Resampler, usize)> = None; // mic -> engine, engine -> mic, block
        let mut ai_in: VecDeque<f32> = VecDeque::new();
        let mut ai_blk: Vec<f32> = Vec::new();
        let mut last_voice: Option<std::time::Instant> = None;
        let input = in_dev
            .build_input_stream(
                &in_conf,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // mono mix first, outside the lock
                    mono.clear();
                    mono.extend(data.chunks(in_channels.max(1)).map(|f| f.iter().sum::<f32>() / f.len() as f32));
                    let level_in = rms(&mono);
                    if in_gate_on.load(Ordering::Relaxed) {
                        gate.process(&mut mono, level_in, f32::from_bits(in_gate_thr.load(Ordering::Relaxed)));
                    } else {
                        gate.reset();
                    }
                    // AI voice: your voice goes to the engine and what comes back replaces it.
                    // Loading or failed: silence, so your real voice never leaks.
                    if let Some(lv) = in_live.as_ref() {
                        if lv.ready() {
                            let (to_eng, from_eng, block) = ai_rs.get_or_insert_with(|| {
                                (
                                    Resampler::new(in_rate, lv.rate()),
                                    Resampler::new(lv.rate(), in_rate),
                                    lv.block(),
                                )
                            });
                            ai_in.extend(mono.iter().copied());
                            while let Some(x) = to_eng.next(&mut ai_in) {
                                ai_blk.push(x);
                                if ai_blk.len() >= *block {
                                    lv.send(std::mem::take(&mut ai_blk));
                                }
                            }
                            mono.clear();
                            lv.take_output(|q| {
                                while let Some(x) = from_eng.next(q) {
                                    mono.push(x);
                                }
                            });
                        } else {
                            mono.clear();
                            ai_in.clear();
                            ai_blk.clear();
                        }
                    }
                    // sound effect after the gate, so noise is not "voice changed"
                    let want = in_fx.load(Ordering::Relaxed);
                    if want != fx_now {
                        fx_now = want;
                        fx_chain = crate::fx::build(want, in_rate);
                    }
                    if !fx_chain.is_empty() {
                        fx_chain.process(&mut mono);
                    }
                    let v = f32::from_bits(in_vol.load(Ordering::Relaxed));
                    let level_out = rms(&mono);
                    in_levels.note(level_in, level_out * v, gate.is_open());
                    // "speaking" for ducking: voice after the gate, with a short hold
                    if level_out > SPEAK_THRESHOLD {
                        last_voice = Some(std::time::Instant::now());
                    }
                    let speaking = last_voice.map(|t| t.elapsed().as_millis() < SPEAK_HOLD_MS).unwrap_or(false);
                    in_levels.speaking.store(speaking, Ordering::Relaxed);
                    if let Ok(mut q) = in_buf.lock() {
                        q.extend(mono.iter().copied());
                        while q.len() > max_samples {
                            q.pop_front(); // drop oldest, keep latency low
                        }
                    }
                },
                |e| eprintln!("mic input error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;

        let out_buf = Arc::clone(&buffer);
        let out_vol = Arc::clone(&vol);
        let out_levels = Arc::clone(&levels);
        let mut mic_limiter = Limiter::new(out_rate);
        let mut resampler = Resampler::new(in_rate, out_rate);
        let mut primed = false;
        let output = out_dev
            .build_output_stream(
                &out_conf,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let v = f32::from_bits(out_vol.load(Ordering::Relaxed));
                    let mut guard = out_buf.lock().ok();
                    if let Some(q) = guard.as_ref() {
                        if !primed && q.len() >= prime_samples {
                            primed = true;
                        }
                    }
                    for frame in data.chunks_mut(out_channels.max(1)) {
                        let s = match guard.as_mut() {
                            Some(q) if primed => match resampler.next(q) {
                                Some(x) => x * v,
                                None => {
                                    primed = false; // ran dry: wait until it refills, no crackle
                                    0.0
                                }
                            },
                            _ => 0.0,
                        };
                        let s = if out_levels.limiter_on.load(Ordering::Relaxed) {
                            mic_limiter.process(s, MIC_CEILING)
                        } else {
                            s
                        };
                        for sample in frame.iter_mut() {
                            *sample = s;
                        }
                    }
                },
                |e| eprintln!("mic output error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;

        output.play().map_err(|e| e.to_string())?;
        input.play().map_err(|e| e.to_string())?;
        levels.active.store(true, Ordering::Relaxed);

        Ok(Mic {
            _input: input,
            _output: output,
            volume: vol,
            gate_on,
            gate_threshold,
            fx,
            levels,
            _ai: live,
        })
    }

    fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    fn set_fx(&self, fx: u32) {
        self.fx.store(fx, Ordering::Relaxed);
    }

    fn set_gate(&self, on: bool, threshold: f32) {
        self.gate_on.store(on, Ordering::Relaxed);
        self.gate_threshold.store(threshold.to_bits(), Ordering::Relaxed);
    }
}

// ---------- device lookup ----------

fn find_output(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name == DEFAULT_OUT {
        return host.default_output_device();
    }
    host.output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

pub fn find_input(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name == DEFAULT_IN {
        return host.default_input_device();
    }
    host.input_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

pub fn list_devices() -> Devices {
    let host = cpal::default_host();
    let mut outputs = vec![DISABLED.to_string(), DEFAULT_OUT.to_string()];
    let mut inputs = vec![DISABLED.to_string(), DEFAULT_IN.to_string()];
    if let Ok(devs) = host.output_devices() {
        outputs.extend(devs.filter_map(|d| d.name().ok()));
    }
    if let Ok(devs) = host.input_devices() {
        inputs.extend(devs.filter_map(|d| d.name().ok()));
    }
    let cable = pick_cable(&outputs, AUTO_CABLE);
    Devices { outputs, inputs, cable }
}

// ---------- virtual cable (Go Live) ----------

/// Headroom for sounds while live: Windows adds sounds and mic on the cable, and a sum
/// above 1.0 would clip (crackle). -3 dB is enough for normal speech.
const LIVE_SOUND_HEADROOM: f32 = 0.7;

/// Pick the playback device that feeds Discord. `wanted` is the user's choice: a device
/// name, or `AUTO_CABLE` to look for VB-CABLE (then other virtual cables).
pub fn pick_cable(outputs: &[String], wanted: &str) -> Option<String> {
    if wanted != AUTO_CABLE && wanted != DISABLED {
        return outputs.iter().find(|n| n.as_str() == wanted).cloned();
    }
    fn rank(name: &str) -> Option<u8> {
        let n = name.to_lowercase();
        if n.contains("cable input") && !n.contains("16ch") {
            Some(0) // VB-CABLE, "CABLE Input (VB-Audio Virtual Cable)"
        } else if n.contains("cable-a input") || n.contains("cable-b input") {
            Some(1) // VB-CABLE A+B
        } else if n.contains("virtual cable") && !n.contains("16ch") {
            Some(2)
        } else {
            None
        }
    }
    outputs
        .iter()
        .filter_map(|n| rank(n).map(|r| (r, n)))
        .min_by_key(|(r, _)| *r)
        .map(|(_, n)| n.clone())
}

// ---------- engine thread ----------

/// Which device each part really uses right now (Go Live replaces the manual choices).
#[derive(Clone, Debug, PartialEq)]
struct Routing {
    primary: String,
    secondary: String,
    mic_in: String,
    mic_out: String,
}

/// `live` = the cable device while Go Live is on. Then sounds go to the cable (Secondary),
/// and the mic goes to the same cable, so Discord hears both. Mic input is the one chosen
/// in Settings, or the default mic. Your own headphones (Primary) stay as they are.
fn routing(s: &AudioSettings, live: Option<&str>) -> Routing {
    match live {
        Some(cable) => Routing {
            primary: s.primary_output.clone(),
            secondary: cable.to_string(),
            mic_in: if s.mic_input == DISABLED {
                DEFAULT_IN.to_string()
            } else {
                s.mic_input.clone()
            },
            mic_out: cable.to_string(),
        },
        None => Routing {
            primary: s.primary_output.clone(),
            secondary: s.secondary_output.clone(),
            mic_in: s.mic_input.clone(),
            mic_out: s.mic_output.clone(),
        },
    }
}

struct Engine {
    settings: Option<AudioSettings>,
    /// cable device name while Go Live is on
    live: Option<String>,
    applied: Option<Routing>,
    primary: Option<Output>,
    secondary: Option<Output>,
    mic: Option<Mic>,
    /// the AI Voice plan the mic was opened with
    ai_applied: Option<crate::voice_ai::Plan>,
    levels: Arc<Levels>,
    on_error: Box<dyn Fn(String) + Send>,
}

impl Engine {
    fn configure(&mut self, s: AudioSettings) {
        self.levels.duck_on.store(s.duck_on, Ordering::Relaxed);
        self.levels.limiter_on.store(s.limiter_on, Ordering::Relaxed);
        self.settings = Some(s);
        self.apply();
    }

    /// Open/close only what changed since the last call.
    fn apply(&mut self) {
        let s = match self.settings.clone() {
            Some(s) => s,
            None => return,
        };
        let route = routing(&s, self.live.as_deref());
        let (primary_changed, secondary_changed, mic_changed) = match &self.applied {
            None => (true, true, true),
            Some(p) => (
                p.primary != route.primary,
                p.secondary != route.secondary,
                p.mic_in != route.mic_in || p.mic_out != route.mic_out,
            ),
        };
        let mic_changed = mic_changed || self.ai_applied != s.ai;

        if primary_changed {
            self.primary = self.open_output(&route.primary, true);
        }
        if secondary_changed {
            self.secondary = self.open_output(&route.secondary, false);
        }
        self.update_gains();

        if mic_changed {
            self.mic = None; // drop old streams first
            if route.mic_in != DISABLED && route.mic_out != DISABLED {
                match Mic::start(
                    &route.mic_in,
                    &route.mic_out,
                    s.mic_volume,
                    s.gate_on,
                    s.gate_threshold,
                    s.fx,
                    Arc::clone(&self.levels),
                    s.ai.clone(),
                ) {
                    Ok(m) => self.mic = Some(m),
                    Err(e) => (self.on_error)(e),
                }
            }
        }
        if let Some(m) = self.mic.as_ref() {
            m.set_volume(s.mic_volume);
            m.set_gate(s.gate_on, s.gate_threshold);
            m.set_fx(s.fx);
        }

        self.ai_applied = s.ai.clone();
        self.applied = Some(route);
    }

    fn update_gains(&mut self) {
        let s = match self.settings.clone() {
            Some(s) => s,
            None => return,
        };
        let headroom = if self.live.is_some() { LIVE_SOUND_HEADROOM } else { 1.0 };
        if let Some(o) = self.primary.as_mut() {
            o.set_gain(s.volume * s.primary_volume);
        }
        if let Some(o) = self.secondary.as_mut() {
            o.set_gain(s.volume * s.secondary_volume);
            o.set_headroom(headroom);
        }
    }

    /// An output that failed to open earlier (busy, unplugged) gets another try when a sound
    /// is played, so one bad moment at start-up does not leave it silent for good.
    fn retry_missing_outputs(&mut self) {
        let route = match &self.applied {
            Some(r) => r.clone(),
            None => return,
        };
        let mut reopened = false;
        if self.primary.is_none() && route.primary != DISABLED {
            self.primary = self.open_output(&route.primary, true);
            reopened = true;
        }
        if self.secondary.is_none() && route.secondary != DISABLED {
            self.secondary = self.open_output(&route.secondary, false);
            reopened = true;
        }
        if reopened {
            self.update_gains();
        }
    }

    /// Go Live: sounds and mic both go to the virtual cable. Turning it off puts the manual
    /// settings back. Safe to call twice.
    fn set_live(&mut self, on: bool) -> Result<LiveStatus, String> {
        self.applied = None; // reopen every device fresh: no stale stream survives a switch
        if !on {
            self.live = None;
            self.apply();
            return Ok(LiveStatus { on: false, output: String::new(), sounds: false, mic: false, headphones: true });
        }
        let wanted = match self.settings.as_ref() {
            Some(s) => s.live_output.clone(),
            None => return Err("Audio is not ready yet. Try again in a moment.".into()),
        };
        let outputs = list_devices().outputs;
        let cable = pick_cable(&outputs, &wanted).ok_or_else(|| {
            if wanted == AUTO_CABLE || wanted == DISABLED {
                "No virtual cable found. Install VB-CABLE (vb-audio.com/Cable), restart Windows, then press Go Live again."
                    .to_string()
            } else {
                format!("Go Live output '{}' not found.", wanted)
            }
        })?;
        self.live = Some(cable.clone());
        self.apply();
        let headphones = self.primary.is_some()
            || self.settings.as_ref().map(|s| s.primary_output == DISABLED).unwrap_or(true);
        let status = LiveStatus {
            on: true,
            output: cable.clone(),
            sounds: self.secondary.is_some(),
            mic: self.mic.is_some(),
            headphones,
        };
        if !status.sounds && !status.mic {
            self.live = None; // nothing opened: do not pretend to be live
            self.applied = None;
            self.apply();
            return Err(format!("Could not open '{}'. Is another app using it exclusively?", cable));
        }
        Ok(status)
    }

    fn open_output(&self, name: &str, fall_back: bool) -> Option<Output> {
        if name == DISABLED {
            return None;
        }
        match Output::open(name, fall_back, Arc::clone(&self.levels)) {
            Ok(o) => Some(o),
            Err(e) => {
                (self.on_error)(e);
                None
            }
        }
    }

    fn play(&mut self, file: &str, volume: f32, looped: bool, fx: u32) {
        self.retry_missing_outputs();
        let overlap = self.settings.as_ref().map(|s| s.overlap).unwrap_or(false);
        self.levels.set_sound_file(file);
        // ducking only on the output that carries the mic (the cable while Go Live is on)
        let mic_out = self.applied.as_ref().map(|r| r.mic_out.clone()).unwrap_or_default();
        let mic_on = self.mic.is_some();
        let mut errors = Vec::new();
        for out in [self.primary.as_mut(), self.secondary.as_mut()].into_iter().flatten() {
            let duck = mic_on && out.name == mic_out;
            if let Err(e) = out.play(file, volume, looped, overlap, fx, duck) {
                errors.push(e);
            }
        }
        for e in errors {
            (self.on_error)(e);
        }
    }

    fn each_output(&mut self, f: impl Fn(&mut Output)) {
        for out in [self.primary.as_mut(), self.secondary.as_mut()].into_iter().flatten() {
            f(out);
        }
    }
}

/// Start the audio thread. Returns the command sender. `levels` is where the mic meters are written.
pub fn spawn(levels: Arc<Levels>, on_error: impl Fn(String) + Send + 'static) -> Sender<AudioCmd> {
    let (tx, rx) = mpsc::channel::<AudioCmd>();
    thread::spawn(move || {
        let mut engine = Engine {
            settings: None,
            live: None,
            applied: None,
            primary: None,
            secondary: None,
            mic: None,
            ai_applied: None,
            levels,
            on_error: Box::new(on_error),
        };
        for cmd in rx {
            match cmd {
                AudioCmd::Configure(s) => engine.configure(s),
                AudioCmd::Play { file, volume, looped, fx } => engine.play(&file, volume, looped, fx),
                AudioCmd::Pause => engine.each_output(|o| o.pause()),
                AudioCmd::Resume => engine.each_output(|o| o.resume()),
                AudioCmd::Stop => engine.each_output(|o| o.stop()),
                AudioCmd::Devices(reply) => {
                    let _ = reply.send(list_devices());
                }
                AudioCmd::Live(on, reply) => {
                    let _ = reply.send(engine.set_live(on));
                }
            }
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn auto_finds_vb_cable() {
        let o = names(&["Speakers (Realtek)", "Headphones", "CABLE In 16ch (VB-Audio Virtual Cable)",
                        "CABLE Input (VB-Audio Virtual Cable)"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE).as_deref(), Some("CABLE Input (VB-Audio Virtual Cable)"));
    }

    #[test]
    fn auto_prefers_vb_cable_over_other_virtual_cables() {
        let o = names(&["Some Virtual Cable Out", "CABLE-A Input (VB-Audio Cable A)", "CABLE Input (VB-Audio Virtual Cable)"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE).as_deref(), Some("CABLE Input (VB-Audio Virtual Cable)"));
        let o = names(&["Speakers", "Some Virtual Cable Out"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE).as_deref(), Some("Some Virtual Cable Out"));
    }

    #[test]
    fn auto_never_picks_the_16ch_cable_alone() {
        let o = names(&["Speakers", "CABLE In 16ch (VB-Audio Virtual Cable)"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE), None);
    }

    #[test]
    fn no_cable_means_none() {
        assert_eq!(pick_cable(&names(&["Speakers", "Headphones"]), AUTO_CABLE), None);
    }

    #[test]
    fn explicit_choice_wins_and_must_exist() {
        let o = names(&["CABLE Input (VB-Audio Virtual Cable)", "CABLE-A Input (VB-Audio Cable A)"]);
        assert_eq!(pick_cable(&o, "CABLE-A Input (VB-Audio Cable A)").as_deref(),
                   Some("CABLE-A Input (VB-Audio Cable A)"));
        assert_eq!(pick_cable(&o, "Gone Device"), None);
    }

    fn settings() -> AudioSettings {
        AudioSettings {
            volume: 1.0, primary_output: "Headphones".into(), primary_volume: 1.0,
            secondary_output: DISABLED.into(), secondary_volume: 1.0, overlap: false,
            mic_input: DISABLED.into(), mic_output: DISABLED.into(), mic_volume: 1.0,
            live_output: AUTO_CABLE.into(),
            gate_on: false, gate_threshold: 0.003,
            fx: 0,
            duck_on: false, limiter_on: true, ai: None,
        }
    }

    #[test]
    fn live_sends_sounds_and_mic_to_the_cable_and_keeps_headphones() {
        let r = routing(&settings(), Some("CABLE Input"));
        assert_eq!(r.secondary, "CABLE Input");
        assert_eq!(r.mic_out, "CABLE Input");
        assert_eq!(r.mic_in, DEFAULT_IN);          // no mic chosen: use the default one
        assert_eq!(r.primary, "Headphones");
    }

    #[test]
    fn live_uses_the_mic_from_settings() {
        let mut s = settings();
        s.mic_input = "Blue Yeti".into();
        assert_eq!(routing(&s, Some("CABLE Input")).mic_in, "Blue Yeti");
    }

    #[test]
    fn not_live_keeps_manual_routing() {
        let mut s = settings();
        s.secondary_output = "Speakers 2".into();
        let r = routing(&s, None);
        assert_eq!((r.secondary.as_str(), r.mic_in.as_str(), r.mic_out.as_str()),
                   ("Speakers 2", DISABLED, DISABLED));
    }

    fn feed(q: &mut VecDeque<f32>, xs: &[f32]) {
        q.extend(xs.iter().copied());
    }

    #[test]
    fn same_rate_passes_samples_through() {
        let mut r = Resampler::new(48000, 48000);
        let mut q = VecDeque::new();
        feed(&mut q, &[0.1, 0.2, 0.3, 0.4, 0.5]);
        let out: Vec<f32> = std::iter::from_fn(|| r.next(&mut q)).collect();
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]); // last sample waits for its neighbour
    }

    #[test]
    fn upsampling_44k_to_48k_makes_more_samples_and_stays_smooth() {
        let n_in = 44100usize;
        let sine: Vec<f32> = (0..n_in).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin() * 0.5).collect();
        let mut r = Resampler::new(44100, 48000);
        let mut q = VecDeque::new();
        feed(&mut q, &sine);
        let out: Vec<f32> = std::iter::from_fn(|| r.next(&mut q)).collect();
        assert!((out.len() as i64 - 48000).abs() < 3, "got {}", out.len());
        let max_step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(max_step < 0.1, "jump {max_step}");          // 440 Hz at 0.5 moves <0.03 per sample
        assert!(out.iter().all(|x| x.abs() <= 0.5 + 1e-3));  // never louder than the input
    }

    #[test]
    fn downsampling_48k_to_44k_makes_fewer_samples() {
        let mut r = Resampler::new(48000, 44100);
        let mut q = VecDeque::new();
        feed(&mut q, &vec![0.25f32; 48000]);
        let out: Vec<f32> = std::iter::from_fn(|| r.next(&mut q)).collect();
        assert!((out.len() as i64 - 44100).abs() < 3, "got {}", out.len());
    }

    #[test]
    fn empty_queue_gives_none_and_recovers() {
        let mut r = Resampler::new(48000, 48000);
        let mut q = VecDeque::new();
        assert_eq!(r.next(&mut q), None);
        feed(&mut q, &[0.5, 0.5, 0.5]);
        assert_eq!(r.next(&mut q), Some(0.5));
    }

    // ----- noise gate -----

    const SR: u32 = 48000;
    const BLOCK: usize = 480; // 10 ms

    fn run_gate(g: &mut Gate, amp: f32, blocks: usize, thr: f32) -> f32 {
        let mut last = 0.0;
        for _ in 0..blocks {
            let mut b = vec![amp; BLOCK];
            let lvl = rms(&b);
            g.process(&mut b, lvl, thr);
            last = *b.last().unwrap();
        }
        last
    }

    #[test]
    fn gate_passes_loud_sound() {
        let mut g = Gate::new(SR);
        let out = run_gate(&mut g, 0.5, 10, 0.01);
        assert!(out > 0.49, "got {out}");
        assert!(g.is_open());
    }

    #[test]
    fn gate_closes_on_quiet_noise_after_hold() {
        let mut g = Gate::new(SR);
        run_gate(&mut g, 0.5, 5, 0.01);
        let out = run_gate(&mut g, 0.001, 100, 0.01); // 1 s of room noise
        assert!(out.abs() < 0.001 * 0.01, "got {out}");
        assert!(!g.is_open());
    }

    #[test]
    fn gate_holds_open_right_after_speech() {
        let mut g = Gate::new(SR);
        run_gate(&mut g, 0.5, 5, 0.01);
        let out = run_gate(&mut g, 0.001, 5, 0.01); // only 50 ms of quiet
        assert!(out > 0.0009, "gate chopped the word end: {out}");
        assert!(g.is_open());
    }

    #[test]
    fn gate_hysteresis_keeps_it_open_just_under_threshold() {
        let mut g = Gate::new(SR);
        run_gate(&mut g, 0.5, 5, 0.01);
        // level 0.007 is below 0.01 but above 0.01 * 0.63: still counts as speech
        run_gate(&mut g, 0.007, 60, 0.01);
        assert!(g.is_open());
    }

    #[test]
    fn gate_reset_is_a_pass_through() {
        let mut g = Gate::new(SR);
        g.reset();
        let mut b = vec![0.25; BLOCK];
        let lvl = rms(&b);
        g.process(&mut b, lvl, 0.5); // threshold far above the level, but hold keeps it open
        assert!((b[0] - 0.25).abs() < 1e-6);
    }

    // ----- meters -----

    #[test]
    fn levels_keep_the_peak_until_read() {
        let l = Levels::default();
        l.note(0.1, 0.05, true);
        l.note(0.5, 0.2, true);
        l.note(0.2, 0.1, true);
        let r = l.take();
        assert!((r.input_db - to_db(0.5)).abs() < 0.01);
        assert!((r.output_db - to_db(0.2)).abs() < 0.01);
        let again = l.take();
        assert_eq!(again.input_db, -100.0);
    }

    #[test]
    fn tap_hears_bass_in_the_low_band_and_leaves_the_sound_alone() {
        use rodio::buffer::SamplesBuffer;
        let rate = 44_100u32;
        let wave: Vec<f32> = (0..rate)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / rate as f32).sin())
            .collect();
        let levels = Arc::new(Levels::default());
        let tap = Tap::new(SamplesBuffer::new(1, rate, wave.clone()), Arc::clone(&levels), false);
        let out: Vec<f32> = tap.collect();
        assert_eq!(out, wave); // untouched
        let r = levels.take_sound();
        assert!(r.bands[0] > 0.2, "bass band: {:?}", r.bands);
        assert!(r.bands[4] < 0.05, "treble band: {:?}", r.bands);
        assert_eq!(levels.take_sound().bands, [0.0; BANDS]); // read once, then start over
    }

    fn tap_out(samples: Vec<f32>, levels: &Arc<Levels>, duck: bool) -> Vec<f32> {
        use rodio::buffer::SamplesBuffer;
        Tap::new(SamplesBuffer::new(1, 44_100, samples), Arc::clone(levels), duck).collect()
    }

    #[test]
    fn ducking_lowers_the_sound_while_you_speak_and_brings_it_back() {
        let levels = Arc::new(Levels::default());
        levels.duck_on.store(true, Ordering::Relaxed);
        let steady = vec![0.5f32; 44_100];

        // not speaking: untouched
        assert_eq!(tap_out(steady.clone(), &levels, true), steady);

        // speaking: it goes down to about DUCK_GAIN
        levels.speaking.store(true, Ordering::Relaxed);
        let out = tap_out(steady.clone(), &levels, true);
        let end = out[out.len() - 1];
        assert!((end - 0.5 * DUCK_GAIN).abs() < 0.01, "ducked level: {end}");
        assert!(out[0] > end, "it goes down smoothly, not in one step");

        // this sound does not go to the mic output: no ducking
        assert_eq!(tap_out(steady.clone(), &levels, false), steady);

        // switch off: untouched again, even while speaking
        levels.duck_on.store(false, Ordering::Relaxed);
        assert_eq!(tap_out(steady.clone(), &levels, true), steady);
    }

    #[test]
    fn limiter_keeps_peaks_under_the_ceiling_and_leaves_quiet_sound_alone() {
        let levels = Arc::new(Levels::default());
        let loud: Vec<f32> = (0..44_100)
            .map(|i| 2.0 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 44_100.0).sin())
            .collect();
        // off: loud stays loud
        let off = tap_out(loud.clone(), &levels, false);
        assert!(off.iter().any(|x| x.abs() > 1.5));

        levels.limiter_on.store(true, Ordering::Relaxed);
        let on = tap_out(loud, &levels, false);
        assert!(on.iter().all(|x| x.abs() <= SOUND_CEILING + 1e-4), "peak: {:?}", on.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs())));

        let quiet: Vec<f32> = (0..44_100)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 44_100.0).sin())
            .collect();
        assert_eq!(tap_out(quiet.clone(), &levels, false), quiet);
    }

    #[test]
    fn mic_limiter_caps_at_its_ceiling() {
        let mut l = Limiter::new(48_000);
        let peak = (0..48_000)
            .map(|i| l.process(1.5 * (i as f32 * 0.05).sin(), MIC_CEILING).abs())
            .fold(0.0f32, f32::max);
        assert!(peak <= MIC_CEILING + 1e-4, "peak {peak}");
        let mut l = Limiter::new(48_000);
        assert_eq!(l.process(0.2, MIC_CEILING), 0.2);
    }

    #[test]
    fn db_of_silence_and_full_scale() {
        assert_eq!(to_db(0.0), -100.0);
        assert!(to_db(1.0).abs() < 1e-6);
        assert!((to_db(0.1) + 20.0).abs() < 1e-4);
    }
}
