//! Beat grid detection — works identically for offline (batch) and online (streaming) use.
//!
//! The caller manages chunking.  This struct just sees a stream of mono samples.
//!
//! Pipeline:
//!   1. Bass-focused onset strength signal (80–400 Hz bandpass + rectify + diff)
//!   2. Broadband onset detector (for bass-light material)
//!   3. Tempo induction via autocorrelation over a 6s window
//!   4. Beat phase estimation by maximising onset energy at grid positions
//!   5. Downbeat detection (bar structure)
//!   6. Variable-tempo grid refinement (for live recordings)

use std::sync::Arc;
use opendeck_types::{BeatAnalyzer, BeatGrid};

/// Autocorrelation window length in seconds.
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
    sample_rate:    u32,
    /// Accumulated mono samples (downmixed).
    samples:        Vec<f32>,
    /// Current best grid estimate.
    grid:           Option<Arc<BeatGrid>>,
    /// True once we have STABLE_SEC of audio and confidence is high.
    stable:         bool,
    /// Simple one-pole IIR state for the bass bandpass.
    lp_state:       f32,
    hp_state:       f32,
    prev_rectified: f32,
}

impl BeatAnalyzerImpl {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            samples: Vec::with_capacity(sample_rate as usize * 60),
            grid: None,
            stable: false,
            lp_state: 0.0,
            hp_state: 0.0,
            prev_rectified: 0.0,
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
        let window = &self.samples[self.samples.len() - ac_len..];

        // ── Stage 1: onset strength signal ────────────────────────────────────
        let onset = onset_strength(window, sr);

        // ── Stage 2: autocorrelation → tempo ──────────────────────────────────
        let (bpm, confidence) = estimate_bpm(&onset, sr);
        if confidence < 0.3 {
            return;
        }

        // ── Stage 3: beat phase ───────────────────────────────────────────────
        let anchor_sample = estimate_anchor(&onset, bpm, sr, self.samples.len() - ac_len);

        // ── Stage 4: downbeat ─────────────────────────────────────────────────
        // TODO: implement bar-level autocorrelation

        let mut grid = BeatGrid::new_constant(anchor_sample as u64, bpm as f64);
        grid.confidence = confidence;

        let secs = self.seconds_accumulated();
        self.stable = secs >= STABLE_SEC && confidence >= 0.7;
        if self.stable {
            grid.locked = false; // locked only by manual user action
        }

        self.grid = Some(Arc::new(grid));
    }
}

impl BeatAnalyzer for BeatAnalyzerImpl {
    fn push(&mut self, samples: &[f32], sample_rate: u32) {
        // Downmix to mono and accumulate.
        debug_assert_eq!(sample_rate, self.sample_rate,
            "sample rate changed mid-stream — re-create the analyzer");

        // Assume interleaved stereo input.
        for chunk in samples.chunks(2) {
            let mono = if chunk.len() == 2 {
                (chunk[0] + chunk[1]) * 0.5
            } else {
                chunk[0]
            };
            self.samples.push(mono);
        }

        // Re-run analysis every ~1 second of new data after warm-up.
        let sr = self.sample_rate as usize;
        if self.samples.len() >= (WARM_UP_SEC * self.sample_rate as f32) as usize
            && self.samples.len() % sr < 2048
        {
            self.run_analysis();
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

/// Compute an onset strength signal from mono PCM.
/// Returns a downsampled vector (~86 values/sec at hop=512).
fn onset_strength(samples: &[f32], sr: f32) -> Vec<f32> {
    const HOP: usize = 512;
    const WINDOW: usize = 1024;

    // Simple bass bandpass via two one-pole IIR filters.
    let lp_coeff = 1.0 - (-2.0 * std::f32::consts::PI * 400.0 / sr).exp();
    let hp_coeff = 1.0 - (-2.0 * std::f32::consts::PI * 80.0 / sr).exp();

    let mut lp = 0f32;
    let mut hp_prev = 0f32;
    let mut filtered = vec![0f32; samples.len()];

    for (i, &s) in samples.iter().enumerate() {
        lp += lp_coeff * (s - lp);
        let hp = lp - hp_prev;
        hp_prev = lp - hp_coeff * (lp - hp_prev);
        filtered[i] = hp.max(0.0); // half-wave rectify
    }

    // Compute RMS energy per hop and differentiate.
    let n_hops = samples.len() / HOP;
    let mut onset = vec![0f32; n_hops];
    let mut prev_energy = 0f32;

    for (h, chunk) in filtered.chunks(HOP).enumerate().take(n_hops) {
        let energy = (chunk.iter().map(|&x| x * x).sum::<f32>() / chunk.len() as f32).sqrt();
        onset[h] = (energy - prev_energy).max(0.0);
        prev_energy = energy;
    }

    onset
}

/// Autocorrelation-based BPM estimation.
/// Returns (bpm, confidence 0–1).
fn estimate_bpm(onset: &[f32], sr: f32) -> (f32, f32) {
    if onset.len() < 64 {
        return (120.0, 0.0);
    }

    const HOP: usize = 512;
    let hop_rate = sr / HOP as f32; // onsets per second

    let min_lag = (hop_rate * 60.0 / BPM_MAX) as usize;
    let max_lag = (hop_rate * 60.0 / BPM_MIN) as usize;
    let max_lag = max_lag.min(onset.len() - 1);

    if min_lag >= max_lag {
        return (120.0, 0.0);
    }

    // Compute autocorrelation over BPM range.
    let mut best_lag = min_lag;
    let mut best_val = f32::NEG_INFINITY;

    for lag in min_lag..=max_lag {
        let ac: f32 = onset.iter()
            .zip(onset[lag..].iter())
            .map(|(a, b)| a * b)
            .sum();
        if ac > best_val {
            best_val = ac;
            best_lag = lag;
        }
    }

    let bpm = hop_rate * 60.0 / best_lag as f32;

    // Normalise confidence: ratio of peak to mean of the AC range.
    let mean: f32 = {
        let sum: f32 = (min_lag..=max_lag)
            .map(|lag| onset.iter().zip(onset[lag..].iter()).map(|(a, b)| a * b).sum::<f32>())
            .sum();
        sum / (max_lag - min_lag + 1) as f32
    };
    let confidence = if mean > 0.0 { (best_val / mean - 1.0).clamp(0.0, 1.0) } else { 0.0 };

    (bpm, confidence)
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
