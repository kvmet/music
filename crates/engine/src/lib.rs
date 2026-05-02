use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use synth::{DrumVoice, DrumVoiceParams, StepLocks, Voice};
use transport::{PlaybackState, Position, Transport, TransportConfig};

pub use crossbeam_channel::TrySendError;

pub const VOICES: usize = 10;
pub const STEPS: usize = 16;
pub const TICKS_PER_STEP: u64 = 240; // 16th note at 960 PPQ
const COMMAND_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct Step {
    pub on: bool,
    pub locks: StepLocks,
}

#[derive(Debug, Clone, Copy)]
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
    Play,
    Stop,
    StopAndRewind,
    RewindAndPlay,
    SetTempo(u32), // millibpm
}

/// State the audio thread publishes for the UI to read without locking.
pub struct Shared {
    pub playhead_ticks: AtomicU64,
    pub playing: AtomicBool,
}

impl Shared {
    pub fn current_step(&self) -> usize {
        ((self.playhead_ticks.load(Ordering::Relaxed) / TICKS_PER_STEP) as usize) % STEPS
    }
}

/// Lives on the audio thread. UI talks to it via `Handle::commands`.
pub struct Engine {
    transport: Transport,
    pattern: [[Step; STEPS]; VOICES],
    voices: Vec<DrumVoice>,
    voice_defaults: [DrumVoiceParams; VOICES],
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
        });
        let voice_defaults: [DrumVoiceParams; VOICES] =
            std::array::from_fn(|i| *voices[i].params());
        let engine = Self {
            transport: Transport::new(TransportConfig::new(120, sample_rate)),
            pattern: [[Step::default(); STEPS]; VOICES],
            voices,
            voice_defaults,
            rx,
            shared: shared.clone(),
        };
        (engine, Handle { commands: tx, shared })
    }

    /// Audio callback entry. Fills `out` with mono samples.
    pub fn process(&mut self, out: &mut [f32]) {
        while let Ok(cmd) = self.rx.try_recv() {
            self.apply(cmd);
        }

        for s in out.iter_mut() {
            *s = 0.0;
        }

        if self.transport.state() != PlaybackState::Playing {
            self.publish();
            return;
        }

        let cfg = *self.transport.config();
        let start_sample = self.transport.sample_position();
        let start_tick = self.transport.position().ticks;
        let total = out.len();

        // If we're sitting exactly on a step boundary, fire it before rendering.
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
                let seg = &mut out[written..render_to];
                for v in self.voices.iter_mut() {
                    v.render_add(seg);
                }
                written = render_to;
            }

            if render_to == next_step_offset && render_to < total {
                self.fire_step(next_step_tick);
                next_step_tick += TICKS_PER_STEP;
            } else {
                break;
            }
        }

        self.transport.advance_samples(total as u64);
        self.publish();
    }

    fn fire_step(&mut self, tick: u64) {
        let step_idx = ((tick / TICKS_PER_STEP) as usize) % STEPS;
        for (v, voice) in self.voices.iter_mut().enumerate() {
            let s = &self.pattern[v][step_idx];
            if !s.on {
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
