//! Two pipeline stage implementations:
//!
//! - `ResampleStage`:      speed change without pitch change (no key-lock).
//!                         Uses `rubato` (pure Rust, sinc interpolation).
//!
//! - `TimestretechStage`:  speed change with constant pitch (key-lock).
//!                         Wraps Rubber Band Library R3 engine via C FFI.

mod rubberband_sys;

use opendeck_types::PipelineStage;
use rubberband_sys as rb;

// ── ResampleStage (no key-lock) ───────────────────────────────────────────────

pub struct ResampleStage {
    speed:   f32,
}

impl ResampleStage {
    pub fn new(_sample_rate: u32, _channels: u8) -> Self {
        Self { speed: 1.0 }
    }
}

impl PipelineStage for ResampleStage {
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        // TODO: run rubato SincFixedOut resampler at self.speed ratio.
        output.extend_from_slice(input);
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    fn set_pitch_semitones(&mut self, _semitones: f32) {
        // No pitch shifting — pitch follows speed.
    }

    fn latency_frames(&self) -> usize {
        256 // rubato SincFixedOut at quality 5
    }

    fn reset(&mut self) {
        // TODO: flush rubato internal state
    }
}

// ── TimestretechStage (key-lock via Rubber Band R3) ───────────────────────────

/// RAII wrapper around a `RubberBandState` pointer.
///
/// # Safety
/// `RubberBandState` is not Send by default (it is a raw pointer), but
/// Rubber Band's real-time path is explicitly designed to be called from a
/// single audio thread, so we assert Send here and ensure we only ever
/// touch `state` from one thread at a time (enforced by the `&mut self`
/// receiver on every method).
struct RbHandle {
    state: rb::RubberBandState,
}

// SAFETY: we never share the pointer across threads; all access is through
// `&mut self`, which the borrow checker prevents from being aliased.
unsafe impl Send for RbHandle {}

impl Drop for RbHandle {
    fn drop(&mut self) {
        unsafe { rb::rubberband_delete(self.state); }
    }
}

// Block size fed to rubberband_process at a time (frames, not samples).
// 512 gives good latency at 44.1 kHz (~11 ms).
const BLOCK_FRAMES: usize = 512;

pub struct TimestretechStage {
    rb:              RbHandle,
    channels:        usize,
    speed:           f32,
    pitch_semitones: f32,
    // Deinterleaved input scratch buffers (one per channel).
    in_bufs:         Vec<Vec<f32>>,
    in_ptrs:         Vec<*const f32>,
    // Deinterleaved output scratch buffers (one per channel).
    out_bufs:        Vec<Vec<f32>>,
    out_ptrs:        Vec<*mut f32>,
}

// SAFETY: same argument as RbHandle above; all raw pointers point into
// in_bufs/out_bufs which live as long as the struct.
unsafe impl Send for TimestretechStage {}

impl TimestretechStage {
    /// Create with the R3 (Finer) engine for maximum quality.
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        let ch = channels as usize;

        let state = unsafe {
            rb::rubberband_new(
                sample_rate,
                channels as u32,
                rb::REALTIME_R3_OPTIONS,
                1.0, // time ratio  (1.0 = unchanged)
                1.0, // pitch scale (1.0 = unchanged)
            )
        };
        assert!(!state.is_null(), "rubberband_new returned null");

        unsafe {
            rb::rubberband_set_max_process_size(state, BLOCK_FRAMES as u32);
        }

        let latency = unsafe { rb::rubberband_get_latency(state) };
        log::info!("Rubber Band R3 engine initialised — latency: {latency} frames");

        let in_bufs:  Vec<Vec<f32>> = vec![vec![0.0f32; BLOCK_FRAMES]; ch];
        let out_bufs: Vec<Vec<f32>> = vec![vec![0.0f32; BLOCK_FRAMES]; ch];
        let in_ptrs:  Vec<*const f32> = in_bufs.iter().map(|v| v.as_ptr()).collect();
        let out_ptrs: Vec<*mut f32>   = out_bufs.iter().map(|v| v.as_ptr() as *mut f32).collect();

        Self {
            rb: RbHandle { state },
            channels: ch,
            speed: 1.0,
            pitch_semitones: 0.0,
            in_bufs,
            in_ptrs,
            out_bufs,
            out_ptrs,
        }
    }

    /// Feed one block of interleaved frames into Rubber Band and collect
    /// whatever output is ready into `output`.
    fn push_block(&mut self, frames: &[f32], output: &mut Vec<f32>) {
        let n = frames.len() / self.channels;
        debug_assert!(n <= BLOCK_FRAMES);

        // Deinterleave.
        for ch in 0..self.channels {
            for i in 0..n {
                self.in_bufs[ch][i] = frames[i * self.channels + ch];
            }
            self.in_ptrs[ch] = self.in_bufs[ch].as_ptr();
        }

        unsafe {
            rb::rubberband_process(
                self.rb.state,
                self.in_ptrs.as_ptr(),
                n as u32,
                0, // not final
            );
        }

        // Drain all available output.
        loop {
            let avail = unsafe { rb::rubberband_available(self.rb.state) };
            if avail <= 0 { break; }

            let to_read = (avail as usize).min(BLOCK_FRAMES);

            // Update out_ptrs in case Vec reallocated (shouldn't, but be safe).
            for ch in 0..self.channels {
                self.out_ptrs[ch] = self.out_bufs[ch].as_mut_ptr();
            }

            let got = unsafe {
                rb::rubberband_retrieve(
                    self.rb.state,
                    self.out_ptrs.as_ptr() as *const *mut f32,
                    to_read as u32,
                )
            } as usize;

            // Re-interleave into output.
            let prev_len = output.len();
            output.resize(prev_len + got * self.channels, 0.0);
            for i in 0..got {
                for ch in 0..self.channels {
                    output[prev_len + i * self.channels + ch] = self.out_bufs[ch][i];
                }
            }
        }
    }
}

impl PipelineStage for TimestretechStage {
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        // Feed input in BLOCK_FRAMES-sized chunks.
        let stride = BLOCK_FRAMES * self.channels;
        let mut offset = 0;
        while offset < input.len() {
            let end = (offset + stride).min(input.len());
            self.push_block(&input[offset..end], output);
            offset = end;
        }
    }

    fn set_speed(&mut self, speed: f32) {
        if (speed - self.speed).abs() > 1e-5 {
            self.speed = speed;
            unsafe {
                rb::rubberband_set_time_ratio(self.rb.state, 1.0 / speed as f64);
            }
        }
    }

    fn set_pitch_semitones(&mut self, semitones: f32) {
        if (semitones - self.pitch_semitones).abs() > 1e-4 {
            self.pitch_semitones = semitones;
            let scale = 2f64.powf(semitones as f64 / 12.0);
            unsafe {
                rb::rubberband_set_pitch_scale(self.rb.state, scale);
            }
        }
    }

    fn latency_frames(&self) -> usize {
        unsafe { rb::rubberband_get_latency(self.rb.state) as usize }
    }

    fn reset(&mut self) {
        unsafe { rb::rubberband_reset(self.rb.state); }
    }
}
