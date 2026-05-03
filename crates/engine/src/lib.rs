use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use synth::{
    BusDistortion, BusDistortionParams, Compressor, CompressorParams, Delay, DelayParams,
    DrumVoice, DrumVoiceParams, Reverb, ReverbParams, StepLocks, Voice,
};
use transport::{PlaybackState, Position, Transport, TransportConfig};

pub use crossbeam_channel::TrySendError;

/// Equal-power pan. `pan` is -1.0 (full L) .. 1.0 (full R). At 0 both
/// channels return 1/sqrt(2), so summing L+R for a centered voice matches
/// unity loudness.
fn equal_power_pan(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let theta = (p + 1.0) * 0.25 * std::f32::consts::PI; // 0..pi/2
    (theta.cos(), theta.sin())
}

pub const VOICES: usize = 10;
pub const STEPS: usize = 16;
pub const TICKS_PER_STEP: u64 = 240; // 16th note at 960 PPQ
const COMMAND_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct Step {
    pub on: bool,
    pub locks: StepLocks,
}

/// Full snapshot of engine-side state — used for scene save/recall.
#[derive(Clone, Debug)]
pub struct SceneData {
    pub voice_params: [DrumVoiceParams; VOICES],
    pub pattern: [[bool; STEPS]; VOICES],
    pub locks: [[StepLocks; STEPS]; VOICES],
    pub muted: [bool; VOICES],
    pub millibpm: u32,
    pub delay: DelayParams,
    pub distortion: BusDistortionParams,
    pub reverb: ReverbParams,
    pub compressor: CompressorParams,
}

#[derive(Debug, Clone)]
pub enum Command {
    SetStep { voice: usize, step: usize, on: bool },
    /// Update the voice's default params AND apply them live.
    SetVoiceParams { voice: usize, params: DrumVoiceParams },
    /// Apply params to the voice live without changing the default.
    /// Used for step-lock / overdub edits so an in-flight envelope responds.
    /// The next trigger will reset to (defaults + step locks) anyway.
    ApplyVoiceParams { voice: usize, params: DrumVoiceParams },
    SetStepLocks { voice: usize, step: usize, locks: StepLocks },
    ClearStepLocks { voice: usize, step: usize },
    SetDelayParams(DelayParams),
    SetBusDistortionParams(BusDistortionParams),
    SetReverbParams(ReverbParams),
    SetCompressorParams(CompressorParams),
    SetVoiceMuted { voice: usize, muted: bool },
    /// Atomically replace pattern, locks, voice params, mutes, tempo, and
    /// global FX from a saved scene.
    LoadScene(Box<SceneData>),
    Play,
    Stop,
    StopAndRewind,
    RewindAndPlay,
    SetTempo(u32), // millibpm
    SetMasterGain(f32),
}

/// State the audio thread publishes for the UI to read without locking.
/// Peak meters are stored as `f32::to_bits` so we can use plain atomic ops.
pub struct Shared {
    pub playhead_ticks: AtomicU64,
    pub playing: AtomicBool,
    pub peak_l: AtomicU32,
    pub peak_r: AtomicU32,
}

impl Shared {
    pub fn current_step(&self) -> usize {
        ((self.playhead_ticks.load(Ordering::Relaxed) / TICKS_PER_STEP) as usize) % STEPS
    }

    pub fn peak_levels(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak_l.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_r.load(Ordering::Relaxed)),
        )
    }
}

