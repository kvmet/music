use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use engine::{Command, Engine, Handle, STEPS, VOICES};
use record::Recorder;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use synth::{
    BusDistortionParams, DelayParams, DrumVoice, DrumVoiceParams, FilterMode, NoiseColor,
    ReverbParams, StepLocks, Wave,
};

const VOICE_NAMES: [&str; VOICES] = [
    "kick", "snare", "closed hat", "open hat", "tom lo", "tom hi", "clap", "rim",
    "perc lo", "bass",
];

/// Discrete edit destination for slider changes.
#[derive(Clone, Copy, Debug)]
enum EditTarget {
    /// Update voice defaults.
    VoiceDefault,
    /// Write directly to a step's locks (selected voice).
    StepLock(usize),
    /// Accumulate into per-voice overdub buffer; applied at trigger time.
    Overdub(usize),
}

/// One slider change → one of these.
#[derive(Clone, Copy, Debug)]
enum FieldEdit {
    Osc1Wave(Wave),
    Osc1Level(f32),
    Osc1StartHz(f32),
    Osc1EndHz(f32),
    Osc1PitchDecayMs(f32),
    Osc1AmpAttackMs(f32),
    Osc1AmpDecayMs(f32),
    Osc2Wave(Wave),
    Osc2Level(f32),
    Osc2Ratio(f32),
    FmAmount(f32),
    NoiseColor(NoiseColor),
    NoiseLevel(f32),
    NoiseFilterHz(f32),
    NoiseFilterMode(FilterMode),
    NoiseAmpAttackMs(f32),
    NoiseAmpDecayMs(f32),
    Drive(f32),
    Fold(f32),
    Crush(f32),
    Srr(f32),
    PostFilterHz(f32),
    PostFilterQ(f32),
    PostFilterMode(FilterMode),
    SendDelay(f32),
    SendReverb(f32),
    SendDistortion(f32),
    MasterGain(f32),
}

impl FieldEdit {
    fn apply_to_params(self, p: &mut DrumVoiceParams) {
        match self {
            FieldEdit::Osc1Wave(v) => p.osc1_wave = v,
            FieldEdit::Osc1Level(v) => p.osc1_level = v,
            FieldEdit::Osc1StartHz(v) => p.osc1_start_hz = v,
            FieldEdit::Osc1EndHz(v) => p.osc1_end_hz = v,
            FieldEdit::Osc1PitchDecayMs(v) => p.osc1_pitch_decay_ms = v,
            FieldEdit::Osc1AmpAttackMs(v) => p.osc1_amp_attack_ms = v,
            FieldEdit::Osc1AmpDecayMs(v) => p.osc1_amp_decay_ms = v,
            FieldEdit::Osc2Wave(v) => p.osc2_wave = v,
            FieldEdit::Osc2Level(v) => p.osc2_level = v,
            FieldEdit::Osc2Ratio(v) => p.osc2_ratio = v,
            FieldEdit::FmAmount(v) => p.fm_amount = v,
            FieldEdit::NoiseColor(v) => p.noise_color = v,
            FieldEdit::NoiseLevel(v) => p.noise_level = v,
            FieldEdit::NoiseFilterHz(v) => p.noise_filter_hz = v,
            FieldEdit::NoiseFilterMode(v) => p.noise_filter_mode = v,
            FieldEdit::NoiseAmpAttackMs(v) => p.noise_amp_attack_ms = v,
            FieldEdit::NoiseAmpDecayMs(v) => p.noise_amp_decay_ms = v,
            FieldEdit::Drive(v) => p.drive = v,
            FieldEdit::Fold(v) => p.fold = v,
            FieldEdit::Crush(v) => p.crush = v,
            FieldEdit::Srr(v) => p.srr = v,
            FieldEdit::PostFilterHz(v) => p.post_filter_hz = v,
            FieldEdit::PostFilterQ(v) => p.post_filter_q = v,
            FieldEdit::PostFilterMode(v) => p.post_filter_mode = v,
            FieldEdit::SendDelay(v) => p.send_delay = v,
            FieldEdit::SendReverb(v) => p.send_reverb = v,
            FieldEdit::SendDistortion(v) => p.send_distortion = v,
            FieldEdit::MasterGain(v) => p.master_gain = v,
        }
    }

    fn apply_to_locks(self, l: &mut StepLocks) {
        match self {
            FieldEdit::Osc1Wave(v) => l.osc1_wave = Some(v),
            FieldEdit::Osc1Level(v) => l.osc1_level = Some(v),
            FieldEdit::Osc1StartHz(v) => l.osc1_start_hz = Some(v),
            FieldEdit::Osc1EndHz(v) => l.osc1_end_hz = Some(v),
            FieldEdit::Osc1PitchDecayMs(v) => l.osc1_pitch_decay_ms = Some(v),
            FieldEdit::Osc1AmpAttackMs(v) => l.osc1_amp_attack_ms = Some(v),
            FieldEdit::Osc1AmpDecayMs(v) => l.osc1_amp_decay_ms = Some(v),
            FieldEdit::Osc2Wave(v) => l.osc2_wave = Some(v),
            FieldEdit::Osc2Level(v) => l.osc2_level = Some(v),
            FieldEdit::Osc2Ratio(v) => l.osc2_ratio = Some(v),
            FieldEdit::FmAmount(v) => l.fm_amount = Some(v),
            FieldEdit::NoiseColor(v) => l.noise_color = Some(v),
            FieldEdit::NoiseLevel(v) => l.noise_level = Some(v),
            FieldEdit::NoiseFilterHz(v) => l.noise_filter_hz = Some(v),
            FieldEdit::NoiseFilterMode(v) => l.noise_filter_mode = Some(v),
            FieldEdit::NoiseAmpAttackMs(v) => l.noise_amp_attack_ms = Some(v),
            FieldEdit::NoiseAmpDecayMs(v) => l.noise_amp_decay_ms = Some(v),
            FieldEdit::Drive(v) => l.drive = Some(v),
            FieldEdit::Fold(v) => l.fold = Some(v),
            FieldEdit::Crush(v) => l.crush = Some(v),
            FieldEdit::Srr(v) => l.srr = Some(v),
            FieldEdit::PostFilterHz(v) => l.post_filter_hz = Some(v),
            FieldEdit::PostFilterQ(v) => l.post_filter_q = Some(v),
            FieldEdit::PostFilterMode(v) => l.post_filter_mode = Some(v),
            FieldEdit::SendDelay(v) => l.send_delay = Some(v),
            FieldEdit::SendReverb(v) => l.send_reverb = Some(v),
            FieldEdit::SendDistortion(v) => l.send_distortion = Some(v),
            FieldEdit::MasterGain(v) => l.master_gain = Some(v),
        }
    }
}

