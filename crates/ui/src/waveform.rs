//! Waveform renderer.
//!
//! The pre-computed RGB waveform cache is uploaded once as a wgpu Texture.
//! Each frame, a push constant or uniform updates the scroll offset (derived
//! from the current playhead position).  The GPU does almost nothing — it
//! just samples a texture with an offset.
//!
//! Overlay draw calls (beat grid, cue markers, loop region) are issued as
//! separate egui Painter calls after the waveform pass.

pub struct WaveformRenderer {
    // TODO: wgpu::Texture, wgpu::BindGroup, wgpu::RenderPipeline
}

impl WaveformRenderer {
    pub fn new(/* device: &wgpu::Device */) -> Self {
        Self {}
    }

    /// Upload or incrementally update the waveform texture from new analysis columns.
    pub fn update_texture(&mut self, /* queue: &wgpu::Queue, columns: &[[u8;3]] */) {
        // TODO: queue.write_texture(...)
    }

    /// Draw the waveform for the current playhead position.
    /// `position_samples`: current playhead in samples.
    /// `sample_rate`: track sample rate for converting samples → columns.
    pub fn draw(
        &self,
        _position_samples: u64,
        _sample_rate: u32,
        /* encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView */
    ) {
        // TODO: render pass with waveform pipeline
    }
}