/// Lives on the audio thread. UI talks to it via `Handle::commands`.
pub struct Engine {
    transport: Transport,
    pattern: [[Step; STEPS]; VOICES],
    voices: Vec<DrumVoice>,
    voice_defaults: [DrumVoiceParams; VOICES],
    muted: [bool; VOICES],
    // Mix routing scratch. Reused per buffer; grown lazily.
    voice_buf: Vec<f32>,
    mix_bus_l: Vec<f32>,
    mix_bus_r: Vec<f32>,
    delay_bus: Vec<f32>,
    reverb_bus: Vec<f32>,
    distortion_bus: Vec<f32>,
    delay_wet_l: Vec<f32>,
    delay_wet_r: Vec<f32>,
    delay: Delay,
    delay_params: DelayParams,
    bus_distortion: BusDistortion,
    bus_distortion_params: BusDistortionParams,
    reverb: Reverb,
    reverb_params: ReverbParams,
    compressor: Compressor,
    compressor_params: CompressorParams,
    master_gain: f32,
    meter_l: f32,
    meter_r: f32,
    rx: Receiver<Command>,
    shared: Arc<Shared>,
}

pub struct Handle {
    pub commands: Sender<Command>,
    pub shared: Arc<Shared>,
}

impl Engine {
    pub fn new(sample_rate: u32, voices: Vec<DrumVoice>) -> (Self, Handle) {
        assert_eq!(voices.len(), VOICES, "engine expects exactly {VOICES} voices");
        let (tx, rx) = bounded(COMMAND_CAPACITY);
        let shared = Arc::new(Shared {
            playhead_ticks: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
        });
        let voice_defaults: [DrumVoiceParams; VOICES] =
            std::array::from_fn(|i| *voices[i].params());
        let engine = Self {
            transport: Transport::new(TransportConfig::new(120, sample_rate)),
            pattern: [[Step::default(); STEPS]; VOICES],
            voices,
            voice_defaults,
            muted: [false; VOICES],
            voice_buf: Vec::new(),
            mix_bus_l: Vec::new(),
            mix_bus_r: Vec::new(),
            delay_bus: Vec::new(),
            reverb_bus: Vec::new(),
            distortion_bus: Vec::new(),
            delay_wet_l: Vec::new(),
            delay_wet_r: Vec::new(),
            delay: Delay::new(sample_rate, 2000.0),
            delay_params: DelayParams::default(),
            bus_distortion: BusDistortion::new(sample_rate),
            bus_distortion_params: BusDistortionParams::default(),
            reverb: Reverb::new(sample_rate),
            reverb_params: ReverbParams::default(),
            compressor: Compressor::new(sample_rate),
            compressor_params: CompressorParams::default(),
            master_gain: 1.0,
            meter_l: 0.0,
            meter_r: 0.0,
            rx,
            shared: shared.clone(),
        };
        (engine, Handle { commands: tx, shared })
    }

    /// Audio callback entry. Fills `out_l` and `out_r` with mono per-frame
    /// samples. Both slices must be the same length.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        debug_assert_eq!(out_l.len(), out_r.len());

        while let Ok(cmd) = self.rx.try_recv() {
            self.apply(cmd);
        }

        let total = out_l.len();
        self.ensure_bufs(total);

        // Zero buses for this buffer.
        for s in &mut self.mix_bus_l[..total] { *s = 0.0; }
        for s in &mut self.mix_bus_r[..total] { *s = 0.0; }
        for s in &mut self.delay_bus[..total] { *s = 0.0; }
        for s in &mut self.reverb_bus[..total] { *s = 0.0; }
        for s in &mut self.distortion_bus[..total] { *s = 0.0; }

        // When stopped, we still render voices (so any in-flight envelope
        // decays naturally) and still run the FX chain (so reverb / delay
        // tails ring out). We just skip step firing. This avoids hard cuts
        // on play/stop transitions.
        if self.transport.state() == PlaybackState::Playing {
            let cfg = *self.transport.config();
            let start_sample = self.transport.sample_position();
            let start_tick = self.transport.position().ticks;

            if start_tick % TICKS_PER_STEP == 0 {
                self.fire_step(start_tick);
            }

            let mut written = 0usize;
            let mut next_step_tick = (start_tick / TICKS_PER_STEP + 1) * TICKS_PER_STEP;

            while written < total {
                let next_step_sample =
                    Position::from_ticks(next_step_tick).to_sample(&cfg);
                let next_step_offset =
                    next_step_sample.saturating_sub(start_sample) as usize;
                let render_to = next_step_offset.min(total);

                if render_to > written {
                    self.render_segment(written, render_to - written);
                    written = render_to;
                }

                if render_to == next_step_offset && render_to < total {
                    self.fire_step(next_step_tick);
                    next_step_tick += TICKS_PER_STEP;
                } else {
                    break;
                }
            }
        } else {
            // Render voice tails into the dry mix without firing new steps.
            self.render_segment(0, total);
        }

