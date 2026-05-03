//! Drum synth: each voice is three independent layers (tone, noise, click)
//! summed together. Inspired by Nord Drum / synthesized drum modules.
//!
//! Tone:  sine osc with pitch envelope (start_hz -> end_hz) + amp envelope.
//! Noise: white noise -> one-pole filter (LP/HP) + amp envelope.
//! Click: short broadband impulse, linear ramp-down. Adds attack transient.

use std::f32::consts::TAU;

/// Audio voice — produces samples and can be retriggered.
///
/// `render_add` adds into the buffer rather than overwriting, so the engine
/// can sum multiple voices into a single output buffer cheaply.
pub trait Voice: Send {
    fn trigger(&mut self, velocity: f32);
    fn render_add(&mut self, out: &mut [f32]);
}

// --- Envelopes ---------------------------------------------------------------

#[derive(Clone, Copy)]
enum AdState {
    Idle,
    Attack,
    Decay,
}

/// Attack-decay envelope. Linear, 0 -> 1 -> 0.
pub struct AdEnv {
    attack_inc: f32,
    decay_inc: f32,
    state: AdState,
    value: f32,
}

impl AdEnv {
    pub fn new(attack_ms: f32, decay_ms: f32, sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            attack_inc: 1.0 / (attack_ms.max(0.01) / 1000.0 * sr),
            decay_inc: 1.0 / (decay_ms.max(0.01) / 1000.0 * sr),
            state: AdState::Idle,
            value: 0.0,
        }
    }

    pub fn trigger(&mut self) {
        self.state = AdState::Attack;
        self.value = 0.0;
    }

    pub fn next(&mut self) -> f32 {
        match self.state {
            AdState::Attack => {
                self.value += self.attack_inc;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.state = AdState::Decay;
                }
            }
            AdState::Decay => {
                self.value -= self.decay_inc;
                if self.value <= 0.0 {
                    self.value = 0.0;
                    self.state = AdState::Idle;
                }
            }
            AdState::Idle => {}
        }
        self.value
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, AdState::Idle)
    }

    pub fn set_rates(&mut self, attack_ms: f32, decay_ms: f32, sample_rate: u32) {
        let sr = sample_rate as f32;
        self.attack_inc = 1.0 / (attack_ms.max(0.01) / 1000.0 * sr);
        self.decay_inc = 1.0 / (decay_ms.max(0.01) / 1000.0 * sr);
    }
}

/// Decay-only envelope. 1 -> 0. Used for pitch sweeps.
pub struct DecayEnv {
    decay_inc: f32,
    value: f32,
}

impl DecayEnv {
    pub fn new(decay_ms: f32, sample_rate: u32) -> Self {
        Self {
            decay_inc: 1.0 / (decay_ms.max(0.01) / 1000.0 * sample_rate as f32),
            value: 0.0,
        }
    }

    pub fn trigger(&mut self) {
        self.value = 1.0;
    }

    pub fn next(&mut self) -> f32 {
        self.value = (self.value - self.decay_inc).max(0.0);
        self.value
    }

    pub fn set_rate(&mut self, decay_ms: f32, sample_rate: u32) {
        self.decay_inc = 1.0 / (decay_ms.max(0.01) / 1000.0 * sample_rate as f32);
    }
}

// --- Filter ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Off,
    LowPass,
    HighPass,
    /// Band-pass. Only meaningful for `Biquad`; `OnePole` treats it as Off.
    BandPass,
}

/// One-pole filter. Cheap, gentle 6 dB/oct slope. Good enough for noise shaping.
pub struct OnePole {
    a: f32,
    z: f32,
    mode: FilterMode,
}

impl OnePole {
    pub fn new(cutoff_hz: f32, mode: FilterMode, sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let a = (1.0 - (-TAU * cutoff_hz / sr).exp()).clamp(0.0, 1.0);
        Self { a, z: 0.0, mode }
    }

    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: u32) {
        let sr = sample_rate as f32;
        self.a = (1.0 - (-TAU * cutoff_hz / sr).exp()).clamp(0.0, 1.0);
    }

    pub fn set_mode(&mut self, mode: FilterMode) {
        self.mode = mode;
    }

    pub fn process(&mut self, x: f32) -> f32 {
        match self.mode {
            FilterMode::Off | FilterMode::BandPass => x,
            FilterMode::LowPass => {
                self.z += self.a * (x - self.z);
                self.z
            }
            FilterMode::HighPass => {
                self.z += self.a * (x - self.z);
                x - self.z
            }
        }
    }
}

/// Soft-clip drive. `amount` is 0..1, with 0 = clean.
/// Internally maps to a tanh saturation with gain ramping up to 31x at max.
pub fn drive(x: f32, amount: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    if a <= 0.001 {
        return x;
    }
    let gain = 1.0 + a * 30.0;
    let driven = (x * gain).tanh();
    x * (1.0 - a) + driven * a
}

/// Sine wavefolder. `amount` is 0..1, with 0 = clean.
/// At higher amounts, input is gained into a sine which folds back on itself.
pub fn fold(x: f32, amount: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    if a <= 0.001 {
        return x;
    }
    let gain = 1.0 + a * 5.0;
    (x * gain).sin()
}

/// Combined bit-crush + sample-rate reduction. Both inputs are 0..1 amounts.
/// Stateful so SRR can hold samples across calls.
pub struct Crusher {
    held: f32,
    accum: f32,
}

impl Crusher {
    pub fn new() -> Self {
        Self { held: 0.0, accum: 0.0 }
    }

