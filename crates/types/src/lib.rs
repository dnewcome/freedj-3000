pub mod beat;
pub mod cue;
pub mod engine;
pub mod media;
pub mod timecode;

pub use beat::*;
pub use cue::*;
pub use engine::*;
pub use media::*;
pub use timecode::*;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("seek out of range: sample {0}")]
    SeekOutOfRange(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec error: {0}")]
    Codec(String),
}

// ── Core audio decoder trait ───────────────────────────────────────────────────

/// Implemented by each format decoder (MP3, FLAC, AAC, WAV, AIFF).
/// All output is interleaved f32 PCM, normalised to ±1.0.
pub trait Decoder: Send {
    /// Decode the next block of frames into `out` (interleaved channels).
    /// Returns the number of **frames** written (not samples).
    /// Returns 0 at EOF.
    fn decode(&mut self, out: &mut [f32]) -> Result<usize, DecodeError>;

    /// Seek to the given sample position.
    /// The decoder may land at a nearby keyframe; the actual position is returned.
    fn seek(&mut self, sample: u64) -> Result<u64, DecodeError>;

    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u8;

    /// `None` if the duration is not known (e.g. streaming source).
    fn total_frames(&self) -> Option<u64>;
}

// ── Beat analysis trait ────────────────────────────────────────────────────────

/// Works in both offline (full-track batch) and online (streaming) modes.
/// The caller manages chunking; the analyzer just sees a stream of mono samples.
pub trait BeatAnalyzer: Send {
    /// Push a block of **mono** samples.  Call repeatedly as audio arrives.
    fn push(&mut self, samples: &[f32], sample_rate: u32);

    /// Current best estimate of the beat grid. `None` until enough data is seen.
    fn beat_grid(&self) -> Option<std::sync::Arc<BeatGrid>>;

    /// True when the analyzer has converged and the grid is reliable for quantize.
    fn is_stable(&self) -> bool;
}

// ── Timecode decoder trait ─────────────────────────────────────────────────────

pub trait TimecodeDecoder: Send {
    /// Feed one block of stereo timecode audio from the line input.
    fn process(&mut self, left: &[f32], right: &[f32]) -> TimecodeOutput;
    fn reset(&mut self);
}

// ── Processing pipeline stage trait ───────────────────────────────────────────

/// A single DSP stage: resampler or timestretcher.
/// Swapped atomically when key-lock mode changes; the RT thread always calls
/// the same interface.
pub trait PipelineStage: Send {
    /// Push input samples, receive output samples.
    /// Input and output lengths may differ (ratio != 1.0).
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>);

    /// Set the playback speed ratio (1.0 = normal).
    fn set_speed(&mut self, speed: f32);

    /// Set the pitch shift in semitones (0 = no shift).
    fn set_pitch_semitones(&mut self, semitones: f32);

    /// Inherent algorithmic latency introduced by this stage, in frames.
    fn latency_frames(&self) -> usize;

    fn reset(&mut self);
}
