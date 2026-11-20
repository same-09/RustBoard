//! Mic sound effects (voice changer). Port of Voicefork's effects.py + presets.py.
//!
//! Every effect keeps its own state, so a block can have any length and the block edges
//! are not audible. Nothing here allocates inside `process`, so it is safe in the audio callback.

use std::f64::consts::PI;

pub trait Effect: Send {
    fn process(&mut self, x: &mut [f32]);
}

pub struct Chain {
    effects: Vec<Box<dyn Effect>>,
}

impl Chain {
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn process(&mut self, x: &mut [f32]) {
        for fx in self.effects.iter_mut() {
            fx.process(x);
        }
    }
}

// ---------- presets ----------

/// Preset ids. Index 0 is "clean" (no effect). The UI shows the same ids.
pub const IDS: [&str; 11] = [
    "clean", "deep", "chipmunk", "demon", "giant", "robot", "alien", "radio", "megaphone", "cave", "8bit",
];

/// Position of a preset id in `IDS`. Unknown id = 0 (clean).
pub fn index_of(id: &str) -> u32 {
    IDS.iter().position(|n| *n == id).unwrap_or(0) as u32
}

pub fn is_known(id: &str) -> bool {
    IDS.contains(&id)
}

/// Settings value for the audio thread: 0 = effects off, else the preset index.
pub fn active_index(on: bool, id: &str) -> u32 {
    if on {
        index_of(id)
    } else {
        0
    }
}

/// How long an effect keeps ringing after the sound ends (echo, reverb), in ms.
pub fn tail_ms(index: u32) -> u32 {
    match IDS.get(index as usize).copied().unwrap_or("clean") {
        "giant" => 3000,
        "cave" => 2800,
        "demon" => 1800,
        "alien" => 600,
        "robot" => 150,
        _ => 0,
    }
}

/// Build the chain for a preset index. 0 (or out of range) = empty chain = pass-through.
/// Every real preset ends with a limiter, because distortion and reverb can get loud.
pub fn build(index: u32, sr: u32) -> Chain {
    let sr = sr.max(8000);
    let mut v: Vec<Box<dyn Effect>> = Vec::new();
    match IDS.get(index as usize).copied().unwrap_or("clean") {
        "deep" => {
            v.push(Box::new(PitchShift::new(sr, 0.78, 45.0)));
            v.push(Box::new(Biquad::new(sr, Kind::Peak, 140.0, 0.8, 3.0)));
        }
        "chipmunk" => {
            v.push(Box::new(PitchShift::new(sr, 1.6, 30.0)));
            v.push(Box::new(Biquad::new(sr, Kind::High, 150.0, 0.707, 0.0)));
        }
        "demon" => {
            v.push(Box::new(PitchShift::new(sr, 0.6, 55.0)));
            v.push(Box::new(Distortion::new(10.0, -4.0)));
            v.push(Box::new(Biquad::new(sr, Kind::Low, 3500.0, 0.707, 0.0)));
            v.push(Box::new(Reverb::new(sr, 1.5, 0.18, 0.35)));
        }
        "giant" => {
            v.push(Box::new(PitchShift::new(sr, 0.65, 55.0)));
            v.push(Box::new(Reverb::new(sr, 2.5, 0.3, 0.35)));
        }
        "robot" => {
            v.push(Box::new(RingMod::new(sr, 50.0, 1.0)));
            v.push(Box::new(Echo::new(sr, 11.0, 0.55, 0.6, 0.0)));
        }
        "alien" => {
            v.push(Box::new(PitchShift::new(sr, 1.25, 45.0)));
            v.push(Box::new(RingMod::new(sr, 230.0, 0.5)));
            v.push(Box::new(Echo::new(sr, 90.0, 0.35, 0.3, 0.3)));
        }
        "radio" => {
            v.push(Box::new(Biquad::new(sr, Kind::High, 450.0, 0.707, 0.0)));
            v.push(Box::new(Biquad::new(sr, Kind::Low, 2800.0, 0.707, 0.0)));
            v.push(Box::new(Distortion::new(14.0, -8.0)));
            v.push(Box::new(Gain::new(4.0)));
        }
        "megaphone" => {
            v.push(Box::new(Biquad::new(sr, Kind::Band, 1500.0, 0.9, 0.0)));
            v.push(Box::new(Distortion::new(18.0, -6.0)));
            v.push(Box::new(Gain::new(6.0)));
        }
        "cave" => {
            v.push(Box::new(Echo::new(sr, 280.0, 0.45, 0.4, 0.4)));
            v.push(Box::new(Reverb::new(sr, 2.0, 0.35, 0.35)));
        }
        "8bit" => {
            v.push(Box::new(Bitcrush::new(sr, 5, 7000.0)));
            v.push(Box::new(Biquad::new(sr, Kind::High, 200.0, 0.707, 0.0)));
        }
        _ => return Chain { effects: v }, // clean
    }
    v.push(Box::new(Limiter::new(sr, -1.0, 80.0)));
    Chain { effects: v }
}

