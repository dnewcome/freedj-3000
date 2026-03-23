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
mod renderer;

use anyhow::{bail, Context, Result};
use audio::AudioHandle;
use opendeck_analysis::{BeatAnalyzerImpl, WaveformBuilder, WaveformCache};
use opendeck_types::{BeatAnalyzer, BeatGrid};
use renderer::Renderer;
use std::{
    path::PathBuf,
    sync::{
        atomic::Ordering,
        Arc,
    },
    time::Instant,
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes},
};

// ── App state ─────────────────────────────────────────────────────────────────

struct DeckApp {
    // Provided before event loop starts.
    path:       PathBuf,
    waveform:   WaveformCache,
    audio:      AudioHandle,
    beat_grid:  Option<BeatGrid>,

    // Created on first `resumed`.
    window:     Option<Arc<Window>>,
    renderer:   Option<Renderer>,
    egui_ctx:   egui::Context,
    egui_state: Option<egui_winit::State>,
}

impl DeckApp {
    fn new(path: PathBuf, waveform: WaveformCache, audio: AudioHandle, beat_grid: Option<BeatGrid>) -> Self {
        Self {
            path,
            waveform,
            audio,
            beat_grid,
            window:     None,
            renderer:   None,
            egui_ctx:   egui::Context::default(),
            egui_state: None,
        }
    }

    fn render_frame(&mut self) {
        let (renderer, egui_state, window) = match (
            self.renderer.as_mut(),
            self.egui_state.as_mut(),
            self.window.as_ref(),
        ) {
            (Some(r), Some(s), Some(w)) => (r, s, w),
            _ => return,
        };

        let pos       = self.audio.position.load(Ordering::Relaxed);
        let playing   = self.audio.playing.load(Ordering::Relaxed);
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
                            ui.label(
                                egui::RichText::new(format!("{:.1} BPM", grid.bpm))
                                    .color(color)
                                    .monospace(),
                            );
                        }
                        ui.separator();
                        ui.label(
                            egui::RichText::new("Space=play/pause  ←/→=seek  Q=quit")
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
            .with_title("OpenDeck")
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
                // Request the next frame unconditionally (we are always animating).
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            _ => {}
        }
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

    // ── 3. Connect MIDI controller (optional — app runs fine without it) ─────────
    let _midi = midi::MidiHandle::connect(
        Arc::clone(&audio.playing),
        Arc::clone(&audio.position),
        audio.sample_rate,
        audio.channels,
        audio.samples.len(),
    );

    // ── 4. Run the UI event loop ──────────────────────────────────────────────
    let event_loop = EventLoop::new().context("failed to create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = DeckApp::new(path, waveform, audio, beat_grid);
    event_loop.run_app(&mut app).context("event loop error")?;

    Ok(())
}