        // Run global FX on send buses.
        self.delay.process(
            &self.delay_bus[..total],
            &mut self.delay_wet_l[..total],
            &mut self.delay_wet_r[..total],
            self.delay_params,
        );
        self.bus_distortion
            .process(&mut self.distortion_bus[..total], self.bus_distortion_params);
        self.reverb
            .process(&mut self.reverb_bus[..total], self.reverb_params);

        // Sum dry mix + sends into stereo output. Mono FX (reverb, distortion)
        // feed both channels equally; delay is already stereo.
        for i in 0..total {
            let mono_fx = self.distortion_bus[i] + self.reverb_bus[i];
            out_l[i] = self.mix_bus_l[i] + self.delay_wet_l[i] + mono_fx;
            out_r[i] = self.mix_bus_r[i] + self.delay_wet_r[i] + mono_fx;
        }
        self.compressor.process(
            &mut out_l[..total],
            &mut out_r[..total],
            self.compressor_params,
        );

        // Master gain + peak meter (post-master). Fast attack, slow release
        // so the UI sees the peak briefly then bleeds down.
        let g = self.master_gain;
        let mut peak_l = 0f32;
        let mut peak_r = 0f32;
        for i in 0..total {
            out_l[i] *= g;
            out_r[i] *= g;
            peak_l = peak_l.max(out_l[i].abs());
            peak_r = peak_r.max(out_r[i].abs());
        }
        // Single-pole release per buffer, instant attack on rise.
        let release = 0.85;
        self.meter_l = if peak_l > self.meter_l { peak_l } else { self.meter_l * release };
        self.meter_r = if peak_r > self.meter_r { peak_r } else { self.meter_r * release };
        self.shared.peak_l.store(self.meter_l.to_bits(), Ordering::Relaxed);
        self.shared.peak_r.store(self.meter_r.to_bits(), Ordering::Relaxed);

