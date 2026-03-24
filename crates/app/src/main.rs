//! OpenDeck MVP — play an MP3 with waveform visualization.
//!
//! Usage:  opendeck <path/to/file.mp3>
//!
//! Controls:
//!   Space    — play / pause
//!   ← / →   — seek ±10 seconds
//!   Esc / Q  — quit

mod audio;
mod midi;
mod prodj;
mod renderer;

use anyhow::{bail, Context, Result};
use audio::AudioHandle;
use opendeck_analysis::{BeatAnalyzerImpl, WaveformBuilder, WaveformCache};
use opendeck_types::{BeatAnalyzer, BeatGrid};
use renderer::Renderer;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes},
};

// ── App state ─────────────────────────────────────────────────────────────────

/// Target frame interval — 60 fps.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

struct DeckApp {
    // Provided before event loop starts.
    path:         PathBuf,
    waveform:     WaveformCache,
    audio:        AudioHandle,
    beat_grid:    Option<BeatGrid>,

    // Second beat grid — tempo controlled by Deck B on the MIDI controller.
    fader_speed:  Arc<AtomicU32>,  // f32 bits; pitch-fader speed (no jog nudge)
    beat2_bpm:    Arc<AtomicU32>,  // f32 bits; BPM of the second grid
    beat2_anchor: Arc<AtomicU64>, // written by MIDI Cue B to signal a phase reset
    beat2_start:  Instant,        // wall-clock time of the last phase reset
    prev_beat2_anchor: u64,       // detect changes in beat2_anchor
    prev_beat2_bpm:    f32,       // detect BPM changes for logging

    // Created on first `resumed`.
    window:      Option<Arc<Window>>,
    renderer:    Option<Renderer>,
    egui_ctx:    egui::Context,
    egui_state:  Option<egui_winit::State>,

    /// Time of the last rendered frame, used to cap to FRAME_INTERVAL.
    last_render: Instant,
}

impl DeckApp {
    fn new(
        path:         PathBuf,
        waveform:     WaveformCache,
        audio:        AudioHandle,
        beat_grid:    Option<BeatGrid>,
        fader_speed:  Arc<AtomicU32>,
        beat2_bpm:    Arc<AtomicU32>,
        beat2_anchor: Arc<AtomicU64>,
    ) -> Self {
        Self {
            path,
            waveform,
            audio,
            beat_grid,
            fader_speed,
            beat2_bpm,
            beat2_anchor,
            beat2_start:       Instant::now(),
            prev_beat2_anchor: 0,
            prev_beat2_bpm:    0.0,
            window:      None,
            renderer:    None,
            egui_ctx:    egui::Context::default(),
            egui_state:  None,
            last_render: Instant::now(),
        }
    }

