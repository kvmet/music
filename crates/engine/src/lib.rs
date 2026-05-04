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
/// Maximum audio buffer size we pre-allocate scratch space for. Audio hosts
/// commonly use 64..2048 frames; 4096 covers worst-case sane configurations.
/// If a host exceeds this, `ensure_bufs` will grow the buffers once on first
/// occurrence (one-time allocation, not per-callback).
const MAX_BUFFER_FRAMES: usize = 4096;

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
    pub voice_swing: [i8; VOICES],
    pub millibpm: u32,
    pub delay: DelayParams,
    pub distortion: BusDistortionParams,
    pub reverb: ReverbParams,
    pub compressor: CompressorParams,
}

/// Convert per-voice swing (0..=100) to a tick delay applied to off-beat
/// (odd-indexed) steps. swing=100 delays by half a step (classic upper bound,
/// triplet-feel territory). Values outside [0,100] are clamped.
fn swing_delay_ticks(swing: i8) -> u64 {
    let s = swing.clamp(0, 100) as u64;
    s * TICKS_PER_STEP / 200
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
    /// Per-voice swing in [0, 100]. 0 = straight; off-beat (odd) steps get
    /// delayed by `swing/200` of a step's worth of ticks.
    SetVoiceSwing { voice: usize, swing: i8 },
    /// Trigger a voice immediately at its current defaults, ignoring step
    /// position. Used for live play-in (shift+number). Honors mute.
    TriggerVoice { voice: usize },
    /// Atomically replace pattern, locks, voice params, mutes, tempo, and
    /// global FX *params* from a saved scene.
    ///
    /// **Intentionally preserved across the swap:**
    /// - Transport position. Playback continues from wherever it was; the
    ///   sequencer does not rewind. Use `StopAndRewind` first if a clean
    ///   restart is desired.
    /// - FX processor *state* (delay lines, reverb comb/allpass buffers,
    ///   distortion feedback, compressor gain envelope). Only the FX params
    ///   are swapped; existing tails ring out into the new scene.
    ///
    /// Rationale: scenes are intended for pattern-level switching within a
    /// song, not whole-song transitions. Continuous transport and bleeding
    /// FX tails make those transitions musical. If you need a clean break,
    /// issue `StopAndRewind` then `LoadScene`.
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
    voice_swing: [i8; VOICES],
    /// Deferred trig per voice: (absolute tick, step index). Set when an
    /// off-beat step's swing pushes its fire past the boundary; cleared when
    /// the fire happens or the transport is stopped/rewound. At most one slot
    /// per voice — if a new boundary scheduled fire arrives before the prior
    /// one resolved (rare: requires swing > one full step), the older one is
    /// dropped.
    pending_fire: [Option<(u64, usize)>; VOICES],
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
    /// Heap-owned command payloads (currently `Box<SceneData>`) get sent here
    /// instead of dropping on the audio thread. A background "engine-gc"
    /// thread drains and drops them. Bounded(8) so a burst of scene loads
    /// doesn't unbound the channel; if it ever fills, the box drops on the
    /// audio thread (rare; same as pre-fix behavior).
    gc_tx: Option<Sender<Box<SceneData>>>,
    gc_handle: Option<std::thread::JoinHandle<()>>,
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
        let (gc_tx, gc_rx) = bounded::<Box<SceneData>>(8);
        let gc_handle = std::thread::Builder::new()
            .name("engine-gc".into())
            .spawn(move || {
                // Drain and drop. Loop exits when all senders are dropped
                // (Engine drop sets gc_tx to None first, see Drop impl).
                while let Ok(_dead) = gc_rx.recv() {
                    // _dead drops here, off the audio thread.
                }
            })
            .ok();
        let engine = Self {
            transport: Transport::new(TransportConfig::new(120, sample_rate)),
            pattern: [[Step::default(); STEPS]; VOICES],
            voices,
            voice_defaults,
            muted: [false; VOICES],
            voice_swing: [0; VOICES],
            pending_fire: [None; VOICES],
            voice_buf: vec![0.0; MAX_BUFFER_FRAMES],
            mix_bus_l: vec![0.0; MAX_BUFFER_FRAMES],
            mix_bus_r: vec![0.0; MAX_BUFFER_FRAMES],
            delay_bus: vec![0.0; MAX_BUFFER_FRAMES],
            reverb_bus: vec![0.0; MAX_BUFFER_FRAMES],
            distortion_bus: vec![0.0; MAX_BUFFER_FRAMES],
            delay_wet_l: vec![0.0; MAX_BUFFER_FRAMES],
            delay_wet_r: vec![0.0; MAX_BUFFER_FRAMES],
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
            gc_tx: Some(gc_tx),
            gc_handle,
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

            // Fire any pending trigs scheduled exactly at start_tick.
            self.fire_pending_at(start_tick);
            // Step boundary coinciding with start_tick.
            if start_tick % TICKS_PER_STEP == 0 {
                self.handle_step_boundary(start_tick);
            }

            let mut written = 0usize;
            let mut next_step_tick = (start_tick / TICKS_PER_STEP + 1) * TICKS_PER_STEP;

            while written < total {
                // Next event = earliest of (next step boundary, any pending
                // fire ticks > start_tick). `fire_pending_at(start_tick)` above
                // already fired any trigs scheduled at or before this buffer's
                // start (firing them late at offset 0 rather than dropping).
                let mut next_event = next_step_tick;
                for v in 0..VOICES {
                    if let Some((t, _)) = self.pending_fire[v] {
                        if t > start_tick && t < next_event {
                            next_event = t;
                        }
                    }
                }

                let next_event_sample =
                    Position::from_ticks(next_event).to_sample(&cfg);
                let next_event_offset =
                    next_event_sample.saturating_sub(start_sample) as usize;
                let render_to = next_event_offset.min(total);

                if render_to > written {
                    self.render_segment(written, render_to - written);
                    written = render_to;
                }

                if render_to == next_event_offset && render_to < total {
                    self.fire_pending_at(next_event);
                    if next_event == next_step_tick {
                        self.handle_step_boundary(next_step_tick);
                        next_step_tick += TICKS_PER_STEP;
                    }
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

        // Publish BEFORE advancing the transport so the UI sees the tick
        // corresponding to the audio just rendered, not the tick of the next
        // buffer. Otherwise the UI playhead leads the audio by one buffer.
        self.publish();
        if self.transport.state() == PlaybackState::Playing {
            self.transport.advance_samples(total as u64);
        }
    }

    fn ensure_bufs(&mut self, len: usize) {
        // Buffers are pre-allocated to MAX_BUFFER_FRAMES in `new`. This grow
        // path only triggers if a host requests an unusually large buffer; in
        // that case we accept a one-time allocation here.
        debug_assert!(
            len <= MAX_BUFFER_FRAMES || self.voice_buf.len() >= len,
            "audio buffer exceeded MAX_BUFFER_FRAMES; growing on RT path"
        );
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
            // Mute is implemented in `handle_step_boundary` (skips new triggers).
            // The voice still renders so any in-flight envelope decays naturally.
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

    /// Process a step boundary at `tick`: for each voice with a trig at this
    /// step, either fire immediately (on-beat or zero swing) or queue a
    /// deferred fire (off-beat with swing).
    fn handle_step_boundary(&mut self, tick: u64) {
        let step_idx = ((tick / TICKS_PER_STEP) as usize) % STEPS;
        for v in 0..VOICES {
            if !self.pattern[v][step_idx].on || self.muted[v] {
                continue;
            }
            let delay = if step_idx % 2 == 1 {
                swing_delay_ticks(self.voice_swing[v])
            } else {
                0
            };
            if delay == 0 {
                self.trigger_voice(v, step_idx);
            } else {
                self.pending_fire[v] = Some((tick + delay, step_idx));
            }
        }
    }

    /// Fire and clear any pending trigs whose scheduled tick is at or before
    /// `tick`. Trigs whose scheduled tick is in the past (relative to a buffer
    /// start) fire late at offset 0 of the current buffer, which is preferable
    /// to silently dropping them. Mute is rechecked here so that a voice muted
    /// between the step boundary and its swing-deferred fire stays silent,
    /// matching the muted-on-boundary behavior in `handle_step_boundary`.
    fn fire_pending_at(&mut self, tick: u64) {
        for v in 0..VOICES {
            if let Some((t, idx)) = self.pending_fire[v] {
                if t <= tick {
                    self.pending_fire[v] = None;
                    if self.muted[v] {
                        continue;
                    }
                    self.trigger_voice(v, idx);
                }
            }
        }
    }

    /// Apply (defaults + step locks) to a voice and trigger it. Always
    /// reapplies so a previously-locked step doesn't leak its overrides.
    fn trigger_voice(&mut self, v: usize, step_idx: usize) {
        let mut params = self.voice_defaults[v];
        self.pattern[v][step_idx].locks.merge_into(&mut params);
        self.voices[v].apply_params(params);
        self.voices[v].trigger(1.0);
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
            Command::SetVoiceSwing { voice, swing } => {
                if voice < VOICES {
                    self.voice_swing[voice] = swing.clamp(0, 100);
                }
            }
            Command::TriggerVoice { voice } => {
                if voice < VOICES && !self.muted[voice] {
                    let p = self.voice_defaults[voice];
                    self.voices[voice].apply_params(p);
                    self.voices[voice].trigger(1.0);
                }
            }
            Command::LoadScene(scene) => {
                // Read fields through the Box (Deref) instead of moving out
                // with `*scene`. All SceneData fields are Copy, so this just
                // copies them to engine state without consuming the Box.
                self.voice_defaults = scene.voice_params;
                for v in 0..VOICES {
                    self.voices[v].apply_params(scene.voice_params[v]);
                }
                for v in 0..VOICES {
                    for st in 0..STEPS {
                        self.pattern[v][st] = Step {
                            on: scene.pattern[v][st],
                            locks: scene.locks[v][st],
                        };
                    }
                }
                self.muted = scene.muted;
                self.voice_swing = scene.voice_swing;
                self.pending_fire = [None; VOICES];
                self.transport.set_tempo(scene.millibpm);
                self.delay_params = scene.delay;
                self.bus_distortion_params = scene.distortion;
                self.reverb_params = scene.reverb;
                self.compressor_params = scene.compressor;
                // Hand the box to the GC thread so the heap free happens off
                // the audio thread. If the channel is full or the GC thread is
                // gone, the box drops here (rare fallback; same as pre-fix).
                if let Some(tx) = &self.gc_tx {
                    let _ = tx.try_send(scene);
                }
            }
            Command::Play => self.transport.play(),
            Command::Stop => {
                self.transport.stop();
                self.pending_fire = [None; VOICES];
            }
            Command::StopAndRewind => {
                self.transport.stop();
                self.transport.seek(Position::default());
                self.pending_fire = [None; VOICES];
            }
            Command::RewindAndPlay => {
                self.transport.seek(Position::default());
                self.transport.play();
                self.pending_fire = [None; VOICES];
            }
            Command::SetTempo(mb) => self.transport.set_tempo(mb),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Drop the gc sender so the background thread's recv() returns Err
        // and its loop exits. Then join. The thread drains any remaining
        // boxes in the channel before exiting (recv() returns the queued
        // items first, then Err).
        self.gc_tx.take();
        if let Some(h) = self.gc_handle.take() {
            let _ = h.join();
        }
    }
}
