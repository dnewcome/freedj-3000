//! Waveform pre-computation — offline and streaming.
//!
//! Each column is [R, G, B, A] where:
//!   R = bass energy  (20–200 Hz)
//!   G = mid energy   (200–2 kHz)
//!   B = high energy  (2k–20 kHz)
//!   A = overall RMS  (for bar height in the display shader)
//!
//! The waveform texture is 1-pixel tall and N columns wide.
//! The display shader draws a centered bar chart scaled by A,
//! colored by RGB.

use rustfft::{num_complex::Complex, FftPlanner};

const FFT_SIZE: usize = 2048;
pub const HOP_SIZE: usize = 512;

/// One display column: [R, G, B, amplitude] each 0–255.
pub type WaveformColumn = [u8; 4];

/// A fully computed waveform ready for GPU upload.
pub struct WaveformCache {
    pub columns:     Vec<WaveformColumn>,
    pub sample_rate: u32,
    pub hop_size:    usize,
}

impl WaveformCache {
    /// Column index for the given sample position.
    pub fn column_for_sample(&self, sample: u64) -> usize {
        (sample as usize / self.hop_size).min(self.columns.len().saturating_sub(1))
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Builds a WaveformCache incrementally.
/// Feed interleaved stereo f32 samples via `push`, then call `finish`.
pub struct WaveformBuilder {
    sample_rate: u32,
    columns:     Vec<WaveformColumn>,
    /// Mono samples accumulated until we have enough for a hop.
    buf:         Vec<f32>,
    window:      Vec<f32>,
    planner:     FftPlanner<f32>,
}

impl WaveformBuilder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            columns: Vec::new(),
            buf: Vec::with_capacity(FFT_SIZE * 2),
            window: hann_window(FFT_SIZE),
            planner: FftPlanner::new(),
        }
    }

    /// Feed a block of interleaved stereo f32 samples.
    pub fn push(&mut self, stereo: &[f32]) {
        for chunk in stereo.chunks(2) {
            let mono = if chunk.len() == 2 {
                (chunk[0] + chunk[1]) * 0.5
            } else {
                chunk[0]
            };
            self.buf.push(mono);

            if self.buf.len() >= FFT_SIZE {
                self.compute_column();
                // Slide forward by HOP_SIZE.
                self.buf.drain(..HOP_SIZE);
            }
        }
    }

    pub fn finish(self) -> WaveformCache {
        WaveformCache {
            columns:     self.columns,
            sample_rate: self.sample_rate,
            hop_size:    HOP_SIZE,
        }
    }

    fn compute_column(&mut self) {
        let fft = self.planner.plan_fft_forward(FFT_SIZE);

        // Apply Hann window and convert to complex.
        let mut buf: Vec<Complex<f32>> = self.buf[..FFT_SIZE]
            .iter()
            .zip(self.window.iter())
            .map(|(&s, &w)| Complex::new(s * w, 0.0))
            .collect();

        fft.process(&mut buf);

        // We only need the positive frequencies: bins 0..FFT_SIZE/2.
        let magnitudes: Vec<f32> = buf[..FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm() / FFT_SIZE as f32)
            .collect();

        let n = magnitudes.len();
        let bin_hz = self.sample_rate as f32 / FFT_SIZE as f32;

        let bass_end = ((200.0  / bin_hz) as usize).min(n);
        let mid_end  = ((2000.0 / bin_hz) as usize).min(n);

        let bass = rms(&magnitudes[1..bass_end]);
        let mid  = rms(&magnitudes[bass_end..mid_end]);
        let high = rms(&magnitudes[mid_end..]);

        // Overall amplitude (for bar height) — weighted sum of all bands.
        let overall = rms(&magnitudes[1..]);

        // Scale and gamma-correct for visual appeal.
        // Boost factor tuned empirically for typical music levels.
        let boost = 6.0_f32;
        self.columns.push([
            to_u8(bass    * boost),
            to_u8(mid     * boost),
            to_u8(high    * boost),
            to_u8(overall * boost),
        ]);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn hann_window(n: usize) -> Vec<f32> {
    use std::f32::consts::PI;
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (n - 1) as f32).cos()))
        .collect()
}

fn rms(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    (v.iter().map(|&x| x * x).sum::<f32>() / v.len() as f32).sqrt()
}

/// Sqrt gamma then clamp to 0–255.
fn to_u8(v: f32) -> u8 {
    (v.sqrt().clamp(0.0, 1.0) * 255.0) as u8
}