struct App {
    handle: Handle,
    pattern: [[bool; STEPS]; VOICES], // UI mirror; engine has authoritative copy.
    voice_params: [DrumVoiceParams; VOICES], // UI mirror of synth voice defaults.
    locks: [[StepLocks; STEPS]; VOICES], // UI mirror of step locks.
    delay_params: DelayParams,
    bus_distortion_params: BusDistortionParams,
    reverb_params: ReverbParams,
    selected_voice: usize,
    bpm: u32,

    // Stage 2: held keys + overdub.
    held_steps: [bool; STEPS],
    held_voices: [bool; VOICES],
    /// True if a held step has had a param edit during the hold; suppresses toggle on release.
    step_edited: [bool; STEPS],
    /// Per-voice overdub buffer. Filled while a number is held; applied at triggers.
    overdub_locks: [StepLocks; VOICES],
    /// Last current_step we observed; used to detect step transitions for overdub commit.
    last_step: Option<usize>,
    /// Per-voice mute state (UI mirror of engine).
    muted: [bool; VOICES],
    recorder: Arc<Recorder>,

    _stream: cpal::Stream, // hold to keep audio alive
}

impl App {
    fn new(
        handle: Handle,
        stream: cpal::Stream,
        voice_params: [DrumVoiceParams; VOICES],
        recorder: Arc<Recorder>,
    ) -> Self {
        Self {
            handle,
            pattern: [[false; STEPS]; VOICES],
            voice_params,
            locks: [[StepLocks::default(); STEPS]; VOICES],
            delay_params: DelayParams::default(),
            bus_distortion_params: BusDistortionParams::default(),
            reverb_params: ReverbParams::default(),
            selected_voice: 0,
            bpm: 120,
            held_steps: [false; STEPS],
            held_voices: [false; VOICES],
            step_edited: [false; STEPS],
            overdub_locks: [StepLocks::default(); VOICES],
            last_step: None,
            muted: [false; VOICES],
            recorder,
            _stream: stream,
        }
    }

    fn send(&self, cmd: Command) {
        // Bounded channel; if it ever fills, dropping a UI command is acceptable.
        let _ = self.handle.commands.try_send(cmd);
    }

    fn toggle_step(&mut self, voice: usize, step: usize) {
        let on = !self.pattern[voice][step];
        self.pattern[voice][step] = on;
        self.send(Command::SetStep { voice, step, on });
        if !on {
            // Clearing a trigger also clears its locks.
            self.locks[voice][step] = StepLocks::default();
            self.send(Command::ClearStepLocks { voice, step });
        }
    }

    fn editor_target(&self) -> EditTarget {
        // Step held wins over number held. First found, in step-key order.
        if let Some(step) = (0..STEPS).find(|s| self.held_steps[*s]) {
            return EditTarget::StepLock(step);
        }
        if self.playing() {
            if let Some(v) = (0..VOICES).find(|v| self.held_voices[*v]) {
                return EditTarget::Overdub(v);
            }
        }
        EditTarget::VoiceDefault
    }

    /// Currently audible params for `voice`: voice defaults overlaid with the
    /// most-recently-fired active step's locks (if any). Always returns a value
    /// so the marker stays put and only moves when there's actual motion.
    fn playing_marker_params(&self, voice: usize) -> DrumVoiceParams {
        let mut p = self.voice_params[voice];
        if self.playing() {
            if let Some(s) = self.last_step {
                if self.pattern[voice][s] {
                    self.locks[voice][s].merge_into(&mut p);
                }
            }
        }
        p
    }

    /// What params to display in the editor for the current target. Selected voice as base.
    fn editor_params(&self) -> DrumVoiceParams {
        let v = self.selected_voice;
        let mut p = self.voice_params[v];
        match self.editor_target() {
            EditTarget::VoiceDefault => p,
            EditTarget::StepLock(step) => {
                self.locks[v][step].merge_into(&mut p);
                p
            }
            EditTarget::Overdub(vv) => {
                // Overdub displays for the held voice; if it differs from selected, use it.
                let mut base = self.voice_params[vv];
                self.overdub_locks[vv].merge_into(&mut base);
                base
            }
        }
    }

