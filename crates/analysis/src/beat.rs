//! Beat grid detection — works identically for offline (batch) and online (streaming) use.
//!
//! The caller manages chunking.  This struct just sees a stream of mono samples.
//!
//! Pipeline:
//!   1. Tempo detection via MiniBPM (6 s analysis window)
//!   2. Beat phase estimation by maximising onset energy at grid positions
//!   3. Downbeat detection (bar structure) — TODO

use std::sync::Arc;
use opendeck_types::{BeatAnalyzer, BeatGrid};
use minibpm_sys::MiniBpm;

/// Analysis window length in seconds fed to MiniBPM.
const AC_WINDOW_SEC: f32 = 6.0;
/// Minimum BPM to consider.
const BPM_MIN: f32 = 60.0;
/// Maximum BPM to consider.
const BPM_MAX: f32 = 200.0;
/// How many seconds of audio before we emit a first estimate.
const WARM_UP_SEC: f32 = 5.0;
/// How many seconds before we call the grid "stable".
const STABLE_SEC: f32 = 15.0;

pub struct BeatAnalyzerImpl {
    sample_rate: u32,
    /// Accumulated mono samples (downmixed).
    samples:     Vec<f32>,
    /// Current best grid estimate.
    grid:        Option<Arc<BeatGrid>>,
    /// True once we have STABLE_SEC of audio.
    stable:      bool,
}

impl BeatAnalyzerImpl {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            samples: Vec::with_capacity(sample_rate as usize * 60),
            grid: None,
            stable: false,
        }
    }

    fn seconds_accumulated(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }

    fn run_analysis(&mut self) {
        let sr = self.sample_rate as f32;
        let ac_len = (AC_WINDOW_SEC * sr) as usize;
        if self.samples.len() < ac_len {
            return;
        }

        // Use the last AC_WINDOW_SEC of audio.
        let window_start = self.samples.len() - ac_len;
        let window = &self.samples[window_start..];

        // ── Stage 1: tempo via MiniBPM ────────────────────────────────────────
        let mut detector = MiniBpm::new(sr);
        detector.set_bpm_range(BPM_MIN as f64, BPM_MAX as f64);
        let bpm = match detector.estimate_tempo(window) {
            Some(b) => {
                log::info!("MiniBPM: {:.1} BPM", b);
                b as f32
            }
            None => {
                log::debug!("MiniBPM returned no estimate");
                return;
            }
        };

        // ── Stage 2: beat phase ───────────────────────────────────────────────
        let onset = onset_strength(window, sr);
        let anchor_sample = estimate_anchor(&onset, bpm, sr, window_start);

        // ── Stage 3: downbeat ─────────────────────────────────────────────────
        // TODO: implement bar-level autocorrelation

        // MiniBPM doesn't return a confidence score; use a fixed value.
        const CONFIDENCE: f32 = 0.8;
        let mut grid = BeatGrid::new_constant(anchor_sample as u64, bpm as f64);
        grid.confidence = CONFIDENCE;

        self.stable = self.seconds_accumulated() >= STABLE_SEC;
        self.grid = Some(Arc::new(grid));
    }
}

impl BeatAnalyzer for BeatAnalyzerImpl {
    fn push(&mut self, samples: &[f32], sample_rate: u32) {
        // Downmix to mono and accumulate.
        debug_assert_eq!(sample_rate, self.sample_rate,
            "sample rate changed mid-stream — re-create the analyzer");

        // Assume interleaved stereo input.
        let sr = self.sample_rate as usize;
        let prev_len = self.samples.len();
        for chunk in samples.chunks(2) {
            let mono = if chunk.len() == 2 {
                (chunk[0] + chunk[1]) * 0.5
            } else {
                chunk[0]
            };
            self.samples.push(mono);
        }

        // Re-run analysis once per second of new data after warm-up.
        let warm_up = (WARM_UP_SEC * self.sample_rate as f32) as usize;
        let new_len = self.samples.len();
        if new_len >= warm_up {
            let prev_sec = prev_len / sr;
            let new_sec  = new_len  / sr;
            if new_sec > prev_sec {
                self.run_analysis();
            }
        }
    }

    fn beat_grid(&self) -> Option<Arc<BeatGrid>> {
        self.grid.clone()
    }

    fn is_stable(&self) -> bool {
        self.stable
    }
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// Compute an onset strength signal from mono PCM using half-wave rectified
/// spectral flux.  Operates on sub-bands so kick, snare, and hi-hat all
/// contribute, making the result more robust than a single-band energy
/// differentiator.  Returns ~86 values/sec at hop=512.
fn onset_strength(samples: &[f32], sr: f32) -> Vec<f32> {
    use rustfft::{num_complex::Complex, FftPlanner};

    const HOP:    usize = 512;
    const WINDOW: usize = 1024;

    let n_hops = samples.len().saturating_sub(WINDOW) / HOP;
    if n_hops == 0 {
        return vec![];
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);

    // Hann window coefficients.
    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32).cos()))
        .collect();

    let bin_hz  = sr / WINDOW as f32;
    // Divide spectrum into three sub-bands: bass, mid, high.
    let bass_end = (300.0  / bin_hz) as usize;
    let mid_end  = (3000.0 / bin_hz) as usize;
    let n_bins   = WINDOW / 2;

    let mut onset     = vec![0f32; n_hops];
    let mut prev_mag  = vec![0f32; n_bins];

    for h in 0..n_hops {
        let start = h * HOP;
        let mut buf: Vec<Complex<f32>> = samples[start..start + WINDOW]
            .iter()
            .zip(hann.iter())
            .map(|(&s, &w)| Complex::new(s * w, 0.0))
            .collect();

        fft.process(&mut buf);

        let mag: Vec<f32> = buf[..n_bins]
            .iter()
            .map(|c| c.norm() / WINDOW as f32)
            .collect();

        // Half-wave rectified spectral flux, weighted by sub-band.
        // Bass gets 3× weight so the kick drum dominates as expected.
        let flux: f32 = mag.iter()
            .zip(prev_mag.iter())
            .enumerate()
            .map(|(bin, (&m, &p))| {
                let diff = (m - p).max(0.0);
                let weight = if bin < bass_end { 3.0 }
                             else if bin < mid_end { 1.0 }
                             else { 0.5 };
                diff * weight
            })
            .sum();

        onset[h]  = flux;
        prev_mag  = mag;
    }

    onset
}

/// Find the beat anchor sample by maximising onset energy at grid positions.
fn estimate_anchor(onset: &[f32], bpm: f32, sr: f32, sample_offset: usize) -> usize {
    const HOP: usize = 512;
    let hop_rate = sr / HOP as f32;
    let period = hop_rate * 60.0 / bpm;

    let n_phases = period as usize;
    let mut best_phase = 0;
    let mut best_score = f32::NEG_INFINITY;

    for phase in 0..n_phases {
        let mut score = 0f32;
        let mut pos = phase as f32;
        while (pos as usize) < onset.len() {
            score += onset[pos as usize];
            pos += period;
        }
        if score > best_score {
            best_score = score;
            best_phase = phase;
        }
    }

    sample_offset + best_phase * HOP
}