        if self.transport.state() == PlaybackState::Playing {
            self.transport.advance_samples(total as u64);
        }
        self.publish();
    }

    fn ensure_bufs(&mut self, len: usize) {
        if self.voice_buf.len() < len {
            self.voice_buf.resize(len, 0.0);
            self.mix_bus_l.resize(len, 0.0);
            self.mix_bus_r.resize(len, 0.0);
            self.delay_bus.resize(len, 0.0);
            self.reverb_bus.resize(len, 0.0);
            self.distortion_bus.resize(len, 0.0);
            self.delay_wet_l.resize(len, 0.0);
            self.delay_wet_r.resize(len, 0.0);
        }
    }

    /// Render one segment (between step boundaries). For each voice, renders
    /// into a scratch buffer, then sums into the mix bus and (scaled) into
    /// the send buses.
    fn render_segment(&mut self, offset: usize, len: usize) {
        for v_idx in 0..self.voices.len() {
            let (sd, sr, sx, pan_l, pan_r, gain) = {
                let p = self.voices[v_idx].params();
                let (l, r) = equal_power_pan(p.pan);
                (
                    p.send_delay,
                    p.send_reverb,
                    p.send_distortion,
                    l,
                    r,
                    p.master_gain,
                )
            };
            for s in &mut self.voice_buf[..len] { *s = 0.0; }
            self.voices[v_idx].render_add(&mut self.voice_buf[..len]);
            // Mute is implemented in `fire_step` (skips new triggers). The
            // voice still renders so any in-flight envelope decays naturally.
            // Sends tap the pre-fader signal; only the dry mix is scaled by
            // master_gain so users can solo a voice into FX.
            let dry_l = pan_l * gain;
            let dry_r = pan_r * gain;
            for i in 0..len {
                let s = self.voice_buf[i];
                self.mix_bus_l[offset + i] += s * dry_l;
                self.mix_bus_r[offset + i] += s * dry_r;
                self.delay_bus[offset + i] += s * sd;
                self.reverb_bus[offset + i] += s * sr;
                self.distortion_bus[offset + i] += s * sx;
            }
        }
    }

    fn fire_step(&mut self, tick: u64) {
        let step_idx = ((tick / TICKS_PER_STEP) as usize) % STEPS;
        for (v, voice) in self.voices.iter_mut().enumerate() {
            let s = &self.pattern[v][step_idx];
            if !s.on {
                continue;
            }
            // Mute = skip new triggers; in-flight envelopes keep decaying
            // naturally (no clicks).
            if self.muted[v] {
                continue;
            }
            // Always reapply (defaults + locks) so a locked step doesn't leave
            // its overrides stuck on the voice for the next trigger.
            let mut params = self.voice_defaults[v];
            s.locks.merge_into(&mut params);
            voice.apply_params(params);
            voice.trigger(1.0);
        }
    }

    fn publish(&self) {
        self.shared
            .playhead_ticks
            .store(self.transport.position().ticks, Ordering::Relaxed);
        self.shared.playing.store(
            self.transport.state() == PlaybackState::Playing,
            Ordering::Relaxed,
        );
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::SetStep { voice, step, on } => {
                if voice < VOICES && step < STEPS {
                    self.pattern[voice][step].on = on;
                }
            }
            Command::SetVoiceParams { voice, params } => {
                if voice < VOICES {
                    self.voice_defaults[voice] = params;
                    self.voices[voice].apply_params(params);
                }
            }
            Command::ApplyVoiceParams { voice, params } => {
                if voice < VOICES {
                    self.voices[voice].apply_params(params);
                }
            }
            Command::SetStepLocks { voice, step, locks } => {
                if voice < VOICES && step < STEPS {
                    self.pattern[voice][step].locks = locks;
                }
            }
            Command::ClearStepLocks { voice, step } => {
                if voice < VOICES && step < STEPS {
                    self.pattern[voice][step].locks = StepLocks::default();
                }
            }
            Command::SetDelayParams(p) => self.delay_params = p,
            Command::SetBusDistortionParams(p) => self.bus_distortion_params = p,
            Command::SetReverbParams(p) => self.reverb_params = p,
            Command::SetCompressorParams(p) => self.compressor_params = p,
            Command::SetMasterGain(g) => self.master_gain = g,
            Command::SetVoiceMuted { voice, muted } => {
                if voice < VOICES {
                    self.muted[voice] = muted;
                }
            }
            Command::LoadScene(scene) => {
                let s = *scene;
                self.voice_defaults = s.voice_params;
                for v in 0..VOICES {
                    self.voices[v].apply_params(s.voice_params[v]);
                }
                for v in 0..VOICES {
                    for st in 0..STEPS {
                        self.pattern[v][st] = Step {
                            on: s.pattern[v][st],
                            locks: s.locks[v][st],
                        };
                    }
                }
                self.muted = s.muted;
                self.transport.set_tempo(s.millibpm);
                self.delay_params = s.delay;
                self.bus_distortion_params = s.distortion;
                self.reverb_params = s.reverb;
                self.compressor_params = s.compressor;
            }
            Command::Play => self.transport.play(),
            Command::Stop => self.transport.stop(),
            Command::StopAndRewind => {
                self.transport.stop();
                self.transport.seek(Position::default());
            }
            Command::RewindAndPlay => {
                self.transport.seek(Position::default());
                self.transport.play();
            }
            Command::SetTempo(mb) => self.transport.set_tempo(mb),
        }
    }
}