    /// `crush`: 0 = clean (16-bit-ish), 1 = harshly quantized.
    /// `srr`:   0 = full sample rate, 1 = held for ~100 samples.
    pub fn process(&mut self, x: f32, crush: f32, srr: f32) -> f32 {
        // SRR: advance the accumulator at a rate that scales with (1 - srr).
        let step = (1.0 - srr.clamp(0.0, 0.99)).max(1e-3);
        self.accum += step;
        if self.accum >= 1.0 {
            self.accum -= 1.0;
            self.held = x;
        }
        let c = crush.clamp(0.0, 1.0);
        if c <= 0.001 {
            return self.held;
        }
        let bits = 16.0 - 15.0 * c;
        let steps = 2.0_f32.powf(bits - 1.0);
        (self.held * steps).round() / steps
    }
}

impl Default for Crusher {
    fn default() -> Self {
        Self::new()
    }
}

/// Resonant biquad (RBJ cookbook). Direct form I.
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    mode: FilterMode,
}

impl Biquad {
    pub fn new(cutoff_hz: f32, q: f32, mode: FilterMode, sample_rate: u32) -> Self {
        let mut b = Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            mode,
        };
        b.set(cutoff_hz, q, mode, sample_rate);
        b
    }

    pub fn set(&mut self, cutoff_hz: f32, q: f32, mode: FilterMode, sample_rate: u32) {
        self.mode = mode;
        if matches!(mode, FilterMode::Off) {
            return;
        }
        let sr = sample_rate as f32;
        let f = cutoff_hz.clamp(20.0, sr * 0.49);
        let q = q.max(0.5);
        let omega = TAU * f / sr;
        let cos_w = omega.cos();
        let sin_w = omega.sin();
        let alpha = sin_w / (2.0 * q);
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;
        let (b0, b1, b2) = match mode {
            FilterMode::LowPass => {
                let k = (1.0 - cos_w) / 2.0;
                (k, 1.0 - cos_w, k)
            }
            FilterMode::HighPass => {
                let k = (1.0 + cos_w) / 2.0;
                (k, -(1.0 + cos_w), k)
            }
            FilterMode::BandPass => (alpha, 0.0, -alpha),
            FilterMode::Off => unreachable!(),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub fn process(&mut self, x: f32) -> f32 {
        if matches!(self.mode, FilterMode::Off) {
            return x;
        }
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

// --- Waveforms / noise colors -----------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Sine,
    Triangle,
    Saw,
    Square,
}

fn wave_sample(wave: Wave, phase: f32) -> f32 {
    // `phase` is 0..1.
    match wave {
        Wave::Sine => (phase * TAU).sin(),
        Wave::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
        Wave::Saw => 2.0 * phase - 1.0,
        Wave::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseColor {
    White,
    Pink,
    /// Sample-and-hold noise at a fixed slow rate. Adds a "grainy" crackle.
    Grain,
    /// 16-bit LFSR clocked at `noise_filter_hz`. Tunable digital / chip noise.
    /// (Filter still applies on top.)
    Digital,
}

/// Paul Kellet's IIR pink-noise filter state.
#[derive(Default)]
struct PinkState {
    b: [f32; 7],
}

impl PinkState {
    fn next(&mut self, white: f32) -> f32 {
        self.b[0] = 0.99886 * self.b[0] + white * 0.0555179;
        self.b[1] = 0.99332 * self.b[1] + white * 0.0750759;
        self.b[2] = 0.96900 * self.b[2] + white * 0.1538520;
        self.b[3] = 0.86650 * self.b[3] + white * 0.3104856;
        self.b[4] = 0.55000 * self.b[4] + white * 0.5329522;
        self.b[5] = -0.7616 * self.b[5] - white * 0.0168980;
        let out = self.b[0] + self.b[1] + self.b[2] + self.b[3] + self.b[4]
            + self.b[5] + self.b[6] + white * 0.5362;
        self.b[6] = white * 0.115926;
        // Compensate level — Kellet's output sits around ±2.
        out * 0.11
    }
}

#[derive(Default)]
struct GrainState {
    held: f32,
    counter: u32,
}

impl GrainState {
    fn next(&mut self, white: f32, sample_rate: u32) -> f32 {
        // ~500Hz S&H rate independent of sample rate.
        let interval = (sample_rate / 500).max(1);
        if self.counter == 0 {
            self.held = white;
        }
        self.counter = (self.counter + 1) % interval;
        self.held
    }
}

/// Tunable LFSR digital noise. Clocks bits out at `rate_hz`.
struct DigitalState {
    lfsr: u32,
    accum: f32,
    held: f32,
}

impl Default for DigitalState {
    fn default() -> Self {
        Self {
            lfsr: 0xACE1,
            accum: 0.0,
            held: 0.0,
        }
    }
}

impl DigitalState {
    fn next(&mut self, rate_hz: f32, sample_rate: u32) -> f32 {
        let step = (rate_hz.max(1.0) / sample_rate as f32).clamp(0.0, 1.0);
        self.accum += step;
        while self.accum >= 1.0 {
            self.accum -= 1.0;
            // 16-bit Galois LFSR — taps at 16, 14, 13, 11.
            let bit =
                (self.lfsr ^ (self.lfsr >> 2) ^ (self.lfsr >> 3) ^ (self.lfsr >> 5)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 15);
            self.held = if (self.lfsr & 1) == 0 { 1.0 } else { -1.0 };
        }
        self.held
    }
}

// --- Drum voice --------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct DrumVoiceParams {
    // Osc 1 (main, with pitch envelope).
    pub osc1_wave: Wave,
    pub osc1_level: f32,
    pub osc1_start_hz: f32,
    pub osc1_end_hz: f32,
    pub osc1_pitch_decay_ms: f32,
    pub osc1_amp_attack_ms: f32,
    pub osc1_amp_decay_ms: f32,

    // Osc 2 (modulator / secondary). Tracks osc1's current pitch via ratio.
    pub osc2_wave: Wave,
    pub osc2_level: f32,
    pub osc2_ratio: f32, // multiplier of osc1's instantaneous freq

    /// Osc2 → osc1 frequency modulation index (0 = no FM).
    pub fm_amount: f32,

    // Noise (filtered, with own envelope).
    pub noise_color: NoiseColor,
    pub noise_level: f32,
    pub noise_filter_hz: f32,
    pub noise_filter_mode: FilterMode,
    pub noise_amp_attack_ms: f32,
    pub noise_amp_decay_ms: f32,

    pub drive: f32,
    pub fold: f32,
    pub crush: f32,
    pub srr: f32,

    pub post_filter_hz: f32,
    pub post_filter_q: f32,
    pub post_filter_mode: FilterMode,

    pub send_delay: f32,
    pub send_reverb: f32,
    pub send_distortion: f32,

    pub master_gain: f32,
    pub pan: f32, // -1.0 (L) .. 1.0 (R), equal-power
}

/// Per-step parameter overrides. None = inherit voice default.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepLocks {
    pub osc1_wave: Option<Wave>,
    pub osc1_level: Option<f32>,
    pub osc1_start_hz: Option<f32>,
    pub osc1_end_hz: Option<f32>,
    pub osc1_pitch_decay_ms: Option<f32>,
    pub osc1_amp_attack_ms: Option<f32>,
    pub osc1_amp_decay_ms: Option<f32>,
    pub osc2_wave: Option<Wave>,
    pub osc2_level: Option<f32>,
    pub osc2_ratio: Option<f32>,
    pub fm_amount: Option<f32>,
    pub noise_color: Option<NoiseColor>,
    pub noise_level: Option<f32>,
    pub noise_filter_hz: Option<f32>,
    pub noise_filter_mode: Option<FilterMode>,
    pub noise_amp_attack_ms: Option<f32>,
    pub noise_amp_decay_ms: Option<f32>,
    pub drive: Option<f32>,
    pub fold: Option<f32>,
    pub crush: Option<f32>,
    pub srr: Option<f32>,
    pub post_filter_hz: Option<f32>,
    pub post_filter_q: Option<f32>,
    pub post_filter_mode: Option<FilterMode>,
    pub send_delay: Option<f32>,
    pub send_reverb: Option<f32>,
    pub send_distortion: Option<f32>,
    pub master_gain: Option<f32>,
    pub pan: Option<f32>,
}

impl StepLocks {
    pub fn is_empty(&self) -> bool {
        self.osc1_wave.is_none()
            && self.osc1_level.is_none()
            && self.osc1_start_hz.is_none()
            && self.osc1_end_hz.is_none()
            && self.osc1_pitch_decay_ms.is_none()
            && self.osc1_amp_attack_ms.is_none()
            && self.osc1_amp_decay_ms.is_none()
            && self.osc2_wave.is_none()
            && self.osc2_level.is_none()
            && self.osc2_ratio.is_none()
            && self.fm_amount.is_none()
            && self.noise_color.is_none()
            && self.noise_level.is_none()
            && self.noise_filter_hz.is_none()
            && self.noise_filter_mode.is_none()
            && self.noise_amp_attack_ms.is_none()
            && self.noise_amp_decay_ms.is_none()
            && self.drive.is_none()
            && self.fold.is_none()
            && self.crush.is_none()
            && self.srr.is_none()
            && self.post_filter_hz.is_none()
            && self.post_filter_q.is_none()
            && self.post_filter_mode.is_none()
            && self.send_delay.is_none()
            && self.send_reverb.is_none()
            && self.send_distortion.is_none()
            && self.master_gain.is_none()
            && self.pan.is_none()
    }

    /// Apply any locked fields onto `p`. Unlocked fields leave `p` untouched.
    pub fn merge_into(&self, p: &mut DrumVoiceParams) {
        if let Some(v) = self.osc1_wave { p.osc1_wave = v; }
        if let Some(v) = self.osc1_level { p.osc1_level = v; }
        if let Some(v) = self.osc1_start_hz { p.osc1_start_hz = v; }
        if let Some(v) = self.osc1_end_hz { p.osc1_end_hz = v; }
        if let Some(v) = self.osc1_pitch_decay_ms { p.osc1_pitch_decay_ms = v; }
        if let Some(v) = self.osc1_amp_attack_ms { p.osc1_amp_attack_ms = v; }
        if let Some(v) = self.osc1_amp_decay_ms { p.osc1_amp_decay_ms = v; }
        if let Some(v) = self.osc2_wave { p.osc2_wave = v; }
        if let Some(v) = self.osc2_level { p.osc2_level = v; }
        if let Some(v) = self.osc2_ratio { p.osc2_ratio = v; }
        if let Some(v) = self.fm_amount { p.fm_amount = v; }
        if let Some(v) = self.noise_color { p.noise_color = v; }
        if let Some(v) = self.noise_level { p.noise_level = v; }
        if let Some(v) = self.noise_filter_hz { p.noise_filter_hz = v; }
        if let Some(v) = self.noise_filter_mode { p.noise_filter_mode = v; }
        if let Some(v) = self.noise_amp_attack_ms { p.noise_amp_attack_ms = v; }
        if let Some(v) = self.noise_amp_decay_ms { p.noise_amp_decay_ms = v; }
        if let Some(v) = self.drive { p.drive = v; }
        if let Some(v) = self.fold { p.fold = v; }
        if let Some(v) = self.crush { p.crush = v; }
        if let Some(v) = self.srr { p.srr = v; }
        if let Some(v) = self.post_filter_hz { p.post_filter_hz = v; }
        if let Some(v) = self.post_filter_q { p.post_filter_q = v; }
        if let Some(v) = self.post_filter_mode { p.post_filter_mode = v; }
        if let Some(v) = self.send_delay { p.send_delay = v; }
        if let Some(v) = self.send_reverb { p.send_reverb = v; }
        if let Some(v) = self.send_distortion { p.send_distortion = v; }
        if let Some(v) = self.master_gain { p.master_gain = v; }
        if let Some(v) = self.pan { p.pan = v; }
    }
}

impl Default for DrumVoiceParams {
    fn default() -> Self {
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 0.0,
            osc1_start_hz: 100.0,
            osc1_end_hz: 100.0,
            osc1_pitch_decay_ms: 50.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 100.0,
            osc2_wave: Wave::Sine,
            osc2_level: 0.0,
            osc2_ratio: 1.0,
            fm_amount: 0.0,
            noise_color: NoiseColor::White,
            noise_level: 0.0,
            noise_filter_hz: 1000.0,
            noise_filter_mode: FilterMode::Off,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 100.0,
            drive: 0.0,
            fold: 0.0,
            crush: 0.0,
            srr: 0.0,
            post_filter_hz: 1000.0,
            post_filter_q: 0.707,
            post_filter_mode: FilterMode::Off,
            send_delay: 0.0,
            send_reverb: 0.0,
            send_distortion: 0.0,
            master_gain: 1.0,
            pan: 0.0,
        }
    }
}

pub struct DrumVoice {
    sample_rate: u32,
    params: DrumVoiceParams,

    tone_amp: AdEnv,
    pitch_env: DecayEnv,
    osc1_phase: f32,
    osc2_phase: f32,

    noise_amp: AdEnv,
    noise_filter: OnePole,
    pink: PinkState,
    grain: GrainState,
    digital: DigitalState,
    crusher: Crusher,
    post_filter: Biquad,
    rng: u32,

    velocity: f32,
}

impl DrumVoice {
    pub fn new(params: DrumVoiceParams, sample_rate: u32) -> Self {
        Self {
            tone_amp: AdEnv::new(params.osc1_amp_attack_ms, params.osc1_amp_decay_ms, sample_rate),
            pitch_env: DecayEnv::new(params.osc1_pitch_decay_ms, sample_rate),
            osc1_phase: 0.0,
            osc2_phase: 0.0,
            noise_amp: AdEnv::new(params.noise_amp_attack_ms, params.noise_amp_decay_ms, sample_rate),
            noise_filter: OnePole::new(params.noise_filter_hz, params.noise_filter_mode, sample_rate),
            pink: PinkState::default(),
            grain: GrainState::default(),
            digital: DigitalState::default(),
            crusher: Crusher::new(),
            post_filter: Biquad::new(
                params.post_filter_hz,
                params.post_filter_q,
                params.post_filter_mode,
                sample_rate,
            ),
            rng: 0xCAFEBABE,
            velocity: 0.0,
            sample_rate,
            params,
        }
    }

    fn next_white(&mut self) -> f32 {
        // xorshift32 -> [-1, 1)
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as i32 as f32) / (i32::MAX as f32)
    }

    fn next_noise(&mut self) -> f32 {
        let white = self.next_white();
        match self.params.noise_color {
            NoiseColor::White => white,
            NoiseColor::Pink => self.pink.next(white),
            NoiseColor::Grain => self.grain.next(white, self.sample_rate),
            NoiseColor::Digital => self
                .digital
                .next(self.params.noise_filter_hz, self.sample_rate),
        }
    }

    pub fn params(&self) -> &DrumVoiceParams {
        &self.params
    }

    /// Replace params and re-derive precomputed values. Does NOT retrigger.
    pub fn apply_params(&mut self, p: DrumVoiceParams) {
        self.params = p;
        let sr = self.sample_rate;
        self.tone_amp.set_rates(p.osc1_amp_attack_ms, p.osc1_amp_decay_ms, sr);
        self.pitch_env.set_rate(p.osc1_pitch_decay_ms, sr);
        self.noise_amp.set_rates(p.noise_amp_attack_ms, p.noise_amp_decay_ms, sr);
        self.noise_filter.set_cutoff(p.noise_filter_hz, sr);
        self.noise_filter.set_mode(p.noise_filter_mode);
        self.post_filter
            .set(p.post_filter_hz, p.post_filter_q, p.post_filter_mode, sr);
    }
}