    fn render_frame(&mut self) {
        self.last_render = Instant::now();

        let (renderer, egui_state, window) = match (
            self.renderer.as_mut(),
            self.egui_state.as_mut(),
            self.window.as_ref(),
        ) {
            (Some(r), Some(s), Some(w)) => (r, s, w),
            _ => return,
        };

        let pos          = self.audio.position.load(Ordering::Relaxed);
        let playing      = self.audio.playing.load(Ordering::Relaxed);
        let speed        = self.audio.speed_load();
        let fader_speed  = f32::from_bits(self.fader_speed.load(Ordering::Relaxed));
        let beat2_bpm    = f32::from_bits(self.beat2_bpm.load(Ordering::Relaxed));
        let beat2_anchor = self.beat2_anchor.load(Ordering::Relaxed);

        // Log when beat2_bpm changes (confirms ProDJ data is reaching the renderer).
        if (beat2_bpm - self.prev_beat2_bpm).abs() > 0.01 {
            log::info!("render: beat2_bpm updated {:.2} → {:.2}", self.prev_beat2_bpm, beat2_bpm);
            self.prev_beat2_bpm = beat2_bpm;
        }

        // Reset the phase timer whenever the MIDI Cue B button is pressed.
        if beat2_anchor != self.prev_beat2_anchor {
            self.beat2_start       = Instant::now();
            self.prev_beat2_anchor = beat2_anchor;
        }
        let beat2_phase_beats = if beat2_bpm > 0.0 {
            let elapsed = self.beat2_start.elapsed().as_secs_f32();
            (elapsed * beat2_bpm / 60.0).fract()
        } else {
            0.0
        };
        let sr        = self.audio.sample_rate;
        let ch        = self.audio.channels as u64;
        let total_s   = self.audio.samples.len() as f64 / sr as f64 / ch as f64;
        let elapsed_s = pos as f64 / sr as f64 / ch as f64;

        // Build egui overlay.
        let raw = egui_state.take_egui_input(window.as_ref());
        let mut output = self.egui_ctx.run(raw, |ctx| {
            // Transparent panel at the top.
            egui::TopBottomPanel::top("info")
                .frame(egui::Frame::default().fill(egui::Color32::from_black_alpha(160)))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(
                                self.path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown"),
                            )
                            .color(egui::Color32::WHITE)
                            .strong(),
                        );
                        ui.separator();
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {:02.0}:{:05.2} / {:02.0}:{:05.2}  {}Hz",
                                if playing { "▶" } else { "⏸" },
                                elapsed_s / 60.0,
                                elapsed_s % 60.0,
                                total_s / 60.0,
                                total_s % 60.0,
                                sr,
                            ))
                            .color(egui::Color32::LIGHT_GRAY)
                            .monospace(),
                        );
                        if let Some(grid) = &self.beat_grid {
                            ui.separator();
                            let conf = grid.confidence;
                            let color = if conf >= 0.7 {
                                egui::Color32::from_rgb(80, 220, 80)
                            } else {
                                egui::Color32::from_rgb(220, 180, 60)
                            };
                            let displayed_bpm = grid.bpm * speed as f64;
                            ui.label(
                                egui::RichText::new(format!("{:.1} BPM", displayed_bpm))
                                    .color(color)
                                    .monospace(),
                            );
                        }
                        ui.separator();
                        ui.label(
                            egui::RichText::new(format!("B2: {beat2_bpm:.1} BPM"))
                                .color(egui::Color32::from_rgb(0, 220, 220))
                                .monospace(),
                        );
                        ui.separator();
                        let speed_color = if (speed - 1.0).abs() < 0.01 {
                            egui::Color32::DARK_GRAY
                        } else {
                            egui::Color32::from_rgb(240, 160, 60)
                        };
                        ui.label(
                            egui::RichText::new(format!("{:.2}×", speed))
                                .color(speed_color)
                                .monospace(),
                        );
                        ui.separator();
                        ui.label(
                            egui::RichText::new("Space=play/pause  ←/→=seek  +/-=speed  Q=quit")
                                .color(egui::Color32::DARK_GRAY)
                                .small(),
                        );
                    });
                });
        });

        let platform_output = std::mem::take(&mut output.platform_output);
        egui_state.handle_platform_output(window.as_ref(), platform_output);

        renderer.render(
            pos,
            sr,
            self.audio.channels,
            self.beat_grid.as_ref(),
            fader_speed,
            beat2_bpm,
            beat2_phase_beats,
            &self.egui_ctx,
            output,
        );
    }
}

