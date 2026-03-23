//! Two pipeline stage implementations:
//!
//! - `ResampleStage`:    speed change without pitch change (no key-lock).
//!                       Uses `rubato` (pure Rust, sinc interpolation).
//!
//! - `TimestretechStage`: speed change with constant pitch (key-lock).
//!                        Wraps Rubber Band Library R3 engine.

use opendeck_types::PipelineStage;

// ── Resample stage (no key-lock) ─────────────────────────────────────────────

pub struct ResampleStage {
    speed:   f32,
    // TODO: rubato SincFixedOut resampler instance
}

impl ResampleStage {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        let _ = (sample_rate, channels);
        Self { speed: 1.0 }
    }
}

impl PipelineStage for ResampleStage {
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        // TODO: run rubato resampler at self.speed ratio.
        // For now, pass through at 1×.
        output.extend_from_slice(input);
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        // TODO: update rubato resampler ratio
    }

    fn set_pitch_semitones(&mut self, _semitones: f32) {
        // No pitch shifting in resample-only mode — pitch follows speed.
    }

    fn latency_frames(&self) -> usize {
        // Rubato SincFixedOut latency at quality level 5 ≈ 256 frames.
        256
    }

    fn reset(&mut self) {
        // TODO: flush rubato internal state
    }
}

// ── Timestretch stage (key-lock) ──────────────────────────────────────────────

pub struct TimestretechStage {
    speed:           f32,
    pitch_semitones: f32,
    // TODO: rubberband::RubberBandStretcher instance
}

impl TimestretechStage {
    /// Create with the R3 (Finer) engine for maximum quality.
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        let _ = (sample_rate, channels);
        // TODO: initialise rubberband with:
        //   RubberBandOption::ProcessRealTime
        //   | RubberBandOption::EngineFiner
        //   | RubberBandOption::PitchHighConsistency
        //   | RubberBandOption::ChannelsTogether
        Self { speed: 1.0, pitch_semitones: 0.0 }
    }
}

impl PipelineStage for TimestretechStage {
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        // TODO: push input to rubberband, retrieve available output.
        output.extend_from_slice(input);
    }

    fn set_speed(&mut self, speed: f32) {
        if (speed - self.speed).abs() > 1e-5 {
            self.speed = speed;
            // TODO: rb.set_time_ratio(1.0 / speed as f64)
        }
    }

    fn set_pitch_semitones(&mut self, semitones: f32) {
        if (semitones - self.pitch_semitones).abs() > 1e-4 {
            self.pitch_semitones = semitones;
            let scale = 2f64.powf(semitones as f64 / 12.0);
            let _ = scale;
            // TODO: rb.set_pitch_scale(scale)
        }
    }

    fn latency_frames(&self) -> usize {
        // Rubber Band R3 latency ≈ 2048–4096 frames depending on ratio.
        // Pre-roll with this many silence frames at startup.
        4096
    }

    fn reset(&mut self) {
        // TODO: rb.reset()
    }
}
