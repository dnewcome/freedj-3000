//! Top-level UI application.  Owns the wgpu device, egui context, and winit window.
//!
//! Frame loop:
//!   1. winit event → egui input
//!   2. wgpu render pass: waveform quad (custom WGSL shader)
//!   3. wgpu render pass: jog wheel (custom WGSL shader)
//!   4. egui pass: pads, text overlays, track browser

use std::sync::Arc;
use opendeck_types::EngineState;

pub struct UiApp {
    /// Shared read-only view of the audio engine state (updated at ~100Hz).
    engine_state: Arc<EngineState>,
    // TODO: wgpu Device, Queue, Surface, egui renderer
}

impl UiApp {
    pub fn new(engine_state: Arc<EngineState>) -> Self {
        Self { engine_state }
    }

    /// Called once per frame by the winit event loop.
    pub fn render(&mut self) {
        let snap = self.engine_state.snapshot();
        // TODO:
        // 1. waveform render pass — read snap.position, scroll waveform texture
        // 2. jog wheel render pass — read snap.beat_phase for platter indicator
        // 3. egui pass — pads, BPM display, browser
        let _ = snap;
    }
}