    fn apply_field_edit(&mut self, edit: FieldEdit) {
        match self.editor_target() {
            EditTarget::VoiceDefault => {
                let v = self.selected_voice;
                edit.apply_to_params(&mut self.voice_params[v]);
                self.send(Command::SetVoiceParams {
                    voice: v,
                    params: self.voice_params[v],
                });
            }
            EditTarget::StepLock(step) => {
                let v = self.selected_voice;
                self.step_edited[step] = true;
                edit.apply_to_locks(&mut self.locks[v][step]);
                self.send(Command::SetStepLocks {
                    voice: v,
                    step,
                    locks: self.locks[v][step],
                });
                // Live apply so an in-flight voice hears the change immediately.
                let mut merged = self.voice_params[v];
                self.locks[v][step].merge_into(&mut merged);
                self.send(Command::ApplyVoiceParams { voice: v, params: merged });
            }
            EditTarget::Overdub(v) => {
                edit.apply_to_locks(&mut self.overdub_locks[v]);
                // Live apply: stack overdub on top of last-fired step's locks if any,
                // otherwise on top of voice defaults.
                let mut merged = self.voice_params[v];
                if let Some(s) = self.last_step {
                    if self.pattern[v][s] {
                        self.locks[v][s].merge_into(&mut merged);
                    }
                }
                self.overdub_locks[v].merge_into(&mut merged);
                self.send(Command::ApplyVoiceParams { voice: v, params: merged });
            }
        }
    }

    /// Once per frame: detect playhead step transitions and commit any active
    /// overdub buffers into steps that just fired.
    fn commit_overdubs(&mut self) {
        if !self.playing() {
            self.last_step = None;
            return;
        }
        let cur = self.current_step();
        let entered_new_step = self.last_step != Some(cur);
        self.last_step = Some(cur);
        if !entered_new_step {
            return;
        }
        for v in 0..VOICES {
            if !self.held_voices[v] {
                continue;
            }
            if self.overdub_locks[v].is_empty() {
                continue;
            }
            if !self.pattern[v][cur] {
                continue;
            }
            // Merge overdub fields into existing step locks.
            overlay_locks(&mut self.locks[v][cur], &self.overdub_locks[v]);
            self.send(Command::SetStepLocks {
                voice: v,
                step: cur,
                locks: self.locks[v][cur],
            });
            // The voice was just triggered with old params at this step boundary;
            // apply the new merged params live so the in-flight envelope picks them up.
            let mut merged = self.voice_params[v];
            self.locks[v][cur].merge_into(&mut merged);
            self.send(Command::ApplyVoiceParams { voice: v, params: merged });
        }
    }

    fn playing(&self) -> bool {
        self.handle.shared.playing.load(Ordering::Relaxed)
    }

    fn current_step(&self) -> usize {
        self.handle.shared.current_step()
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::Key::*;
        // Suppress egui's default Tab focus navigation; we use Tab as a held
        // modifier for mute toggling.
        ctx.input_mut(|i| {
            let _ = i.consume_key(egui::Modifiers::NONE, Tab);
            let _ = i.consume_key(egui::Modifiers::SHIFT, Tab);
        });

        // Bars 1-2 land on the left hand (Q-R / A-F), bars 3-4 on the right
        // (U-P / J-;). Steps 1-16 = Q W E R | A S D F | U I O P | J K L ;
        let step_keys = [Q, W, E, R, A, S, D, F, U, I, O, P, J, K, L, Semicolon];
        let num_keys = [
            Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, Num0,
        ];

        // Snapshot input.
        let (
            step_down,
            step_released,
            shift_step_pressed,
            num_down,
            num_pressed,
            tab_down,
            plain_space,
            shift_space,
        ) = ctx.input(|i| {
            let shift = i.modifiers.shift;
            let mut step_down = [false; STEPS];
            let mut step_released = [false; STEPS];
            let mut shift_step_pressed = [false; STEPS];
            for (idx, k) in step_keys.iter().enumerate() {
                step_down[idx] = i.key_down(*k);
                step_released[idx] = i.key_released(*k);
                if i.key_pressed(*k) && shift {
                    shift_step_pressed[idx] = true;
                }
            }
            let mut num_down = [false; VOICES];
            let mut num_pressed = [false; VOICES];
            for (idx, k) in num_keys.iter().enumerate() {
                num_down[idx] = i.key_down(*k);
                num_pressed[idx] = i.key_pressed(*k);
            }
            let tab_down = i.key_down(Tab);
            let space = i.key_pressed(Space);
            (
                step_down,
                step_released,
                shift_step_pressed,
                num_down,
                num_pressed,
                tab_down,
                space && !shift,
                space && shift,
            )
        });

        // Step keys: maintain held state. On press, either clear locks (if shift)
        // or arm for tap-toggle. On release, if no edits happened during the hold,
        // toggle the step on/off.
        for s in 0..STEPS {
            let was_held = self.held_steps[s];
            self.held_steps[s] = step_down[s];
            if step_down[s] && !was_held {
                // Rising edge of press.
                if shift_step_pressed[s] {
                    let v = self.selected_voice;
                    self.locks[v][s] = StepLocks::default();
                    self.send(Command::ClearStepLocks { voice: v, step: s });
                    self.step_edited[s] = true; // suppress toggle on release
                } else {
                    self.step_edited[s] = false;
                }
            }
            if step_released[s] {
                if !self.step_edited[s] {
                    let v = self.selected_voice;
                    self.toggle_step(v, s);
                }
                self.step_edited[s] = false;
            }
        }

        // Number keys: tap = select voice; holding = enables overdub for that voice.
        // Tab+number = toggle mute (skips both selection and overdub tracking).
        if tab_down {
            for v in 0..VOICES {
                if num_pressed[v] {
                    self.muted[v] = !self.muted[v];
                    self.send(Command::SetVoiceMuted {
                        voice: v,
                        muted: self.muted[v],
                    });
                }
            }
        } else {
            for v in 0..VOICES {
                let was_held = self.held_voices[v];
                self.held_voices[v] = num_down[v];
                if num_pressed[v] {
                    self.selected_voice = v;
                }
                if was_held && !num_down[v] {
                    self.overdub_locks[v] = StepLocks::default();
                }
            }
        }

        if shift_space {
            if self.playing() {
                self.send(Command::StopAndRewind);
            } else {
                self.send(Command::RewindAndPlay);
            }
        } else if plain_space {
            if self.playing() {
                self.send(Command::Stop);
            } else {
                self.send(Command::Play);
            }
        }
    }
}