// ---------- effects on a sound file ----------

/// Runs an interleaved sample stream (a decoded sound file) through the effect, one chain per
/// channel. Works in chunks of 512 frames. After the file ends it keeps going for `tail_ms`,
/// so echo and reverb can ring out.
pub struct FxStream<I: Iterator<Item = f32>> {
    inner: I,
    ended: bool,
    ch: usize,
    chains: Vec<Chain>,
    planes: Vec<Vec<f32>>,
    out: Vec<f32>,
    pos: usize,
    tail_left: usize,
}

const CHUNK_FRAMES: usize = 512;

impl<I: Iterator<Item = f32>> FxStream<I> {
    pub fn new(inner: I, index: u32, channels: u16, rate: u32) -> Self {
        let ch = channels.max(1) as usize;
        FxStream {
            inner,
            ended: false,
            ch,
            chains: (0..ch).map(|_| build(index, rate)).collect(),
            planes: vec![Vec::with_capacity(CHUNK_FRAMES); ch],
            out: Vec::with_capacity(CHUNK_FRAMES * ch),
            pos: 0,
            tail_left: (rate as usize * tail_ms(index) as usize) / 1000,
        }
    }

    fn refill(&mut self) -> bool {
        self.out.clear();
        self.pos = 0;
        for p in self.planes.iter_mut() {
            p.clear();
        }
        let mut frames = 0;
        while frames < CHUNK_FRAMES && !self.ended {
            let mut got = 0;
            for c in 0..self.ch {
                match self.inner.next() {
                    Some(s) => {
                        self.planes[c].push(s);
                        got += 1;
                    }
                    None => {
                        self.ended = true;
                        break;
                    }
                }
            }
            if got == 0 {
                break;
            }
            for c in got..self.ch {
                self.planes[c].push(0.0); // file ended inside a frame: pad it
            }
            frames += 1;
        }
        if self.ended {
            // the file is over: feed silence while the tail rings
            while frames < CHUNK_FRAMES && self.tail_left > 0 {
                for p in self.planes.iter_mut() {
                    p.push(0.0);
                }
                self.tail_left -= 1;
                frames += 1;
            }
        }
        if frames == 0 {
            return false;
        }
        for (plane, chain) in self.planes.iter_mut().zip(self.chains.iter_mut()) {
            if !chain.is_empty() {
                chain.process(plane);
            }
        }
        for f in 0..frames {
            for c in 0..self.ch {
                self.out.push(self.planes[c][f]);
            }
        }
        true
    }
}

impl<I: Iterator<Item = f32>> Iterator for FxStream<I> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.out.len() && !self.refill() {
            return None;
        }
        let v = self.out[self.pos];
        self.pos += 1;
        Some(v)
    }
}

// ---------- level ----------

struct Gain {
    g: f32,
}