impl ApplicationHandler for DeckApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialised (e.g. Android resume)
        }

        let attrs = WindowAttributes::default()
            .with_title("freedj-3000")
            .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 480u32));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let renderer =
            pollster::block_on(Renderer::new(Arc::clone(&window), &self.waveform))
                .expect("failed to create renderer");

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            self.egui_ctx.viewport_id(),
            &*window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        self.window     = Some(Arc::clone(&window));
        self.renderer   = Some(renderer);
        self.egui_state = Some(egui_state);

        // Kick off the first redraw.
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id:  winit::window::WindowId,
        event:       WindowEvent,
    ) {
        // Forward all events to egui first.
        if let (Some(state), Some(window)) = (&mut self.egui_state, &self.window) {
            let resp = state.on_window_event(window.as_ref(), &event);
            if resp.repaint {
                window.request_redraw();
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput {
                event: KeyEvent { physical_key, state: ElementState::Pressed, .. },
                ..
            } => match physical_key {
                PhysicalKey::Code(KeyCode::Space) => {
                    let was = self.audio.playing.load(Ordering::Relaxed);
                    self.audio.playing.store(!was, Ordering::Relaxed);
                    log::info!("{}", if was { "paused" } else { "playing" });
                }
                PhysicalKey::Code(KeyCode::ArrowRight) => {
                    let delta = self.audio.sample_rate as u64
                        * self.audio.channels as u64
                        * 10;
                    let pos = self.audio.position.load(Ordering::Relaxed);
                    self.audio.position.store(
                        pos.saturating_add(delta).min(self.audio.samples.len() as u64),
                        Ordering::Relaxed,
                    );
                }
                PhysicalKey::Code(KeyCode::ArrowLeft) => {
                    let delta = self.audio.sample_rate as u64
                        * self.audio.channels as u64
                        * 10;
                    let pos = self.audio.position.load(Ordering::Relaxed);
                    self.audio.position.store(
                        pos.saturating_sub(delta),
                        Ordering::Relaxed,
                    );
                }
                PhysicalKey::Code(KeyCode::Equal) | PhysicalKey::Code(KeyCode::NumpadAdd) => {
                    let s = (self.audio.speed_load() + 0.05).min(2.0);
                    self.audio.speed_store(s);
                    log::info!("speed → {s:.2}×");
                }
                PhysicalKey::Code(KeyCode::Minus) | PhysicalKey::Code(KeyCode::NumpadSubtract) => {
                    let s = (self.audio.speed_load() - 0.05).max(0.25);
                    self.audio.speed_store(s);
                    log::info!("speed → {s:.2}×");
                }
                PhysicalKey::Code(KeyCode::Digit0) | PhysicalKey::Code(KeyCode::Numpad0) => {
                    self.audio.speed_store(1.0);
                    log::info!("speed → 1.00× (reset)");
                }
                PhysicalKey::Code(KeyCode::Escape) | PhysicalKey::Code(KeyCode::KeyQ) => {
                    event_loop.exit();
                }
                _ => {}
            },

            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Request one redraw per frame interval.  Fifo vsync makes wgpu block
        // inside present() until the display is ready, so the thread sleeps in
        // the driver rather than spinning.  WaitUntil is a belt-and-suspenders
        // guard for compositors that deliver frame callbacks faster than vsync.
        let next = self.last_render + FRAME_INTERVAL;
        if Instant::now() >= next {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu=warn,naga=warn"),
    )
    .init();

    let path: PathBuf = std::env::args()
        .nth(1)
        .context("usage: opendeck <path/to/file.mp3>")?
        .into();

    if !path.exists() {
        bail!("file not found: {}", path.display());
    }

    // ── 1. Decode audio ───────────────────────────────────────────────────────
    let audio = AudioHandle::open(&path)?;

    // ── 2. Build waveform + detect beat grid (synchronous, before window opens) ─
    log::info!("computing waveform ({} samples)...", audio.samples.len());
    let t0 = Instant::now();
    let mut waveform_builder = WaveformBuilder::new(audio.sample_rate);
    let mut beat_analyzer    = BeatAnalyzerImpl::new(audio.sample_rate);
    waveform_builder.push(&audio.samples);
    beat_analyzer.push(&audio.samples, audio.sample_rate);
    let waveform  = waveform_builder.finish();
    let beat_grid = beat_analyzer.beat_grid().map(|g| (*g).clone());
    match &beat_grid {
        Some(g) => log::info!(
            "waveform done: {} columns, {:.1} BPM (confidence {:.2}) in {:.1}s",
            waveform.len(), g.bpm, g.confidence, t0.elapsed().as_secs_f32()
        ),
        None => log::info!(
            "waveform done: {} columns, BPM detection failed in {:.1}s",
            waveform.len(), t0.elapsed().as_secs_f32()
        ),
    }

    // ── 3. Create second beat grid state ─────────────────────────────────────
    let base_bpm     = beat_grid.as_ref().map(|g| g.bpm as f32).unwrap_or(120.0);
    let fader_speed  = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let beat2_bpm    = Arc::new(AtomicU32::new(base_bpm.to_bits()));
    let beat2_anchor = Arc::new(AtomicU64::new(0));

    // ── 4. Start ProDJ Link listener (optional — app runs fine without it) ────────
    let _prodj = prodj::ProDjHandle::listen(
        Arc::clone(&beat2_bpm),
        Arc::clone(&beat2_anchor),
    );

    // ── 5. Connect MIDI controller (optional — app runs fine without it) ──────────
    let _midi = midi::MidiHandle::connect(
        Arc::clone(&audio.playing),
        Arc::clone(&audio.position),
        Arc::clone(&audio.speed),
        Arc::clone(&fader_speed),
        audio.sample_rate,
        audio.channels,
        audio.samples.len(),
        Arc::clone(&beat2_bpm),
        Arc::clone(&beat2_anchor),
        base_bpm,
    );

    // ── 6. Run the UI event loop ──────────────────────────────────────────────
    let event_loop = EventLoop::new().context("failed to create event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = DeckApp::new(path, waveform, audio, beat_grid, fader_speed, beat2_bpm, beat2_anchor);
    event_loop.run_app(&mut app).context("event loop error")?;

    Ok(())
}