impl Voice for DrumVoice {
    fn trigger(&mut self, velocity: f32) {
        self.velocity = velocity.clamp(0.0, 1.0);
        self.tone_amp.trigger();
        self.pitch_env.trigger();
        self.osc1_phase = 0.0;
        self.osc2_phase = 0.0;
        self.noise_amp.trigger();
    }

    fn render_add(&mut self, out: &mut [f32]) {
        let sr = self.sample_rate as f32;
        let p = self.params;
        for s in out.iter_mut() {
            let mut sample = 0.0;

            // Tone block: osc2 modulates osc1's frequency (FM); both summed.
            let tone_active =
                self.tone_amp.is_active() && (p.osc1_level > 0.0 || p.osc2_level > 0.0);
            if tone_active {
                let pitch = self.pitch_env.next();
                let base_freq = p.osc1_end_hz + (p.osc1_start_hz - p.osc1_end_hz) * pitch;

                // Osc2 first — its current sample becomes osc1's FM input.
                let mod_freq = (base_freq * p.osc2_ratio).max(0.01);
                self.osc2_phase += mod_freq / sr;
                if self.osc2_phase >= 1.0 {
                    self.osc2_phase -= self.osc2_phase.floor();
                }
                let osc2 = wave_sample(p.osc2_wave, self.osc2_phase);

                // FM: deviation = mod_freq * index. Slider 0..1 → modulation
                // index 0..10 (deep enough for clangy / bell-like tones).
                let deviation = mod_freq * p.fm_amount * 10.0;
                let f1 = (base_freq + osc2 * deviation).max(0.0);
                self.osc1_phase += f1 / sr;
                if self.osc1_phase >= 1.0 {
                    self.osc1_phase -= self.osc1_phase.floor();
                }
                let osc1 = wave_sample(p.osc1_wave, self.osc1_phase);

                let amp = self.tone_amp.next();
                sample += osc1 * amp * p.osc1_level;
                sample += osc2 * amp * p.osc2_level;
            }

            // Noise layer
            if p.noise_level > 0.0 && self.noise_amp.is_active() {
                let n = self.next_noise();
                let filtered = self.noise_filter.process(n);
                let amp = self.noise_amp.next();
                sample += filtered * amp * p.noise_level;
            }

            // Insert chain: drive → fold → crush+srr → post-filter.
            sample = drive(sample, p.drive);
            sample = fold(sample, p.fold);
            sample = self.crusher.process(sample, p.crush, p.srr);
            sample = self.post_filter.process(sample);

            // master_gain is applied by the engine to the dry mix only, so
            // sends remain at full level even when "out" is turned down.
            *s += sample * self.velocity;
        }
    }
}

