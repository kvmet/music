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

// --- Drum voice --------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct DrumVoiceParams {
    pub tone_level: f32,
    pub tone_start_hz: f32,
    pub tone_end_hz: f32,
    pub tone_pitch_decay_ms: f32,
    pub tone_amp_attack_ms: f32,
    pub tone_amp_decay_ms: f32,

    pub noise_level: f32,
    pub noise_filter_hz: f32,
    pub noise_filter_mode: FilterMode,
    pub noise_amp_attack_ms: f32,
    pub noise_amp_decay_ms: f32,

    pub click_level: f32,
    pub click_ms: f32,

    pub post_filter_hz: f32,
    pub post_filter_q: f32,
    pub post_filter_mode: FilterMode,

    pub master_gain: f32,
}

/// Per-step parameter overrides. None = inherit voice default.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepLocks {
    pub tone_level: Option<f32>,
    pub tone_start_hz: Option<f32>,
    pub tone_end_hz: Option<f32>,
    pub tone_pitch_decay_ms: Option<f32>,
    pub tone_amp_attack_ms: Option<f32>,
    pub tone_amp_decay_ms: Option<f32>,
    pub noise_level: Option<f32>,
    pub noise_filter_hz: Option<f32>,
    pub noise_filter_mode: Option<FilterMode>,
    pub noise_amp_attack_ms: Option<f32>,
    pub noise_amp_decay_ms: Option<f32>,
    pub click_level: Option<f32>,
    pub click_ms: Option<f32>,
    pub post_filter_hz: Option<f32>,
    pub post_filter_q: Option<f32>,
    pub post_filter_mode: Option<FilterMode>,
    pub master_gain: Option<f32>,
}

impl StepLocks {
    pub fn is_empty(&self) -> bool {
        self.tone_level.is_none()
            && self.tone_start_hz.is_none()
            && self.tone_end_hz.is_none()
            && self.tone_pitch_decay_ms.is_none()
            && self.tone_amp_attack_ms.is_none()
            && self.tone_amp_decay_ms.is_none()
            && self.noise_level.is_none()
            && self.noise_filter_hz.is_none()
            && self.noise_filter_mode.is_none()
            && self.noise_amp_attack_ms.is_none()
            && self.noise_amp_decay_ms.is_none()
            && self.click_level.is_none()
            && self.click_ms.is_none()
            && self.post_filter_hz.is_none()
            && self.post_filter_q.is_none()
            && self.post_filter_mode.is_none()
            && self.master_gain.is_none()
    }

    /// Apply any locked fields onto `p`. Unlocked fields leave `p` untouched.
    pub fn merge_into(&self, p: &mut DrumVoiceParams) {
        if let Some(v) = self.tone_level { p.tone_level = v; }
        if let Some(v) = self.tone_start_hz { p.tone_start_hz = v; }
        if let Some(v) = self.tone_end_hz { p.tone_end_hz = v; }
        if let Some(v) = self.tone_pitch_decay_ms { p.tone_pitch_decay_ms = v; }
        if let Some(v) = self.tone_amp_attack_ms { p.tone_amp_attack_ms = v; }
        if let Some(v) = self.tone_amp_decay_ms { p.tone_amp_decay_ms = v; }
        if let Some(v) = self.noise_level { p.noise_level = v; }
        if let Some(v) = self.noise_filter_hz { p.noise_filter_hz = v; }
        if let Some(v) = self.noise_filter_mode { p.noise_filter_mode = v; }
        if let Some(v) = self.noise_amp_attack_ms { p.noise_amp_attack_ms = v; }
        if let Some(v) = self.noise_amp_decay_ms { p.noise_amp_decay_ms = v; }
        if let Some(v) = self.click_level { p.click_level = v; }
        if let Some(v) = self.click_ms { p.click_ms = v; }
        if let Some(v) = self.post_filter_hz { p.post_filter_hz = v; }
        if let Some(v) = self.post_filter_q { p.post_filter_q = v; }
        if let Some(v) = self.post_filter_mode { p.post_filter_mode = v; }
        if let Some(v) = self.master_gain { p.master_gain = v; }
    }
}

