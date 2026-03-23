//! DSP pipeline stage management.
//!
//! The active pipeline is swapped atomically when key-lock changes.
//! The RT thread always calls the same `PipelineStage` interface.

use opendeck_types::PipelineStage;

pub enum Pipeline {
    Passthrough,
    Active(Box<dyn PipelineStage>),
}

impl Pipeline {
    /// Process one frame of audio through the active stage.
    /// Returns a `[f32; 2]` stereo frame.
    pub fn process_frame(
        &mut self,
        input: &[f32],
        speed: f32,
        pitch_semitones: f32,
    ) -> (Vec<f32>, usize) {
        match self {
            Pipeline::Passthrough => (input.to_vec(), input.len() / 2),
            Pipeline::Active(stage) => {
                stage.set_speed(speed);
                stage.set_pitch_semitones(pitch_semitones);
                let mut output = Vec::new();
                stage.process(input, &mut output);
                let frames = output.len() / 2;
                (output, frames)
            }
        }
    }
}