// --- Global FX ---------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct DelayParams {
    pub time_ms: f32,
    pub feedback: f32, // 0..0.95
    pub lpf_hz: f32,   // one-pole on the feedback path
    pub ping_pong: f32, // 0 = parallel mono, 1 = full L<->R cross-feedback
}

impl Default for DelayParams {
    fn default() -> Self {
        Self {
            time_ms: 300.0,
            feedback: 0.4,
            lpf_hz: 4000.0,
            ping_pong: 0.0,
        }
    }
}

/// Mono-in / stereo-out delay with one-pole low-pass on the feedback path.
/// At ping_pong = 0 the two delay lines run in parallel (identical L+R).
/// At ping_pong = 1 the input lands on L only and feedback crosses fully L<->R.
pub struct Delay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_idx: usize,
    sample_rate: u32,
    fb_z_l: f32,
    fb_z_r: f32,
}

impl Delay {
    pub fn new(sample_rate: u32, max_time_ms: f32) -> Self {
        let len = ((max_time_ms / 1000.0) * sample_rate as f32).ceil() as usize + 1;
        let len = len.max(2);
        Self {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            write_idx: 0,
            sample_rate,
            fb_z_l: 0.0,
            fb_z_r: 0.0,
        }
    }

    /// `input` is the mono send signal. `out_l` / `out_r` receive the wet
    /// (delayed) signal. All three slices must be the same length.
    pub fn process(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32], params: DelayParams) {
        let sr = self.sample_rate as f32;
        let buf_len = self.buf_l.len();
        let delay_samples = ((params.time_ms.max(1.0) / 1000.0) * sr) as usize;
        let delay_samples = delay_samples.clamp(1, buf_len - 1);
        let fb = params.feedback.clamp(0.0, 0.95);
        let lpf_a =
            (1.0 - (-TAU * params.lpf_hz.clamp(20.0, sr * 0.49) / sr).exp()).clamp(0.0, 1.0);
        let pp = params.ping_pong.clamp(0.0, 1.0);
        // Input distribution: ping_pong tilts the input to L only.
        let in_l = 1.0 - 0.5 * pp;
        let in_r = 1.0 - pp;
        // Feedback routing: pp=0 → straight (L→L, R→R); pp=1 → fully crossed.
        let self_fb = 1.0 - pp;
        let cross_fb = pp;

        for i in 0..input.len() {
            let s = input[i];
            let read_idx = (self.write_idx + buf_len - delay_samples) % buf_len;
            let dl = self.buf_l[read_idx];
            let dr = self.buf_r[read_idx];
            self.fb_z_l += lpf_a * (dl - self.fb_z_l);
            self.fb_z_r += lpf_a * (dr - self.fb_z_r);
            self.buf_l[self.write_idx] =
                s * in_l + (self.fb_z_l * self_fb + self.fb_z_r * cross_fb) * fb;
            self.buf_r[self.write_idx] =
                s * in_r + (self.fb_z_r * self_fb + self.fb_z_l * cross_fb) * fb;
            self.write_idx = (self.write_idx + 1) % buf_len;
            out_l[i] = dl;
            out_r[i] = dr;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BusDistortionParams {
    pub stage1_drive: f32, // 0..1
    pub stage1_tone: f32,  // 0=dark, 1=bright (post-clip LPF)
    pub stage2_drive: f32,
    pub stage2_tone: f32,
    /// Pre-clip DC offset on stage 2 — shifts asymmetric clip thresholds and
    /// creates octave-up character at high amounts. 0 = bypass.
    pub bias: f32,
    /// Cross-stage feedback (stage 2 output → stage 1 input, one-sample delayed).
    /// Creates squelchy, oscillating "dying battery" character. 0 = bypass.
    pub feedback: f32,
    /// Hard noise gate at the end of the chain. Higher values = higher threshold,
    /// making the gate close between waveform cycles for sputter / Velcro fuzz
    /// character. 0 = bypass.
    pub gate: f32,
    pub output: f32, // 0..2
}

impl Default for BusDistortionParams {
    fn default() -> Self {
        Self {
            stage1_drive: 0.14,
            stage1_tone: 0.36,
            stage2_drive: 0.30,
            stage2_tone: 0.65,
            bias: 0.25,
            feedback: 0.0,
            gate: 0.0,
            output: 0.42,
        }
    }
}

/// King-of-Tone-inspired bus distortion. Two cascaded stages, each with a
/// pre-clip HPF (so bass passes clean), a clipping curve, and a post-clip
/// tone-shape LPF.
///
/// Stage 1: tanh soft clip (transparent OD).
/// Stage 2: asymmetric soft saturation with higher gain (harsher distortion).
pub struct BusDistortion {
    stage1_pre: Biquad,
    stage1_post: Biquad,
    stage2_pre: Biquad,
    stage2_post: Biquad,
    fb_state: f32,
    gate_env: f32,
    gate_smooth: f32,
    // DC-blocker state (one-pole HPF). Necessary because `bias` shifts the
    // stage 2 operating point, which produces a constant offset at the
    // output even with zero input.
    dc_x_prev: f32,
    dc_y_prev: f32,
    dc_r: f32,
    sample_rate: u32,
}

impl BusDistortion {
    pub fn new(sample_rate: u32) -> Self {
        // ~5 Hz corner: R = exp(-2*pi*fc/sr).
        let dc_r = (-TAU * 5.0 / sample_rate as f32).exp();
        Self {
            stage1_pre: Biquad::new(80.0, 0.707, FilterMode::HighPass, sample_rate),
            stage1_post: Biquad::new(2500.0, 0.707, FilterMode::LowPass, sample_rate),
            stage2_pre: Biquad::new(120.0, 0.707, FilterMode::HighPass, sample_rate),
            stage2_post: Biquad::new(2000.0, 0.707, FilterMode::LowPass, sample_rate),
            fb_state: 0.0,
            gate_env: 0.0,
            gate_smooth: 0.0,
            dc_x_prev: 0.0,
            dc_y_prev: 0.0,
            dc_r,
            sample_rate,
        }
    }

    pub fn process(&mut self, io: &mut [f32], params: BusDistortionParams) {
        let tone_to_hz = |t: f32| {
            let t = t.clamp(0.0, 1.0);
            // 200Hz (dark) → 14kHz (bright), log-mapped — wider range so the
            // top end can really scream when stage 2 is opened up.
            200.0 * (14000.0_f32 / 200.0).powf(t)
        };
        self.stage1_post.set(
            tone_to_hz(params.stage1_tone),
            0.707,
            FilterMode::LowPass,
            self.sample_rate,
        );
        self.stage2_post.set(
            tone_to_hz(params.stage2_tone),
            0.707,
            FilterMode::LowPass,
            self.sample_rate,
        );

        // Stage 1 saturates harder (was 1..31). Still tanh, transparent OD character.
        let g1 = 1.0 + params.stage1_drive.clamp(0.0, 1.0) * 80.0;
        // Stage 2 is now an asymmetric hard-clip after a tanh pre-saturator.
        // At max drive it produces a near-square wave with shifted clip points,
        // so the harmonic content explodes (odd + even).
        let g2 = 1.0 + params.stage2_drive.clamp(0.0, 1.0) * 150.0;
        let bias = params.bias.clamp(0.0, 1.0) * 0.5;
        let fb_amount = params.feedback.clamp(0.0, 0.4);
        let gate_amt = params.gate.clamp(0.0, 1.0);
        // Quadratic threshold so the lower half of the slider has finer control.
        let gate_threshold = gate_amt * gate_amt * 0.7;
        let out_gain = params.output.clamp(0.0, 2.0);

        for s in io.iter_mut() {
            // Cross-stage feedback: last sample of fully-processed signal mixes
            // back into the input one sample late. Soft-limited via tanh on store.
            let mut x = *s + self.fb_state * fb_amount;

            // Stage 1: HPF → tanh → tone LPF.
            x = self.stage1_pre.process(x);
            x = (x * g1).tanh();
            x = self.stage1_post.process(x);

            // Stage 2: HPF → bias → tanh → asymmetric hard clip → tone LPF.
            x = self.stage2_pre.process(x);
            let pre = ((x + bias) * g2).tanh();
            x = pre.clamp(-0.55, 0.85);
            x = self.stage2_post.process(x);

            // Save tanh-bounded value as next sample's feedback source.
            // Lower internal scaling so feedback ramps in smoothly across the slider.
            self.fb_state = (x * 0.5).tanh();

            // Optional sputter gate. Peak follower with fast 3%/sample release
            // (~0.7 ms half-life at 44.1k) lets the envelope dip between waveform
            // cycles, so a high threshold makes the gate flutter.
            if gate_amt > 0.001 {
                let abs_x = x.abs();
                self.gate_env = (self.gate_env * 0.97).max(abs_x);
                let target = if self.gate_env > gate_threshold { 1.0 } else { 0.0 };
                self.gate_smooth += 0.5 * (target - self.gate_smooth);
                x *= self.gate_smooth;
            }

            // DC blocker: y[n] = x[n] - x[n-1] + R * y[n-1]
            let xn = x;
            let yn = xn - self.dc_x_prev + self.dc_r * self.dc_y_prev;
            self.dc_x_prev = xn;
            self.dc_y_prev = yn;

            *s = yn * out_gain;
        }
    }
}

// --- Reverb ------------------------------------------------------------------

/// Freeverb-style mono reverb: 8 parallel comb filters with damping LPFs in
/// feedback, followed by 4 allpass filters in series for diffusion.
const COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];

struct Comb {
    buf: Vec<f32>,
    idx: usize,
    z: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
            z: 0.0,
        }
    }
    fn process(&mut self, x: f32, fb: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.z = out * (1.0 - damp) + self.z * damp;
        self.buf[self.idx] = x + self.z * fb;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }
    fn process(&mut self, x: f32, fb: f32) -> f32 {
        let buffered = self.buf[self.idx];
        let out = buffered - x;
        self.buf[self.idx] = x + buffered * fb;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReverbParams {
    pub size: f32,   // 0..1, longer tail
    pub damp: f32,   // 0..1, more high-freq damping
    pub output: f32, // 0..2
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            size: 0.6,
            damp: 0.4,
            output: 1.0,
        }
    }
}