// TODO: slider row sizing (label width, value-text width, bar share) is rough.
// Tighten once the rest of the layout settles.
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    log: bool,
    marker: f32,
    mut on_change: impl FnMut(f32),
) {
    ui.horizontal(|ui| {
        ui.add_sized([90.0, 18.0], egui::Label::new(label));
        // Reserve a shape slot before adding the slider so the marker sits behind it.
        let marker_slot = ui.painter().add(egui::Shape::Noop);
        let mut v = value;
        let mut s = egui::Slider::new(&mut v, range.clone()).show_value(false);
        if log {
            s = s.logarithmic(true);
        }
        let resp = ui.add(s);
        if resp.changed() {
            on_change(v);
        }
        ui.painter()
            .set(marker_slot, marker_shape(resp.rect, marker, &range, log));
        ui.add_sized([54.0, 18.0], egui::Label::new(format_param_value(value)));
    });
}

fn format_param_value(v: f32) -> String {
    if v.abs() >= 100.0 {
        format!("{:.0}", v)
    } else if v.abs() >= 10.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    }
}

fn marker_shape(
    rect: egui::Rect,
    value: f32,
    range: &std::ops::RangeInclusive<f32>,
    log: bool,
) -> egui::Shape {
    let lo = *range.start();
    let hi = *range.end();
    let t = if log {
        let l_lo = (lo.max(1e-6) as f64).log2();
        let l_hi = (hi.max(1e-6) as f64).log2();
        let l_v = (value.max(1e-6) as f64).log2();
        ((l_v - l_lo) / (l_hi - l_lo)).clamp(0.0, 1.0) as f32
    } else {
        ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    };
    // Match egui slider's handle-radius padding so the marker aligns with the knob travel.
    let pad = 7.0;
    let bar_left = rect.left() + pad;
    let bar_right = rect.right() - pad;
    let x = bar_left + t * (bar_right - bar_left);
    let half_w = 2.5;
    // Span the slider's full height so a sliver of the marker shows above and
    // below the slider's own track regardless of track opacity.
    let r = egui::Rect::from_min_max(
        egui::pos2(x - half_w, rect.top()),
        egui::pos2(x + half_w, rect.bottom()),
    );
    egui::Shape::rect_filled(r, 1.5, MARKER_COLOR)
}

/// Copy any Some fields from `src` into `target`, leaving target's other fields alone.
fn overlay_locks(target: &mut StepLocks, src: &StepLocks) {
    if src.osc1_wave.is_some() { target.osc1_wave = src.osc1_wave; }
    if src.osc1_level.is_some() { target.osc1_level = src.osc1_level; }
    if src.osc1_start_hz.is_some() { target.osc1_start_hz = src.osc1_start_hz; }
    if src.osc1_end_hz.is_some() { target.osc1_end_hz = src.osc1_end_hz; }
    if src.osc1_pitch_decay_ms.is_some() { target.osc1_pitch_decay_ms = src.osc1_pitch_decay_ms; }
    if src.osc1_amp_attack_ms.is_some() { target.osc1_amp_attack_ms = src.osc1_amp_attack_ms; }
    if src.osc1_amp_decay_ms.is_some() { target.osc1_amp_decay_ms = src.osc1_amp_decay_ms; }
    if src.osc2_wave.is_some() { target.osc2_wave = src.osc2_wave; }
    if src.osc2_level.is_some() { target.osc2_level = src.osc2_level; }
    if src.osc2_ratio.is_some() { target.osc2_ratio = src.osc2_ratio; }
    if src.fm_amount.is_some() { target.fm_amount = src.fm_amount; }
    if src.noise_color.is_some() { target.noise_color = src.noise_color; }
    if src.noise_level.is_some() { target.noise_level = src.noise_level; }
    if src.noise_filter_hz.is_some() { target.noise_filter_hz = src.noise_filter_hz; }
    if src.noise_filter_mode.is_some() { target.noise_filter_mode = src.noise_filter_mode; }
    if src.noise_amp_attack_ms.is_some() { target.noise_amp_attack_ms = src.noise_amp_attack_ms; }
    if src.noise_amp_decay_ms.is_some() { target.noise_amp_decay_ms = src.noise_amp_decay_ms; }
    if src.drive.is_some() { target.drive = src.drive; }
    if src.fold.is_some() { target.fold = src.fold; }
    if src.crush.is_some() { target.crush = src.crush; }
    if src.srr.is_some() { target.srr = src.srr; }
    if src.post_filter_hz.is_some() { target.post_filter_hz = src.post_filter_hz; }
    if src.post_filter_q.is_some() { target.post_filter_q = src.post_filter_q; }
    if src.post_filter_mode.is_some() { target.post_filter_mode = src.post_filter_mode; }
    if src.send_delay.is_some() { target.send_delay = src.send_delay; }
    if src.send_reverb.is_some() { target.send_reverb = src.send_reverb; }
    if src.send_distortion.is_some() { target.send_distortion = src.send_distortion; }
    if src.master_gain.is_some() { target.master_gain = src.master_gain; }
}

