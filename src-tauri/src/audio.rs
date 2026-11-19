//! Audio engine. Runs on its own thread (cpal streams are not Send).
//! Port of AudioPlaybackEngine, MicPassthrough, SoundManager.

use crate::config::{Config, AUTO_CABLE, DEFAULT_OUT, DISABLED};
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

pub const DEFAULT_IN: &str = "Default Input Device";

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
}

pub enum AudioCmd {
    Configure(AudioSettings),
    Play { file: String, volume: f32, looped: bool },
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

// ---------- output ----------

struct Output {
    #[allow(dead_code)]
    name: String,
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sinks: Vec<(Sink, f32)>,
    gain: f32,
    /// extra fixed attenuation (1.0 = none), used while live so sounds + mic fit on the cable
    headroom: f32,
}

impl Output {
    fn open(name: &str, fall_back_to_default: bool) -> Result<Output, String> {
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

    fn play(&mut self, file: &str, volume: f32, looped: bool, overlap: bool) -> Result<(), String> {
        self.sinks.retain(|(s, _)| !s.empty());
        if !overlap {
            self.stop();
        }
        let f = File::open(file).map_err(|e| format!("Cannot open '{}': {}", file, e))?;
        let source = Decoder::new(BufReader::new(f)).map_err(|e| format!("Cannot decode '{}': {}", file, e))?;
        let sink = Sink::try_new(&self.handle).map_err(|e| e.to_string())?;
        sink.set_volume(self.level(volume));
        if looped {
            sink.append(source.buffered().repeat_infinite());
        } else {
            sink.append(source);
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
}

impl Mic {
    fn start(input_name: &str, output_name: &str, volume: f32) -> Result<Mic, String> {
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
        let max_samples = (in_rate as usize) * 150 / 1000;
        let prime_samples = (in_rate as usize) * 40 / 1000;
        let buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(max_samples)));
        let vol = Arc::new(AtomicU32::new(volume.to_bits()));

        let in_buf = Arc::clone(&buffer);
        let input = in_dev
            .build_input_stream(
                &in_conf,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut q) = in_buf.lock() {
                        for frame in data.chunks(in_channels.max(1)) {
                            let mono = frame.iter().sum::<f32>() / frame.len() as f32;
                            q.push_back(mono);
                        }
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

        Ok(Mic {
            _input: input,
            _output: output,
            volume: vol,
        })
    }

    fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
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

fn find_input(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name == DEFAULT_IN {
        return host.default_input_device();
    }
    host.input_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

fn list_devices() -> Devices {
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
        } else if n.contains("virtual audio driver") {
            Some(2) // the driver in this repo
        } else if n.contains("virtual cable") && !n.contains("16ch") {
            Some(3)
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
    on_error: Box<dyn Fn(String) + Send>,
}

impl Engine {
    fn configure(&mut self, s: AudioSettings) {
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

        if primary_changed {
            self.primary = self.open_output(&route.primary, true);
        }
        if secondary_changed {
            self.secondary = self.open_output(&route.secondary, false);
        }
        let headroom = if self.live.is_some() { LIVE_SOUND_HEADROOM } else { 1.0 };
        if let Some(o) = self.primary.as_mut() {
            o.set_gain(s.volume * s.primary_volume);
        }
        if let Some(o) = self.secondary.as_mut() {
            o.set_gain(s.volume * s.secondary_volume);
            o.set_headroom(headroom);
        }

        if mic_changed {
            self.mic = None; // drop old streams first
            if route.mic_in != DISABLED && route.mic_out != DISABLED {
                match Mic::start(&route.mic_in, &route.mic_out, s.mic_volume) {
                    Ok(m) => self.mic = Some(m),
                    Err(e) => (self.on_error)(e),
                }
            }
        }
        if let Some(m) = self.mic.as_ref() {
            m.set_volume(s.mic_volume);
        }

        self.applied = Some(route);
    }

    /// Go Live: sounds and mic both go to the virtual cable. Turning it off puts the manual
    /// settings back. Safe to call twice.
    fn set_live(&mut self, on: bool) -> Result<LiveStatus, String> {
        if !on {
            self.live = None;
            self.apply();
            return Ok(LiveStatus { on: false, output: String::new(), sounds: false, mic: false });
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
        let status = LiveStatus {
            on: true,
            output: cable.clone(),
            sounds: self.secondary.is_some(),
            mic: self.mic.is_some(),
        };
        if !status.sounds && !status.mic {
            self.live = None; // nothing opened: do not pretend to be live
            self.apply();
            return Err(format!("Could not open '{}'. Is another app using it exclusively?", cable));
        }
        Ok(status)
    }

    fn open_output(&self, name: &str, fall_back: bool) -> Option<Output> {
        if name == DISABLED {
            return None;
        }
        match Output::open(name, fall_back) {
            Ok(o) => Some(o),
            Err(e) => {
                (self.on_error)(e);
                None
            }
        }
    }

    fn play(&mut self, file: &str, volume: f32, looped: bool) {
        let overlap = self.settings.as_ref().map(|s| s.overlap).unwrap_or(false);
        let mut errors = Vec::new();
        for out in [self.primary.as_mut(), self.secondary.as_mut()].into_iter().flatten() {
            if let Err(e) = out.play(file, volume, looped, overlap) {
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

/// Start the audio thread. Returns the command sender.
pub fn spawn(on_error: impl Fn(String) + Send + 'static) -> Sender<AudioCmd> {
    let (tx, rx) = mpsc::channel::<AudioCmd>();
    thread::spawn(move || {
        let mut engine = Engine {
            settings: None,
            live: None,
            applied: None,
            primary: None,
            secondary: None,
            mic: None,
            on_error: Box::new(on_error),
        };
        for cmd in rx {
            match cmd {
                AudioCmd::Configure(s) => engine.configure(s),
                AudioCmd::Play { file, volume, looped } => engine.play(&file, volume, looped),
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
    fn auto_prefers_vb_cable_over_other_virtual_devices() {
        let o = names(&["Virtual Audio Driver", "CABLE Input (VB-Audio Virtual Cable)"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE).as_deref(), Some("CABLE Input (VB-Audio Virtual Cable)"));
        let o = names(&["Speakers", "Virtual Audio Driver"]);
        assert_eq!(pick_cable(&o, AUTO_CABLE).as_deref(), Some("Virtual Audio Driver"));
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
}