impl Default for DrumVoiceParams {
    fn default() -> Self {
        Self {
            tone_level: 0.0,
            tone_start_hz: 100.0,
            tone_end_hz: 100.0,
            tone_pitch_decay_ms: 50.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 100.0,
            noise_level: 0.0,
            noise_filter_hz: 1000.0,
            noise_filter_mode: FilterMode::Off,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 100.0,
            click_level: 0.0,
            click_ms: 1.0,
            post_filter_hz: 1000.0,
            post_filter_q: 0.707,
            post_filter_mode: FilterMode::Off,
            master_gain: 1.0,
        }
    }
}

pub struct DrumVoice {
    sample_rate: u32,
    params: DrumVoiceParams,

    tone_amp: AdEnv,
    tone_pitch: DecayEnv,
    tone_phase: f32,

    noise_amp: AdEnv,
    noise_filter: OnePole,
    post_filter: Biquad,
    rng: u32,

    click_remaining: u32,
    click_total: u32,

    velocity: f32,
}

impl DrumVoice {
    pub fn new(params: DrumVoiceParams, sample_rate: u32) -> Self {
        let click_total = (params.click_ms / 1000.0 * sample_rate as f32).max(0.0) as u32;
        Self {
            tone_amp: AdEnv::new(params.tone_amp_attack_ms, params.tone_amp_decay_ms, sample_rate),
            tone_pitch: DecayEnv::new(params.tone_pitch_decay_ms, sample_rate),
            tone_phase: 0.0,
            noise_amp: AdEnv::new(params.noise_amp_attack_ms, params.noise_amp_decay_ms, sample_rate),
            noise_filter: OnePole::new(params.noise_filter_hz, params.noise_filter_mode, sample_rate),
            post_filter: Biquad::new(
                params.post_filter_hz,
                params.post_filter_q,
                params.post_filter_mode,
                sample_rate,
            ),
            rng: 0xCAFEBABE,
            click_remaining: 0,
            click_total,
            velocity: 0.0,
            sample_rate,
            params,
        }
    }

    fn next_noise(&mut self) -> f32 {
        // xorshift32 -> [-1, 1)
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as i32 as f32) / (i32::MAX as f32)
    }

    pub fn params(&self) -> &DrumVoiceParams {
        &self.params
    }

    /// Replace params and re-derive precomputed values (env increments,
    /// filter coefficient, click length). Does NOT retrigger.
    pub fn apply_params(&mut self, p: DrumVoiceParams) {
        self.params = p;
        let sr = self.sample_rate;
        self.tone_amp.set_rates(p.tone_amp_attack_ms, p.tone_amp_decay_ms, sr);
        self.tone_pitch.set_rate(p.tone_pitch_decay_ms, sr);
        self.noise_amp.set_rates(p.noise_amp_attack_ms, p.noise_amp_decay_ms, sr);
        self.noise_filter.set_cutoff(p.noise_filter_hz, sr);
        self.noise_filter.set_mode(p.noise_filter_mode);
        self.post_filter
            .set(p.post_filter_hz, p.post_filter_q, p.post_filter_mode, sr);
        self.click_total = (p.click_ms / 1000.0 * sr as f32).max(0.0) as u32;
    }
}

impl Voice for DrumVoice {
    fn trigger(&mut self, velocity: f32) {
        self.velocity = velocity.clamp(0.0, 1.0);
        self.tone_amp.trigger();
        self.tone_pitch.trigger();
        self.tone_phase = 0.0;
        self.noise_amp.trigger();
        self.click_remaining = self.click_total;
    }

    fn render_add(&mut self, out: &mut [f32]) {
        let sr = self.sample_rate as f32;
        let p = self.params;
        for s in out.iter_mut() {
            let mut sample = 0.0;

            // Tone layer
            if p.tone_level > 0.0 && self.tone_amp.is_active() {
                let pitch_env = self.tone_pitch.next();
                let freq = p.tone_end_hz + (p.tone_start_hz - p.tone_end_hz) * pitch_env;
                self.tone_phase += freq / sr;
                if self.tone_phase >= 1.0 {
                    self.tone_phase -= 1.0;
                }
                let osc = (self.tone_phase * TAU).sin();
                let amp = self.tone_amp.next();
                sample += osc * amp * p.tone_level;
            }

            // Noise layer
            if p.noise_level > 0.0 && self.noise_amp.is_active() {
                let n = self.next_noise();
                let filtered = self.noise_filter.process(n);
                let amp = self.noise_amp.next();
                sample += filtered * amp * p.noise_level;
            }

            // Click layer
            if self.click_remaining > 0 {
                let n = self.next_noise();
                let click_amp = self.click_remaining as f32 / self.click_total.max(1) as f32;
                sample += n * click_amp * p.click_level;
                self.click_remaining -= 1;
            }

            // Insert chain (currently just post-filter; drive/fold/crush slot in here later).
            sample = self.post_filter.process(sample);

            *s += sample * p.master_gain * self.velocity;
        }
    }
}