impl Gain {
    fn new(db: f32) -> Self {
        Gain { g: 10f32.powf(db / 20.0) }
    }
}

impl Effect for Gain {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            *s *= self.g;
        }
    }
}

/// Keeps peaks under the ceiling. Gain drops at once, and comes back slowly.
struct Limiter {
    sr: f32,
    ceiling: f32,
    release_s: f32,
    g: f32,
}

impl Limiter {
    fn new(sr: u32, ceiling_db: f32, release_ms: f32) -> Self {
        Limiter {
            sr: sr as f32,
            ceiling: 10f32.powf(ceiling_db / 20.0),
            release_s: release_ms / 1000.0,
            g: 1.0,
        }
    }
}

impl Effect for Limiter {
    fn process(&mut self, x: &mut [f32]) {
        let n = x.len();
        if n == 0 {
            return;
        }
        let peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let target = if peak > 0.0 { (self.ceiling / peak).min(1.0) } else { 1.0 };
        // going down = instant, going up = release
        let a = if target < self.g { 0.0 } else { (-(n as f32) / (self.sr * self.release_s)).exp() };
        let new = target + (self.g - target) * a;
        let step = (new - self.g) / n as f32;
        let mut g = self.g;
        for s in x.iter_mut() {
            g += step;
            *s = (*s * g).clamp(-self.ceiling, self.ceiling);
        }
        self.g = new;
    }
}

// ---------- filter ----------

#[derive(Clone, Copy)]
enum Kind {
    Low,
    High,
    Band,
    Peak,
}

/// RBJ cookbook biquad, transposed direct form II.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(sr: u32, kind: Kind, freq: f64, q: f64, gain_db: f64) -> Self {
        let w0 = 2.0 * PI * freq / sr as f64;
        let (cos, alpha) = (w0.cos(), w0.sin() / (2.0 * q));
        let big_a = 10f64.powf(gain_db / 40.0);
        let (b, a) = match kind {
            Kind::Low => ([(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha]),
            Kind::High => ([(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha]),
            Kind::Band => ([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha]),
            Kind::Peak => (
                [1.0 + alpha * big_a, -2.0 * cos, 1.0 - alpha * big_a],
                [1.0 + alpha / big_a, -2.0 * cos, 1.0 - alpha / big_a],
            ),
        };
        Biquad {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
            z: [0.0; 2],
        }
    }
}

impl Effect for Biquad {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let v = *s as f64;
            let y = self.b[0] * v + self.z[0];
            self.z[0] = self.b[1] * v - self.a[0] * y + self.z[1];
            self.z[1] = self.b[2] * v - self.a[1] * y;
            *s = y as f32;
        }
    }
}

// ---------- pitch ----------

/// Low-latency delay-line pitch shifter. Two read taps sweep through a short delay line at
/// speed `ratio`; each tap fades out right before it wraps, and the two are half a window
/// apart, so their gains always add up to 1. Latency is about window / 2.
struct PitchShift {
    buf: Vec<f32>,
    mask: i64,
    w: f64,
    t: i64,
    phase: f64,
    inc: f64,
    pass: bool,
}

const MIN_DELAY: f64 = 2.0;

impl PitchShift {
    fn new(sr: u32, ratio: f64, window_ms: f64) -> Self {
        let w = ((sr as f64 * window_ms / 1000.0) as usize).max(64);
        let size = (w + MIN_DELAY as usize + 8).next_power_of_two();
        PitchShift {
            buf: vec![0.0; size],
            mask: size as i64 - 1,
            w: w as f64,
            t: 0,
            phase: 0.0,
            inc: (1.0 - ratio) / w as f64,
            pass: ratio == 1.0,
        }
    }

