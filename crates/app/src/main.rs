use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use engine::{BoxedVoice, Command, Engine, Handle, STEPS, VOICES};
use std::sync::atomic::Ordering;
use synth::DrumVoice;

struct App {
    handle: Handle,
    pattern: [[bool; STEPS]; VOICES], // UI mirror; engine has authoritative copy.
    selected_voice: usize,
    bpm: u32,
    _stream: cpal::Stream, // hold to keep audio alive
}

impl App {
    fn new(handle: Handle, stream: cpal::Stream) -> Self {
        Self {
            handle,
            pattern: [[false; STEPS]; VOICES],
            selected_voice: 0,
            bpm: 120,
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
    }

    fn playing(&self) -> bool {
        self.handle.shared.playing.load(Ordering::Relaxed)
    }

    fn current_step(&self) -> usize {
        self.handle.shared.current_step()
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::Key::*;
        let step_keys = [Q, W, E, R, U, I, O, P, A, S, D, F, J, K, L, Semicolon];
        let voice_keys = [Z, X, C, V, B, N, M, Comma, Period, Slash];
        let num_keys = [
            Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, Num0,
        ];

        let (toggled_steps, selected, plain_space, shift_space) = ctx.input(|i| {
            let mut steps: Vec<usize> = Vec::new();
            for (idx, k) in step_keys.iter().enumerate() {
                if i.key_pressed(*k) {
                    steps.push(idx);
                }
            }
            let mut sel = None;
            for (idx, k) in voice_keys.iter().enumerate() {
                if i.key_pressed(*k) {
                    sel = Some(idx);
                }
            }
            for (idx, k) in num_keys.iter().enumerate() {
                if i.key_pressed(*k) {
                    sel = Some(idx);
                }
            }
            let space = i.key_pressed(Space);
            let shift = i.modifiers.shift;
            (steps, sel, space && !shift, space && shift)
        });

        for s in toggled_steps {
            let v = self.selected_voice;
            self.toggle_step(v, s);
        }
        if let Some(v) = selected {
            self.selected_voice = v;
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

fn cell_color(on: bool, here: bool, selected: bool) -> egui::Color32 {
    match (on, here, selected) {
        (true, true, _) => egui::Color32::from_rgb(255, 210, 90),
        (true, false, _) => egui::Color32::from_rgb(210, 130, 60),
        (false, true, _) => egui::Color32::from_rgb(90, 90, 110),
        (false, false, true) => egui::Color32::from_rgb(55, 55, 70),
        (false, false, false) => egui::Color32::from_rgb(38, 38, 44),
    }
}

impl App {
    fn draw_overview(&mut self, ui: &mut egui::Ui) {
        let cur_step = self.current_step();
        let playing = self.playing();
        let cell = egui::vec2(14.0, 12.0);
        let bar_gap = 6.0;

        for v in 0..VOICES {
            ui.horizontal(|ui| {
                let is_sel = v == self.selected_voice;
                let marker = if is_sel { ">" } else { " " };
                ui.monospace(format!("{}{:>2}", marker, v + 1));
                for s in 0..STEPS {
                    let on = self.pattern[v][s];
                    let here = playing && s == cur_step;
                    let color = cell_color(on, here, is_sel);
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    ui.painter().rect_filled(rect, 2.0, color);
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
    }

    fn draw_focused(&mut self, ui: &mut egui::Ui) {
        // 16 steps laid out in 2 rows of 8, split 4+4 to mirror the keyboard.
        // Left half flush-left, right half flush-right, gap fills remaining space.
        let cur_step = self.current_step();
        let playing = self.playing();
        let v = self.selected_voice;
        let cell = egui::vec2(72.0, 56.0);
        let item_spacing_x = ui.spacing().item_spacing.x;
        let key_rows = [
            (0..8, ["Q", "W", "E", "R", "U", "I", "O", "P"]),
            (8..16, ["A", "S", "D", "F", "J", "K", "L", ";"]),
        ];

        for (range, labels) in key_rows {
            ui.horizontal(|ui| {
                let avail = ui.available_width();
                let cells_w = 8.0 * cell.x;
                let inner_spacings = 6.0 * item_spacing_x;
                let center_gap = (avail - cells_w - inner_spacings).max(item_spacing_x);

                for (i, s) in range.enumerate() {
                    let on = self.pattern[v][s];
                    let here = playing && s == cur_step;
                    let color = cell_color(on, here, true);
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    ui.painter().rect_filled(rect, 4.0, color);
                    let text_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160);
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
                    if i == 3 {
                        ui.add_space(center_gap - item_spacing_x);
                    }
                }
            });
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_keys(ctx);
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
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("focused_grid").show_inside(ui, |ui| {
            ui.add_space(12.0);
            self.draw_focused(ui);
            ui.add_space(12.0);
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(8.0);
            self.draw_overview(ui);
        });
    }
}

fn build_engine_and_stream() -> Result<(Handle, cpal::Stream), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let config = device.default_output_config()?;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate;
    let channels = stream_config.channels as usize;

    let voices: Vec<BoxedVoice> = vec![
        Box::new(DrumVoice::kick(sample_rate)),
        Box::new(DrumVoice::snare(sample_rate)),
        Box::new(DrumVoice::closed_hat(sample_rate)),
        Box::new(DrumVoice::open_hat(sample_rate)),
        Box::new(DrumVoice::tom_lo(sample_rate)),
        Box::new(DrumVoice::tom_hi(sample_rate)),
        Box::new(DrumVoice::clap(sample_rate)),
        Box::new(DrumVoice::rim(sample_rate)),
        Box::new(DrumVoice::perc_lo(sample_rate)),
        Box::new(DrumVoice::perc_hi(sample_rate)),
    ];
    assert_eq!(voices.len(), VOICES);
    let (mut engine, handle) = Engine::new(sample_rate, voices);

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
    Ok((handle, stream))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (handle, stream) = build_engine_and_stream()?;
    eframe::run_native(
        "drum sequencer",
        eframe::NativeOptions::default(),
        Box::new(move |_| Ok(Box::new(App::new(handle, stream)))),
    )?;
    Ok(())
}