const LOCK_COLOR: egui::Color32 = egui::Color32::BLACK;
const MARKER_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 220, 100);

fn dim(c: egui::Color32, factor: f32) -> egui::Color32 {
    let f = factor.clamp(0.0, 1.0);
    egui::Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

fn cell_color(on: bool, here: bool, selected: bool) -> egui::Color32 {
    match (on, here, selected) {
        (true, true, _) => egui::Color32::from_rgb(255, 210, 90),
        (true, false, _) => egui::Color32::from_rgb(210, 130, 60),
        (false, true, _) => egui::Color32::from_rgb(90, 90, 110),
        (false, false, true) => egui::Color32::from_rgb(55, 55, 70),
        (false, false, false) => egui::Color32::from_rgb(38, 38, 44),
    }
}

const FOCUS_CELL: egui::Vec2 = egui::vec2(56.0, 60.0);

impl App {
    fn draw_focus_cluster(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[(std::ops::Range<usize>, [&'static str; 4])],
    ) {
        let cur_step = self.current_step();
        let playing = self.playing();
        let v = self.selected_voice;
        ui.vertical(|ui| {
            for (range, labels) in rows {
                ui.horizontal(|ui| {
                    for (i, s) in range.clone().enumerate() {
                        let on = self.pattern[v][s];
                        let here = playing && s == cur_step;
                        let color = cell_color(on, here, true);
                        let (rect, resp) =
                            ui.allocate_exact_size(FOCUS_CELL, egui::Sense::click());
                        ui.painter().rect_filled(rect, 4.0, color);
                        if !self.locks[v][s].is_empty() {
                            ui.painter().circle_filled(
                                rect.right_top() + egui::vec2(-7.0, 7.0),
                                3.0,
                                LOCK_COLOR,
                            );
                        }
                        let text_color =
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160);
                        ui.painter().text(
                            rect.left_top() + egui::vec2(6.0, 4.0),
                            egui::Align2::LEFT_TOP,
                            labels[i],
                            egui::FontId::monospace(12.0),
                            text_color,
                        );
                        ui.painter().text(
                            rect.right_bottom() - egui::vec2(6.0, 4.0),
                            egui::Align2::RIGHT_BOTTOM,
                            format!("{}", s + 1),
                            egui::FontId::proportional(12.0),
                            text_color,
                        );
                        if resp.clicked() {
                            self.toggle_step(v, s);
                        }
                    }
                });
            }
        });
    }

    fn draw_overview_filling(&mut self, ui: &mut egui::Ui, height: f32) {
        // Render the 10 x 16 overview to fill the given height, expanding cells
        // horizontally to fill available_width.
        let cur_step = self.current_step();
        let playing = self.playing();
        let avail_w = ui.available_width();
        let bar_gap = 6.0;
        let cell_w = ((avail_w - bar_gap) / STEPS as f32).floor().max(4.0);
        let row_spacing = 2.0;
        let cell_h = ((height - (VOICES as f32 - 1.0) * row_spacing) / VOICES as f32)
            .floor()
            .max(6.0);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(2.0, row_spacing);
            for v in 0..VOICES {
                ui.horizontal(|ui| {
                    let is_sel = v == self.selected_voice;
                    for s in 0..STEPS {
                        let on = self.pattern[v][s];
                        let here = playing && s == cur_step;
                        let mut color = cell_color(on, here, is_sel);
                        if self.muted[v] {
                            color = dim(color, 0.35);
                        }
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(cell_w, cell_h),
                            egui::Sense::click(),
                        );
                        ui.painter().rect_filled(rect, 2.0, color);
                        if !self.locks[v][s].is_empty() {
                            let r = (cell_h * 0.18).max(1.5);
                            ui.painter().circle_filled(
                                rect.right_top() + egui::vec2(-r - 1.0, r + 1.0),
                                r,
                                LOCK_COLOR,
                            );
                        }
                        if resp.clicked() {
                            self.selected_voice = v;
                            self.toggle_step(v, s);
                        }
                        if s == 7 {
                            ui.add_space(bar_gap);
                        }
                    }
                });
            }
        });
    }

    fn draw_record_button(&mut self, ui: &mut egui::Ui) {
        let recording = self.recorder.is_recording();
        if recording {
            // red dot + "stop" label.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 4.0, egui::Color32::from_rgb(220, 60, 60));
        }
        let label = if recording { "Stop Rec" } else { "Record" };
        if ui.button(label).clicked() {
            if recording {
                self.recorder.stop();
            } else {
                let path = record::default_filename(std::path::Path::new("."));
                if let Err(e) = self.recorder.start(&path) {
                    eprintln!("recorder start failed: {e}");
                } else {
                    eprintln!("recording to {}", path.display());
                }
            }
        }
    }

    fn draw_global_fx(&mut self, ui: &mut egui::Ui) {
        ui.columns(3, |cols| {
            // Delay
            cols[0].label(egui::RichText::new("Delay").strong());
            let mut d = self.delay_params;
            let mut delay_changed = false;
            slider(&mut cols[0], "time ms", d.time_ms, 5.0..=2000.0, true, d.time_ms, |v| {
                d.time_ms = v;
                delay_changed = true;
            });
            slider(&mut cols[0], "feedback", d.feedback, 0.0..=0.95, false, d.feedback, |v| {
                d.feedback = v;
                delay_changed = true;
            });
            slider(&mut cols[0], "tone hz", d.lpf_hz, 200.0..=16000.0, true, d.lpf_hz, |v| {
                d.lpf_hz = v;
                delay_changed = true;
            });
            if delay_changed {
                self.delay_params = d;
                self.send(Command::SetDelayParams(d));
            }

            // Distortion
            cols[1].label(egui::RichText::new("Distortion").strong());
            let mut x = self.bus_distortion_params;
            let mut dist_changed = false;
            cols[1].label("stage 1 (soft)");
            slider(&mut cols[1], "drive", x.stage1_drive, 0.0..=1.0, false, x.stage1_drive, |v| {
                x.stage1_drive = v;
                dist_changed = true;
            });
            slider(&mut cols[1], "tone", x.stage1_tone, 0.0..=1.0, false, x.stage1_tone, |v| {
                x.stage1_tone = v;
                dist_changed = true;
            });
            cols[1].add_space(6.0);
            cols[1].label("stage 2 (hard)");
            slider(&mut cols[1], "drive", x.stage2_drive, 0.0..=1.0, false, x.stage2_drive, |v| {
                x.stage2_drive = v;
                dist_changed = true;
            });
            slider(&mut cols[1], "tone", x.stage2_tone, 0.0..=1.0, false, x.stage2_tone, |v| {
                x.stage2_tone = v;
                dist_changed = true;
            });
            cols[1].add_space(6.0);
            cols[1].label("nasty");
            slider(&mut cols[1], "bias", x.bias, 0.0..=1.0, false, x.bias, |v| {
                x.bias = v;
                dist_changed = true;
            });
            slider(&mut cols[1], "feedback", x.feedback, 0.0..=0.4, false, x.feedback, |v| {
                x.feedback = v;
                dist_changed = true;
            });
            slider(&mut cols[1], "gate", x.gate, 0.0..=1.0, false, x.gate, |v| {
                x.gate = v;
                dist_changed = true;
            });
            cols[1].add_space(6.0);
            slider(&mut cols[1], "output", x.output, 0.0..=2.0, false, x.output, |v| {
                x.output = v;
                dist_changed = true;
            });
            if dist_changed {
                self.bus_distortion_params = x;
                self.send(Command::SetBusDistortionParams(x));
            }

            // Reverb
            cols[2].label(egui::RichText::new("Reverb").strong());
            let mut r = self.reverb_params;
            let mut rev_changed = false;
            slider(&mut cols[2], "size", r.size, 0.0..=1.0, false, r.size, |v| {
                r.size = v;
                rev_changed = true;
            });
            slider(&mut cols[2], "damp", r.damp, 0.0..=1.0, false, r.damp, |v| {
                r.damp = v;
                rev_changed = true;
            });
            slider(&mut cols[2], "output", r.output, 0.0..=2.0, false, r.output, |v| {
                r.output = v;
                rev_changed = true;
            });
            if rev_changed {
                self.reverb_params = r;
                self.send(Command::SetReverbParams(r));
            }
        });
    }

    fn draw_param_editor(&mut self, ui: &mut egui::Ui) {
        let target = self.editor_target();
        let display_voice = match target {
            EditTarget::Overdub(v) => v,
            _ => self.selected_voice,
        };
        let p = self.editor_params();

        ui.horizontal(|ui| {
            let suffix = match target {
                EditTarget::VoiceDefault => String::new(),
                EditTarget::StepLock(s) => format!("  (step {} lock)", s + 1),
                EditTarget::Overdub(_) => "  (overdub)".to_string(),
            };
            ui.heading(format!(
                "Voice {} — {}{}",
                display_voice + 1,
                VOICE_NAMES[display_voice],
                suffix
            ));
        });
        ui.separator();
        ui.add_space(4.0);

        let mp = self.playing_marker_params(display_voice);

        let mut edit: Option<FieldEdit> = None;

        ui.columns(3, |cols| {
            // ===== Col 0: Osc 1 + Osc 2 + FM =====
            cols[0].label(egui::RichText::new("Osc 1").strong());
            cols[0].horizontal(|ui| {
                ui.label("wave");
                let mut w = p.osc1_wave;
                for (ww, lbl) in [
                    (Wave::Sine, "sin"),
                    (Wave::Triangle, "tri"),
                    (Wave::Saw, "saw"),
                    (Wave::Square, "sq"),
                ] {
                    if ui.selectable_value(&mut w, ww, lbl).clicked() {
                        edit = Some(FieldEdit::Osc1Wave(ww));
                    }
                }
            });
            slider(&mut cols[0], "level", p.osc1_level, 0.0..=1.0, false, mp.osc1_level, |v| {
                edit = Some(FieldEdit::Osc1Level(v));
            });
            slider(&mut cols[0], "start hz", p.osc1_start_hz, 20.0..=4000.0, true, mp.osc1_start_hz, |v| {
                edit = Some(FieldEdit::Osc1StartHz(v));
            });
            slider(&mut cols[0], "end hz", p.osc1_end_hz, 20.0..=4000.0, true, mp.osc1_end_hz, |v| {
                edit = Some(FieldEdit::Osc1EndHz(v));
            });
            slider(&mut cols[0], "pitch decay ms", p.osc1_pitch_decay_ms, 0.5..=1000.0, true, mp.osc1_pitch_decay_ms, |v| {
                edit = Some(FieldEdit::Osc1PitchDecayMs(v));
            });
            slider(&mut cols[0], "attack ms", p.osc1_amp_attack_ms, 0.1..=200.0, true, mp.osc1_amp_attack_ms, |v| {
                edit = Some(FieldEdit::Osc1AmpAttackMs(v));
            });
            slider(&mut cols[0], "decay ms", p.osc1_amp_decay_ms, 1.0..=2000.0, true, mp.osc1_amp_decay_ms, |v| {
                edit = Some(FieldEdit::Osc1AmpDecayMs(v));
            });
            cols[0].add_space(12.0);
            cols[0].label(egui::RichText::new("Osc 2").strong());
            cols[0].horizontal(|ui| {
                ui.label("wave");
                let mut w = p.osc2_wave;
                for (ww, lbl) in [
                    (Wave::Sine, "sin"),
                    (Wave::Triangle, "tri"),
                    (Wave::Saw, "saw"),
                    (Wave::Square, "sq"),
                ] {
                    if ui.selectable_value(&mut w, ww, lbl).clicked() {
                        edit = Some(FieldEdit::Osc2Wave(ww));
                    }
                }
            });
            slider(&mut cols[0], "level", p.osc2_level, 0.0..=1.0, false, mp.osc2_level, |v| {
                edit = Some(FieldEdit::Osc2Level(v));
            });
            slider(&mut cols[0], "ratio", p.osc2_ratio, 0.25..=8.0, true, mp.osc2_ratio, |v| {
                edit = Some(FieldEdit::Osc2Ratio(v));
            });
            slider(&mut cols[0], "fm", p.fm_amount, 0.0..=1.0, false, mp.fm_amount, |v| {
                edit = Some(FieldEdit::FmAmount(v));
            });

            // ===== Col 1: Noise =====
            cols[1].label(egui::RichText::new("Noise").strong());
            cols[1].horizontal(|ui| {
                ui.label("color");
                let mut c = p.noise_color;
                for (cc, lbl) in [
                    (NoiseColor::White, "white"),
                    (NoiseColor::Pink, "pink"),
                    (NoiseColor::Grain, "grain"),
                    (NoiseColor::Digital, "digital"),
                ] {
                    if ui.selectable_value(&mut c, cc, lbl).clicked() {
                        edit = Some(FieldEdit::NoiseColor(cc));
                    }
                }
            });
            slider(&mut cols[1], "level", p.noise_level, 0.0..=1.0, false, mp.noise_level, |v| {
                edit = Some(FieldEdit::NoiseLevel(v));
            });
            cols[1].horizontal(|ui| {
                ui.label("filter");
                let mut mode = p.noise_filter_mode;
                for (mm, lbl) in [
                    (FilterMode::Off, "off"),
                    (FilterMode::LowPass, "lp"),
                    (FilterMode::HighPass, "hp"),
                ] {
                    if ui.selectable_value(&mut mode, mm, lbl).clicked() {
                        edit = Some(FieldEdit::NoiseFilterMode(mm));
                    }
                }
            });
            slider(&mut cols[1], "filter hz", p.noise_filter_hz, 20.0..=18000.0, true, mp.noise_filter_hz, |v| {
                edit = Some(FieldEdit::NoiseFilterHz(v));
            });
            slider(&mut cols[1], "attack ms", p.noise_amp_attack_ms, 0.1..=200.0, true, mp.noise_amp_attack_ms, |v| {
                edit = Some(FieldEdit::NoiseAmpAttackMs(v));
            });
            slider(&mut cols[1], "decay ms", p.noise_amp_decay_ms, 1.0..=2000.0, true, mp.noise_amp_decay_ms, |v| {
                edit = Some(FieldEdit::NoiseAmpDecayMs(v));
            });

            // ===== Col 2: FX → Filter → Master → Sends =====
            cols[2].label(egui::RichText::new("FX").strong());
            slider(&mut cols[2], "drive", p.drive, 0.0..=1.0, false, mp.drive, |v| {
                edit = Some(FieldEdit::Drive(v));
            });
            slider(&mut cols[2], "fold", p.fold, 0.0..=1.0, false, mp.fold, |v| {
                edit = Some(FieldEdit::Fold(v));
            });
            slider(&mut cols[2], "crush", p.crush, 0.0..=1.0, false, mp.crush, |v| {
                edit = Some(FieldEdit::Crush(v));
            });
            slider(&mut cols[2], "srr", p.srr, 0.0..=1.0, false, mp.srr, |v| {
                edit = Some(FieldEdit::Srr(v));
            });
            cols[2].add_space(12.0);
            cols[2].label(egui::RichText::new("Filter").strong());
            cols[2].horizontal(|ui| {
                ui.label("mode");
                let mut mode = p.post_filter_mode;
                for (mm, lbl) in [
                    (FilterMode::Off, "off"),
                    (FilterMode::LowPass, "lp"),
                    (FilterMode::HighPass, "hp"),
                    (FilterMode::BandPass, "bp"),
                ] {
                    if ui.selectable_value(&mut mode, mm, lbl).clicked() {
                        edit = Some(FieldEdit::PostFilterMode(mm));
                    }
                }
            });
            slider(&mut cols[2], "cutoff hz", p.post_filter_hz, 20.0..=18000.0, true, mp.post_filter_hz, |v| {
                edit = Some(FieldEdit::PostFilterHz(v));
            });
            slider(&mut cols[2], "resonance", p.post_filter_q, 0.5..=15.0, true, mp.post_filter_q, |v| {
                edit = Some(FieldEdit::PostFilterQ(v));
            });
            cols[2].add_space(12.0);
            cols[2].label(egui::RichText::new("Master").strong());
            slider(&mut cols[2], "gain", p.master_gain, 0.0..=2.0, false, mp.master_gain, |v| {
                edit = Some(FieldEdit::MasterGain(v));
            });
            cols[2].add_space(12.0);
            cols[2].label(egui::RichText::new("Sends").strong());
            slider(&mut cols[2], "delay", p.send_delay, 0.0..=1.0, false, mp.send_delay, |v| {
                edit = Some(FieldEdit::SendDelay(v));
            });
            slider(&mut cols[2], "reverb", p.send_reverb, 0.0..=1.0, false, mp.send_reverb, |v| {
                edit = Some(FieldEdit::SendReverb(v));
            });
            slider(&mut cols[2], "dist", p.send_distortion, 0.0..=1.0, false, mp.send_distortion, |v| {
                edit = Some(FieldEdit::SendDistortion(v));
            });
        });

        if let Some(e) = edit {
            self.apply_field_edit(e);
        }
    }

    fn draw_bottom(&mut self, ui: &mut egui::Ui) {
        let item_y = ui.spacing().item_spacing.y;
        let cluster_w = 4.0 * FOCUS_CELL.x + 3.0 * ui.spacing().item_spacing.x;
        let cluster_h = 2.0 * FOCUS_CELL.y + item_y;

        ui.horizontal(|ui| {
            ui.allocate_ui(egui::vec2(cluster_w, cluster_h), |ui| {
                self.draw_focus_cluster(
                    ui,
                    &[
                        (0..4, ["Q", "W", "E", "R"]),
                        (4..8, ["A", "S", "D", "F"]),
                    ],
                );
            });

            let middle_w = (ui.available_width() - cluster_w - ui.spacing().item_spacing.x)
                .max(0.0);
            ui.allocate_ui(egui::vec2(middle_w, cluster_h), |ui| {
                self.draw_overview_filling(ui, cluster_h);
            });

            ui.allocate_ui(egui::vec2(cluster_w, cluster_h), |ui| {
                self.draw_focus_cluster(
                    ui,
                    &[
                        (8..12, ["U", "I", "O", "P"]),
                        (12..16, ["J", "K", "L", ";"]),
                    ],
                );
            });
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_keys(ctx);
        self.commit_overdubs();
        ctx.request_repaint();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("transport_bar").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let playing = self.playing();
                if ui.button(if playing { "Stop" } else { "Play" }).clicked() {
                    self.send(if playing { Command::Stop } else { Command::Play });
                }
                let mut bpm = self.bpm;
                if ui
                    .add(egui::Slider::new(&mut bpm, 40..=240).text("BPM"))
                    .changed()
                {
                    self.bpm = bpm;
                    self.send(Command::SetTempo(bpm * 1000));
                }
                ui.separator();
                ui.label(format!("Voice {}", self.selected_voice + 1));
                ui.separator();
                self.draw_record_button(ui);
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("bottom_panel").show_inside(ui, |ui| {
            ui.add_space(12.0);
            self.draw_bottom(ui);
            ui.add_space(12.0);
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    egui::CollapsingHeader::new(egui::RichText::new("Global FX").strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            self.draw_global_fx(ui);
                        });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    self.draw_param_editor(ui);
                });
        });
    }
}