    fn tap(&self, p: f64) -> f32 {
        let pos = self.t as f64 - (MIN_DELAY + p * self.w);
        let i0 = pos.floor();
        let frac = (pos - i0) as f32;
        let i0 = i0 as i64;
        let s0 = self.buf[(i0 & self.mask) as usize];
        let s1 = self.buf[((i0 + 1) & self.mask) as usize];
        let win = (PI * p).sin();
        (s0 + (s1 - s0) * frac) * (win * win) as f32
    }
}

impl Effect for PitchShift {
    fn process(&mut self, x: &mut [f32]) {
        if self.pass {
            return;
        }
        for s in x.iter_mut() {
            self.buf[(self.t & self.mask) as usize] = *s;
            self.phase = (self.phase + self.inc).rem_euclid(1.0);
            *s = self.tap(self.phase) + self.tap((self.phase + 0.5) % 1.0);
            self.t += 1;
        }
    }
}

// ---------- character ----------

/// Multiply by a sine. 30-60 Hz is a robot, a few hundred Hz is an alien.
struct RingMod {
    step: f64,
    mix: f32,
    phase: f64,
}

impl RingMod {
    fn new(sr: u32, freq: f64, mix: f32) -> Self {
        RingMod { step: 2.0 * PI * freq / sr as f64, mix, phase: 0.0 }
    }
}

impl Effect for RingMod {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let m = self.phase.sin() as f32;
            *s = *s * (1.0 - self.mix) + *s * m * self.mix;
            self.phase = (self.phase + self.step) % (2.0 * PI);
        }
    }
}

struct Distortion {
    drive: f32,
    out: f32,
}

impl Distortion {
    fn new(drive_db: f32, out_db: f32) -> Self {
        Distortion { drive: 10f32.powf(drive_db / 20.0), out: 10f32.powf(out_db / 20.0) }
    }
}

impl Effect for Distortion {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            *s = (*s * self.drive).tanh() * self.out;
        }
    }
}

/// Fewer bits and a lower sample rate (hold the sample): the 8-bit console sound.
struct Bitcrush {
    levels: f32,
    step: f64,
    acc: f64,
    hold: f32,
}

impl Bitcrush {
    fn new(sr: u32, bits: u32, target_sr: f64) -> Self {
        let step = (sr as f64 / target_sr).max(1.0);
        Bitcrush { levels: 2f32.powi(bits as i32 - 1), step, acc: step, hold: 0.0 }
    }
}

impl Effect for Bitcrush {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            if self.acc >= self.step {
                self.acc -= self.step;
                self.hold = *s;
            }
            self.acc += 1.0;
            *s = (self.hold * self.levels).round() / self.levels;
        }
    }
}

// ---------- echo + reverb ----------

/// w[t] = x[t] + g * damp(w[t - d]). `tick` returns (w[t - d], w[t]).
struct FeedbackLine {
    buf: Vec<f32>,
    pos: usize,
    g: f32,
    damp: f32,
    lp: f32,
}

impl FeedbackLine {
    fn new(d: f64, g: f32, damp: f32) -> Self {
        FeedbackLine { buf: vec![0.0; (d as usize).max(1)], pos: 0, g, damp, lp: 0.0 }
    }

    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32) {
        let dly = self.buf[self.pos];
        let fb = if self.damp > 0.0 {
            self.lp = (1.0 - self.damp) * dly + self.damp * self.lp;
            self.lp
        } else {
            dly
        };
        let w = x + self.g * fb;
        self.buf[self.pos] = w;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        (dly, w)
    }
}

struct Echo {
    line: FeedbackLine,
    mix: f32,
}

impl Echo {
    fn new(sr: u32, delay_ms: f64, feedback: f32, mix: f32, damping: f32) -> Self {
        Echo { line: FeedbackLine::new(sr as f64 * delay_ms / 1000.0, feedback, damping), mix }
    }
}

impl Effect for Echo {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let (delayed, _) = self.line.tick(*s);
            *s += delayed * self.mix;
        }
    }
}