pub struct Reverb {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Reverb {
    pub fn new(sample_rate: u32) -> Self {
        let scale = sample_rate as f32 / 44100.0;
        let combs = COMB_TUNINGS
            .iter()
            .map(|&t| Comb::new(((t as f32) * scale) as usize))
            .collect();
        let allpasses = ALLPASS_TUNINGS
            .iter()
            .map(|&t| Allpass::new(((t as f32) * scale) as usize))
            .collect();
        Self { combs, allpasses }
    }

    pub fn process(&mut self, io: &mut [f32], params: ReverbParams) {
        let fb = 0.70 + params.size.clamp(0.0, 1.0) * 0.28;
        let damp = params.damp.clamp(0.0, 1.0) * 0.5;
        let allpass_fb = 0.5;
        let out_gain = params.output.clamp(0.0, 2.0);

        for s in io.iter_mut() {
            let input = *s;
            let mut wet = 0.0;
            for comb in &mut self.combs {
                wet += comb.process(input, fb, damp);
            }
            for ap in &mut self.allpasses {
                wet = ap.process(wet, allpass_fb);
            }
            // Normalize: 8 combs in parallel, scale down.
            *s = wet * 0.125 * out_gain;
        }
    }
}

// --- Master compressor -------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct CompressorParams {
    pub threshold_db: f32, // -60..0
    pub ratio: f32,        // 1..20 (1 = no compression)
    pub attack_ms: f32,    // 0.1..200
    pub release_ms: f32,   // 5..2000
    pub makeup_db: f32,    // -12..24
    /// Downward gate. Signal below this level is muted (smoothed). Set very
    /// low (e.g. -120) to effectively disable.
    pub gate_db: f32, // -120..0
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold_db: 0.0,
            ratio: 1.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            gate_db: -120.0,
        }
    }
}