type Built = (Handle, cpal::Stream, [DrumVoiceParams; VOICES], Arc<Recorder>);

fn build_engine_and_stream() -> Result<Built, Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let config = device.default_output_config()?;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate;
    let channels = stream_config.channels as usize;

    let presets: [DrumVoiceParams; VOICES] = [
        DrumVoiceParams::kick(),
        DrumVoiceParams::snare(),
        DrumVoiceParams::closed_hat(),
        DrumVoiceParams::open_hat(),
        DrumVoiceParams::tom_lo(),
        DrumVoiceParams::tom_hi(),
        DrumVoiceParams::clap(),
        DrumVoiceParams::rim(),
        DrumVoiceParams::perc_lo(),
        DrumVoiceParams::bass(),
    ];
    let voices: Vec<DrumVoice> = presets
        .iter()
        .map(|p| DrumVoice::new(*p, sample_rate))
        .collect();
    let (mut engine, handle) = Engine::new(sample_rate, voices);

    let recorder = Arc::new(Recorder::new(sample_rate));
    let recorder_audio = recorder.clone();

    let err_fn = |e| eprintln!("audio stream error: {e}");
    let mut mono_buf: Vec<f32> = Vec::new();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |out: &mut [f32], _| {
                let frames = out.len() / channels;
                if mono_buf.len() < frames {
                    mono_buf.resize(frames, 0.0);
                }
                let mono = &mut mono_buf[..frames];
                engine.process(mono);
                // Tap the mono signal for recording before duplicating to channels.
                recorder_audio.push_samples(mono);
                for (i, frame) in out.chunks_mut(channels).enumerate() {
                    for s in frame.iter_mut() {
                        *s = mono[i];
                    }
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(format!("unsupported sample format: {other:?}").into()),
    };

    stream.play()?;
    Ok((handle, stream, presets, recorder))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (handle, stream, presets, recorder) = build_engine_and_stream()?;
    eframe::run_native(
        "drum sequencer",
        eframe::NativeOptions::default(),
        Box::new(move |_| Ok(Box::new(App::new(handle, stream, presets, recorder)))),
    )?;
    Ok(())
}