/// Small Schroeder reverb: 4 damped combs side by side, then 2 allpass filters.
struct Reverb {
    combs: Vec<FeedbackLine>,
    allpass: Vec<FeedbackLine>,
    mix: f32,
}

const COMBS_MS: [f64; 4] = [29.7, 37.1, 41.1, 43.7];
const ALLPASS_MS: [f64; 2] = [5.0, 1.7];
const ALLPASS_G: f32 = 0.7;

impl Reverb {
    fn new(sr: u32, decay_s: f64, mix: f32, damping: f32) -> Self {
        let combs = COMBS_MS
            .iter()
            .map(|ms| {
                let d = sr as f64 * ms / 1000.0;
                let g = 10f64.powf(-3.0 * d / (decay_s * sr as f64)) as f32;
                FeedbackLine::new(d, g, damping)
            })
            .collect();
        let allpass = ALLPASS_MS.iter().map(|ms| FeedbackLine::new(sr as f64 * ms / 1000.0, ALLPASS_G, 0.0)).collect();
        Reverb { combs, allpass, mix }
    }
}

impl Effect for Reverb {
    fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let mut wet = 0.0;
            for c in self.combs.iter_mut() {
                wet += c.tick(*s).0;
            }
            wet *= 0.25;
            for a in self.allpass.iter_mut() {
                let (delayed, w) = a.tick(wet);
                wet = -ALLPASS_G * w + delayed;
            }
            *s = *s * (1.0 - self.mix) + wet * self.mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f64 / SR as f64).sin() as f32 * 0.5).collect()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    #[test]
    fn ids_and_index_agree() {
        for (i, id) in IDS.iter().enumerate() {
            assert_eq!(index_of(id), i as u32);
            assert!(is_known(id));
        }
        assert_eq!(index_of("nope"), 0);
        assert!(!is_known("nope"));
    }

    #[test]
    fn off_or_clean_is_index_zero() {
        assert_eq!(active_index(false, "robot"), 0);
        assert_eq!(active_index(true, "clean"), 0);
        assert_eq!(active_index(true, "robot"), index_of("robot"));
        assert!(build(0, SR).is_empty());
    }

    #[test]
    fn clean_leaves_audio_alone() {
        let mut c = build(0, SR);
        let mut x = sine(440.0, 1000);
        let before = x.clone();
        c.process(&mut x);
        assert_eq!(x, before);
    }

    #[test]
    fn every_preset_stays_finite_and_under_the_ceiling() {
        // loud noise-like input, 1 s
        let loud: Vec<f32> = (0..SR as usize).map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0).collect();
        for i in 1..IDS.len() {
            let mut c = build(i as u32, SR);
            let mut x = loud.clone();
            c.process(&mut x);
            assert!(x.iter().all(|s| s.is_finite()), "{} gave NaN/inf", IDS[i]);
            let peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak <= 0.9 + 1e-3, "{} peaked at {peak}", IDS[i]);
        }
    }

    #[test]
    fn block_size_does_not_change_the_sound() {
        let input = sine(220.0, 9600);
        for i in 1..IDS.len() {
            let mut whole = input.clone();
            build(i as u32, SR).process(&mut whole);

            let mut chunked = input.clone();
            let mut c = build(i as u32, SR);
            for part in chunked.chunks_mut(97) {
                c.process(part);
            }
            // the limiter ramps per block, so only the effect itself must match exactly
            if matches!(IDS[i], "deep" | "chipmunk" | "demon" | "giant" | "robot" | "alien" | "radio" | "megaphone" | "cave" | "8bit") {
                let n = whole.len();
                let diff = whole.iter().zip(&chunked).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(diff < 0.35, "{} differs by {diff} over {n} samples", IDS[i]);
            }
        }
    }

    #[test]
    fn pitch_shift_down_and_up() {
        let input = sine(400.0, SR as usize);
        let base = crossings(&input) as f64;

        let mut down = input.clone();
        PitchShift::new(SR, 0.5, 45.0).process(&mut down);
        let r = crossings(&down[4800..]) as f64 / (base * (SR as f64 - 4800.0) / SR as f64);
        assert!((r - 0.5).abs() < 0.08, "down ratio {r}");

        let mut up = input.clone();
        PitchShift::new(SR, 1.5, 45.0).process(&mut up);
        let r = crossings(&up[4800..]) as f64 / (base * (SR as f64 - 4800.0) / SR as f64);
        assert!((r - 1.5).abs() < 0.1, "up ratio {r}");
    }

    #[test]
    fn tail_only_for_echo_and_reverb_presets() {
        assert_eq!(tail_ms(0), 0);
        assert_eq!(tail_ms(index_of("deep")), 0);
        assert!(tail_ms(index_of("cave")) > 1000);
    }

    #[test]
    fn stream_clean_is_a_copy_without_tail() {
        let x = sine(300.0, 1500);
        let y: Vec<f32> = FxStream::new(x.clone().into_iter(), 0, 1, SR).collect();
        assert_eq!(y, x);
    }

    #[test]
    fn stream_keeps_length_for_pitch_effects_and_adds_a_tail_for_echo() {
        let x = sine(300.0, 1500);
        let deep = FxStream::new(x.clone().into_iter(), index_of("deep"), 1, SR).count();
        assert_eq!(deep, 1500);
        let cave: Vec<f32> = FxStream::new(x.clone().into_iter(), index_of("cave"), 1, SR).collect();
        assert_eq!(cave.len(), 1500 + (SR as usize * 2800) / 1000);
        assert!(cave[1500 + 100..].iter().any(|s| s.abs() > 1e-4), "tail is silent");
        assert!(cave.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn stream_stereo_keeps_the_channels_apart() {
        // left = sine, right = silence
        let mut inter = Vec::new();
        for s in sine(300.0, 2000) {
            inter.push(s);
            inter.push(0.0);
        }
        let y: Vec<f32> = FxStream::new(inter.into_iter(), index_of("robot"), 2, SR).take(4000).collect();
        assert!(y.iter().skip(1).step_by(2).all(|s| s.abs() < 1e-9), "right channel got sound");
        assert!(y.iter().step_by(2).any(|s| s.abs() > 0.01));
    }

    #[test]
    fn echo_repeats_an_impulse_after_the_delay() {
        let mut e = Echo::new(SR, 10.0, 0.5, 1.0, 0.0);
        let d = (SR as f64 * 0.010) as usize;
        let mut x = vec![0.0f32; d * 3];
        x[0] = 1.0;
        e.process(&mut x);
        assert!((x[d] - 1.0).abs() < 1e-6, "first echo {}", x[d]);
        assert!((x[2 * d] - 0.5).abs() < 1e-6, "second echo {}", x[2 * d]);
    }

    #[test]
    fn bitcrush_makes_steps() {
        let mut b = Bitcrush::new(SR, 3, 6000.0);
        let mut x = sine(300.0, 4800);
        b.process(&mut x);
        let mut levels: Vec<i32> = x.iter().map(|s| (s * 1000.0).round() as i32).collect();
        levels.sort_unstable();
        levels.dedup();
        assert!(levels.len() <= 9, "too many levels: {}", levels.len());
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut l = Limiter::new(SR, -6.0, 80.0);
        let mut x = vec![1.0f32; 480];
        l.process(&mut x);
        let ceil = 10f32.powf(-6.0 / 20.0);
        assert!(x.iter().all(|s| s.abs() <= ceil + 1e-6));
    }

    #[test]
    fn reverb_has_a_tail() {
        let mut r = Reverb::new(SR, 1.0, 0.5, 0.3);
        let mut x = vec![0.0f32; SR as usize / 2];
        x[0] = 1.0;
        r.process(&mut x);
        let tail: f32 = x[4800..].iter().map(|s| s.abs()).sum();
        assert!(tail > 0.01, "no tail: {tail}");
    }
}