// --- Presets -----------------------------------------------------------------

impl DrumVoiceParams {
    pub fn kick() -> Self {
        Self {
            tone_level: 1.0,
            tone_start_hz: 130.0,
            tone_end_hz: 50.0,
            tone_pitch_decay_ms: 60.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 250.0,
            click_level: 0.5,
            click_ms: 1.5,
            master_gain: 0.8,
            ..Default::default()
        }
    }

    pub fn snare() -> Self {
        Self {
            tone_level: 0.55,
            tone_start_hz: 240.0,
            tone_end_hz: 180.0,
            tone_pitch_decay_ms: 25.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 90.0,
            noise_level: 0.7,
            noise_filter_hz: 1500.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 150.0,
            click_level: 0.3,
            click_ms: 1.0,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn closed_hat() -> Self {
        Self {
            noise_level: 0.9,
            noise_filter_hz: 7000.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 35.0,
            master_gain: 0.5,
            ..Default::default()
        }
    }

    pub fn open_hat() -> Self {
        Self {
            noise_level: 0.85,
            noise_filter_hz: 6500.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 280.0,
            master_gain: 0.45,
            ..Default::default()
        }
    }

    pub fn tom_lo() -> Self {
        Self {
            tone_level: 1.0,
            tone_start_hz: 140.0,
            tone_end_hz: 90.0,
            tone_pitch_decay_ms: 50.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 350.0,
            click_level: 0.2,
            click_ms: 1.0,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn tom_hi() -> Self {
        Self {
            tone_level: 1.0,
            tone_start_hz: 230.0,
            tone_end_hz: 160.0,
            tone_pitch_decay_ms: 40.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 250.0,
            click_level: 0.2,
            click_ms: 1.0,
            master_gain: 0.7,
            ..Default::default()
        }
    }

    pub fn clap() -> Self {
        Self {
            noise_level: 0.95,
            noise_filter_hz: 1200.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 2.0,
            noise_amp_decay_ms: 120.0,
            master_gain: 0.55,
            ..Default::default()
        }
    }

    pub fn rim() -> Self {
        Self {
            tone_level: 0.7,
            tone_start_hz: 1200.0,
            tone_end_hz: 800.0,
            tone_pitch_decay_ms: 8.0,
            tone_amp_attack_ms: 0.5,
            tone_amp_decay_ms: 25.0,
            click_level: 0.5,
            click_ms: 0.6,
            master_gain: 0.6,
            ..Default::default()
        }
    }

    pub fn perc_lo() -> Self {
        Self {
            tone_level: 0.9,
            tone_start_hz: 400.0,
            tone_end_hz: 300.0,
            tone_pitch_decay_ms: 20.0,
            tone_amp_attack_ms: 1.0,
            tone_amp_decay_ms: 80.0,
            noise_level: 0.2,
            noise_filter_hz: 3000.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 1.0,
            noise_amp_decay_ms: 60.0,
            master_gain: 0.6,
            ..Default::default()
        }
    }

    pub fn perc_hi() -> Self {
        Self {
            tone_level: 0.9,
            tone_start_hz: 900.0,
            tone_end_hz: 700.0,
            tone_pitch_decay_ms: 12.0,
            tone_amp_attack_ms: 0.5,
            tone_amp_decay_ms: 50.0,
            noise_level: 0.15,
            noise_filter_hz: 5000.0,
            noise_filter_mode: FilterMode::HighPass,
            noise_amp_attack_ms: 0.5,
            noise_amp_decay_ms: 40.0,
            master_gain: 0.55,
            ..Default::default()
        }
    }
}
