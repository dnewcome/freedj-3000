use serde::{Deserialize, Serialize};

/// Complete beat grid for a track.
///
/// For constant-tempo tracks: only `anchor_sample` and `bpm` are used.
/// For variable-tempo tracks: `beats` contains the sample offset of every
/// detected beat, and `bpm` is the average (informational only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatGrid {
    /// Sample offset of beat 0 (the first beat anchor).
    pub anchor_sample: u64,

    /// Constant BPM (average for variable-tempo tracks).
    pub bpm: f64,

    /// Per-beat sample offsets for variable-tempo tracks.
    /// Empty = constant BPM from anchor.
    pub beats: Vec<u64>,

    /// Which beat within the bar is beat 0 (0 = downbeat).
    pub downbeat_offset: u8,

    /// 0.0–1.0 analysis confidence.
    pub confidence: f32,

    /// True if manually reviewed/corrected — grid will not be re-analysed.
    pub locked: bool,
}

impl BeatGrid {
    pub fn new_constant(anchor_sample: u64, bpm: f64) -> Self {
        Self {
            anchor_sample,
            bpm,
            beats: Vec::new(),
            downbeat_offset: 0,
            confidence: 0.0,
            locked: false,
        }
    }

    /// Sample position of the given beat index (may be negative for pre-roll).
    pub fn sample_of_beat(&self, beat_index: i64, sample_rate: u32) -> u64 {
        if self.beats.is_empty() {
            let samples_per_beat = sample_rate as f64 * 60.0 / self.bpm;
            let offset = (beat_index as f64 * samples_per_beat).round() as i64;
            (self.anchor_sample as i64 + offset).max(0) as u64
        } else {
            let idx = beat_index.clamp(0, self.beats.len() as i64 - 1) as usize;
            self.beats[idx]
        }
    }

    /// Fractional beat number at the given sample position.
    pub fn beat_at_sample(&self, sample: u64, sample_rate: u32) -> f64 {
        if self.beats.is_empty() {
            let samples_per_beat = sample_rate as f64 * 60.0 / self.bpm;
            (sample as f64 - self.anchor_sample as f64) / samples_per_beat
        } else {
            match self.beats.binary_search(&sample) {
                Ok(i) => i as f64,
                Err(i) => {
                    if i == 0 {
                        return 0.0;
                    }
                    let lo = self.beats[i - 1];
                    let hi = self.beats[i.min(self.beats.len() - 1)];
                    let t = (sample - lo) as f64 / (hi - lo) as f64;
                    (i - 1) as f64 + t
                }
            }
        }
    }

    /// Phase within the current beat: 0.0 = on the beat, 1.0 = next beat.
    pub fn phase_at_sample(&self, sample: u64, sample_rate: u32) -> f32 {
        self.beat_at_sample(sample, sample_rate).fract() as f32
    }

    /// Sample offset of the nearest beat that is at or before `sample`.
    pub fn nearest_beat_before(&self, sample: u64, sample_rate: u32) -> u64 {
        let beat = self.beat_at_sample(sample, sample_rate).floor() as i64;
        self.sample_of_beat(beat, sample_rate)
    }

    /// Duration in samples of one beat at the given position.
    pub fn samples_per_beat_at(&self, sample: u64, sample_rate: u32) -> f64 {
        if self.beats.is_empty() {
            sample_rate as f64 * 60.0 / self.bpm
        } else {
            match self.beats.binary_search(&sample) {
                Ok(i) | Err(i) => {
                    let i = i.saturating_sub(1).min(self.beats.len() - 2);
                    (self.beats[i + 1] - self.beats[i]) as f64
                }
            }
        }
    }
}
