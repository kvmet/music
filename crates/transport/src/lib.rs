#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSignature {
    pub beats_per_measure: u32,
    pub ticks_per_beat: u32,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            beats_per_measure: 4,
            ticks_per_beat: 960,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    pub millibpm: u32,
    pub sample_rate: u32,
    pub time_signature: TimeSignature,
}

impl TransportConfig {
    pub fn new(bpm: u32, sample_rate: u32) -> Self {
        Self {
            millibpm: bpm * 1000,
            sample_rate,
            time_signature: TimeSignature::default(),
        }
    }

    pub fn new_millibpm(millibpm: u32, sample_rate: u32) -> Self {
        Self {
            millibpm,
            sample_rate,
            time_signature: TimeSignature::default(),
        }
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self::new(120, 44100)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub ticks: u64,
}

impl Position {
    pub fn from_ticks(ticks: u64) -> Self {
        Self { ticks }
    }

    pub fn from_sample(sample: u64, cfg: &TransportConfig) -> Self {
        Self {
            ticks: samples_to_ticks(sample, cfg),
        }
    }

    pub fn from_beat(beat: u64, cfg: &TransportConfig) -> Self {
        Self {
            ticks: beat * cfg.time_signature.ticks_per_beat as u64,
        }
    }

    pub fn from_measure(measure: u64, cfg: &TransportConfig) -> Self {
        let ticks_per_measure = cfg.time_signature.ticks_per_beat as u64
            * cfg.time_signature.beats_per_measure as u64;
        Self {
            ticks: measure * ticks_per_measure,
        }
    }

    pub fn to_sample(&self, cfg: &TransportConfig) -> u64 {
        ticks_to_samples(self.ticks, cfg)
    }

    pub fn beat(&self, cfg: &TransportConfig) -> u64 {
        self.ticks / cfg.time_signature.ticks_per_beat as u64
    }

    pub fn measure(&self, cfg: &TransportConfig) -> u64 {
        let ticks_per_measure = cfg.time_signature.ticks_per_beat as u64
            * cfg.time_signature.beats_per_measure as u64;
        self.ticks / ticks_per_measure
    }

    pub fn tick_in_beat(&self, cfg: &TransportConfig) -> u32 {
        (self.ticks % cfg.time_signature.ticks_per_beat as u64) as u32
    }

    pub fn beat_in_measure(&self, cfg: &TransportConfig) -> u32 {
        (self.beat(cfg) % cfg.time_signature.beats_per_measure as u64) as u32
    }
}

fn ticks_to_samples(ticks: u64, cfg: &TransportConfig) -> u64 {
    let numer = ticks as u128 * cfg.sample_rate as u128 * 60_000u128;
    let denom = cfg.time_signature.ticks_per_beat as u128 * cfg.millibpm as u128;
    ((numer + denom / 2) / denom) as u64
}

fn samples_to_ticks(samples: u64, cfg: &TransportConfig) -> u64 {
    let numer = samples as u128
        * cfg.time_signature.ticks_per_beat as u128
        * cfg.millibpm as u128;
    let denom = cfg.sample_rate as u128 * 60_000u128;
    ((numer + denom / 2) / denom) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
}

pub struct Transport {
    config: TransportConfig,
    position: Position,
    sample_position: u64,
    state: PlaybackState,
}

impl Transport {
    pub fn new(config: TransportConfig) -> Self {
        Self {
            config,
            position: Position::default(),
            sample_position: 0,
            state: PlaybackState::Stopped,
        }
    }

    pub fn play(&mut self) {
        self.state = PlaybackState::Playing;
    }

    pub fn stop(&mut self) {
        self.state = PlaybackState::Stopped;
    }

    pub fn seek(&mut self, position: Position) {
        self.position = position;
        self.sample_position = position.to_sample(&self.config);
    }

    /// Called by the audio thread each buffer. Only advances when playing.
    pub fn advance_samples(&mut self, samples: u64) {
        if self.state != PlaybackState::Playing {
            return;
        }
        self.sample_position += samples;
        self.position = Position::from_sample(self.sample_position, &self.config);
    }

    /// Advance by ticks, for scrubbing and non-audio-driven use.
    pub fn advance_ticks(&mut self, ticks: u64) {
        self.position.ticks += ticks;
        self.sample_position = self.position.to_sample(&self.config);
    }

    pub fn set_tempo(&mut self, millibpm: u32) {
        debug_assert!(millibpm > 0, "millibpm must be > 0");
        self.config.millibpm = millibpm;
        // Resync sample position from ticks so tempo change doesn't cause a jump.
        self.sample_position = self.position.to_sample(&self.config);
    }

    pub fn set_time_signature(&mut self, time_signature: TimeSignature) {
        self.config.time_signature = time_signature;
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn sample_position(&self) -> u64 {
        self.sample_position
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn config(&self) -> &TransportConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TransportConfig {
        TransportConfig::new(120, 44100)
    }

    #[test]
    fn tick_to_sample_roundtrip() {
        let cfg = cfg();
        let ticks = 960 * 4; // one measure
        let samples = ticks_to_samples(ticks, &cfg);
        let back = samples_to_ticks(samples, &cfg);
        assert_eq!(ticks, back);
    }

    #[test]
    fn fractional_bpm() {
        // 120.5 BPM = 120_500 millibpm
        let cfg = TransportConfig::new_millibpm(120_500, 44100);
        let ticks = 960u64;
        let samples = ticks_to_samples(ticks, &cfg);
        let back = samples_to_ticks(samples, &cfg);
        assert_eq!(ticks, back);
    }

    #[test]
    fn position_beat_measure() {
        let cfg = cfg();
        // 2 full measures = 8 beats = 7680 ticks
        let pos = Position::from_measure(2, &cfg);
        assert_eq!(pos.measure(&cfg), 2);
        assert_eq!(pos.beat(&cfg), 8);
        assert_eq!(pos.beat_in_measure(&cfg), 0);
        assert_eq!(pos.tick_in_beat(&cfg), 0);
    }

    #[test]
    fn advance_samples_updates_position() {
        let cfg = cfg();
        let mut t = Transport::new(cfg);
        t.play();
        // one beat at 120 BPM, 44100 Hz = 22050 samples
        let samples_per_beat = ticks_to_samples(960, &cfg);
        t.advance_samples(samples_per_beat);
        assert_eq!(t.position().beat(&cfg), 1);
    }

    #[test]
    fn seek_syncs_sample_position() {
        let cfg = cfg();
        let mut t = Transport::new(cfg);
        let pos = Position::from_beat(4, &cfg);
        t.seek(pos);
        assert_eq!(t.sample_position(), pos.to_sample(&cfg));
    }

    #[test]
    fn advance_while_stopped_is_noop() {
        let cfg = cfg();
        let mut t = Transport::new(cfg);
        t.advance_samples(44100);
        assert_eq!(t.position().ticks, 0);
    }
}