pub struct Compressor {
    sample_rate: u32,
    env_db: f32,   // smoothed level in dB
    gate_open: f32, // 0..1, smoothed gate gain
}

impl Compressor {
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate, env_db: -120.0, gate_open: 0.0 }
    }

    /// Stereo, linked detector (envelope follows max(|L|,|R|)) so the image
    /// stays put under heavy compression. `l` and `r` must be the same length.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], params: CompressorParams) {
        let sr = self.sample_rate as f32;
        let atk_t = (params.attack_ms.max(0.1)) * 0.001;
        let rel_t = (params.release_ms.max(0.1)) * 0.001;
        let atk_a = (-1.0 / (atk_t * sr)).exp();
        let rel_a = (-1.0 / (rel_t * sr)).exp();
        let thr = params.threshold_db;
        let ratio = params.ratio.max(1.0);
        let makeup_lin = 10f32.powf(params.makeup_db / 20.0);
        let inv_ratio = 1.0 / ratio;
        let gate_thr = params.gate_db;
        // ~5 ms open / ~80 ms close — short enough to grab transients, long
        // enough to avoid chatter on quiet content.
        let gate_open_a = (-1.0 / (0.005 * sr)).exp();
        let gate_close_a = (-1.0 / (0.080 * sr)).exp();
        // 6 dB hysteresis so the gate doesn't chatter right at the threshold.
        let gate_close_thr = gate_thr - 6.0;

        for i in 0..l.len() {
            let abs = l[i].abs().max(r[i].abs()).max(1e-9);
            let in_db = 20.0 * abs.log10();
            let a = if in_db > self.env_db { atk_a } else { rel_a };
            self.env_db = a * self.env_db + (1.0 - a) * in_db;

            // Gate target: open when above the upper threshold, closed when
            // below the lower (hysteresis). In between, hold current state.
            let target = if self.env_db > gate_thr {
                1.0
            } else if self.env_db < gate_close_thr {
                0.0
            } else {
                self.gate_open
            };
            let gate_a = if target > self.gate_open { gate_open_a } else { gate_close_a };
            self.gate_open = gate_a * self.gate_open + (1.0 - gate_a) * target;

            let over = self.env_db - thr;
            let gain_db = if over > 0.0 { -over * (1.0 - inv_ratio) } else { 0.0 };
            let gain_lin = 10f32.powf(gain_db / 20.0) * makeup_lin * self.gate_open;
            l[i] *= gain_lin;
            r[i] *= gain_lin;
        }
    }
}

// --- Presets -----------------------------------------------------------------

impl DrumVoiceParams {
    pub fn kick() -> Self {
        // Sine kick + sub triangle one octave below for thump. Light drive.
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 1.0,
            osc1_start_hz: 130.0,
            osc1_end_hz: 50.0,
            osc1_pitch_decay_ms: 55.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 250.0,
            osc2_wave: Wave::Triangle,
            osc2_level: 0.25,
            osc2_ratio: 0.5,
            drive: 0.15,
            master_gain: 0.8,
            ..Default::default()
        }
    }

    pub fn snare() -> Self {
        // Triangle body (richer than sine) + pink noise for warmth + drive for snap.
        Self {
            osc1_wave: Wave::Triangle,
            osc1_level: 0.5,
            osc1_start_hz: 240.0,
            osc1_end_hz: 180.0,
            osc1_pitch_decay_ms: 25.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 90.0,
            noise_color: NoiseColor::Pink,
            noise_level: 0.75,
            noise_filter_hz: 1800.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 150.0,
            drive: 0.2,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn closed_hat() -> Self {
        // Digital LFSR clocked high for that 8-bit / chip hat character.
        Self {
            noise_color: NoiseColor::Digital,
            noise_level: 0.85,
            noise_filter_hz: 6500.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 35.0,
            master_gain: 0.5,
            ..Default::default()
        }
    }

    pub fn open_hat() -> Self {
        // Pink noise for an analog-style open hat with a longer tail.
        Self {
            noise_color: NoiseColor::Pink,
            noise_level: 0.85,
            noise_filter_hz: 5500.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 320.0,
            master_gain: 0.45,
            ..Default::default()
        }
    }

    pub fn tom_lo() -> Self {
        // Subtle FM gives the tom a touch of clang without going full bell.
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 1.0,
            osc1_start_hz: 160.0,
            osc1_end_hz: 95.0,
            osc1_pitch_decay_ms: 60.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 380.0,
            osc2_wave: Wave::Sine,
            osc2_level: 0.0,
            osc2_ratio: 1.4,
            fm_amount: 0.12,
            drive: 0.08,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn tom_hi() -> Self {
        // Higher inharmonic ratio for a brighter, more bell-tinged tom.
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 1.0,
            osc1_start_hz: 260.0,
            osc1_end_hz: 165.0,
            osc1_pitch_decay_ms: 45.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 270.0,
            osc2_wave: Wave::Sine,
            osc2_level: 0.0,
            osc2_ratio: 2.7,
            fm_amount: 0.18,
            drive: 0.08,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn clap() -> Self {
        // Grain noise gives the clap a granular / handclap-y texture.
        Self {
            noise_color: NoiseColor::Grain,
            noise_level: 0.95,
            noise_filter_hz: 1400.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 2.0,
            noise_amp_decay_ms: 130.0,
            drive: 0.15,
            master_gain: 0.55,
            ..Default::default()
        }
    }

    pub fn rim() -> Self {
        // Square + drive for a sharp, plastic-y rim click.
        Self {
            osc1_wave: Wave::Square,
            osc1_level: 0.55,
            osc1_start_hz: 1400.0,
            osc1_end_hz: 800.0,
            osc1_pitch_decay_ms: 6.0,
            osc1_amp_attack_ms: 0.5,
            osc1_amp_decay_ms: 22.0,
            drive: 0.3,
            master_gain: 0.6,
            ..Default::default()
        }
    }

    pub fn perc_lo() -> Self {
        // Heavy FM + fold for a metallic, bell-like perc.
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 0.85,
            osc1_start_hz: 420.0,
            osc1_end_hz: 280.0,
            osc1_pitch_decay_ms: 18.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 90.0,
            osc2_wave: Wave::Triangle,
            osc2_level: 0.1,
            osc2_ratio: 3.6,
            fm_amount: 0.35,
            fold: 0.12,
            master_gain: 0.6,
            ..Default::default()
        }
    }

    pub fn bass() -> Self {
        Self {
            osc1_wave: Wave::Sine,
            osc1_level: 0.63,
            osc1_start_hz: 800.0,
            osc1_end_hz: 100.0,
            osc1_pitch_decay_ms: 5.0,
            osc1_amp_attack_ms: 1.0,
            osc1_amp_decay_ms: 800.0,
            osc2_wave: Wave::Sine,
            osc2_level: 0.09,
            osc2_ratio: 0.5,
            fm_amount: 0.18,
            noise_color: NoiseColor::White,
            noise_level: 0.20,
            noise_filter_hz: 1000.0,
            noise_filter_mode: FilterMode::Off,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 100.0,
            drive: 0.20,
            fold: 0.10,
            crush: 0.0,
            srr: 0.0,
            post_filter_hz: 1000.0,
            post_filter_q: 0.71,
            post_filter_mode: FilterMode::Off,
            send_delay: 0.0,
            send_reverb: 0.0,
            send_distortion: 0.0,
            master_gain: 0.70,
            pan: 0.0,
        }
    }
}
