//! Audio decode and real-time playback with variable-speed timestretching.
//!
//! Architecture:
//!
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │ processor thread (non-RT)                                           │
//! │  samples[position..] → TimestretechStage (RubberBand R3) → rtrb   │
//! └──────────────────────────────────────┬──────────────────────────────┘
//!                                        │ lock-free ring buffer
//! ┌──────────────────────────────────────▼──────────────────────────────┐
//! │ cpal callback (RT thread)                                           │
//! │  rtrb consumer → output device                                      │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Shared atomics (all Relaxed unless noted):
//!   position    — source sample index; written by processor, readable by UI
//!   playing     — play/pause flag; written by UI/HID, read by both threads
//!   speed       — f32 bits; written by UI/HID, read by processor
//!   drain_flag  — set by processor on seek-detect; cleared by cpal on drain

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opendeck_decode::SymphoniaDecoder;
use opendeck_timestretch::TimestretechStage;
use opendeck_types::{Decoder, PipelineStage};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Frames fed to the timestretch engine per iteration.
const BLOCK_FRAMES: usize = 512;

/// Ring buffer capacity in device samples (not frames).
///
/// Keep this small so speed changes are heard immediately.  At 44.1kHz stereo
/// 8 192 samples ≈ 93 ms — large enough to absorb thread-scheduling jitter,
/// small enough that the pitch fader response feels instant.
const RING_BUFFER_SAMPLES: usize = 8_192;

/// Minimum free slots required before processing another block.
///
/// At the minimum supported speed (0.25×) RubberBand outputs 4× input frames:
///   BLOCK_FRAMES / 0.25 × device_ch = 512 × 4 × 2 = 4 096 samples worst case.
/// Must be < RING_BUFFER_SAMPLES so back-pressure never permanently stalls.
const BACK_PRESSURE_SLOTS: usize = BLOCK_FRAMES * 4 * 2; // = 4 096

/// If position jumps by more than this many source samples the processor
/// treats it as a seek and resets the timestretch engine.
const SEEK_THRESHOLD_SAMPLES: u64 = 4096 * 2; // ~46 ms at 44.1kHz stereo

// ── Public handle ─────────────────────────────────────────────────────────────

pub struct AudioHandle {
    /// Interleaved f32 PCM from the decoded file.
    pub samples:     Arc<Vec<f32>>,
    /// Current source read position in samples (not frames).
    /// Updated by the processor thread; read by renderer for waveform scroll.
    pub position:    Arc<AtomicU64>,
    /// Play / pause.
    pub playing:     Arc<AtomicBool>,
    /// Playback speed as f32 bits (1.0 = normal, 0.5 = half, 2.0 = double).
    /// Set via `speed_store` / `speed_load` helpers.
    pub speed:       Arc<AtomicU32>,
    pub sample_rate: u32,
    pub channels:    u8,
    _stream:     cpal::Stream,
    _processor:  thread::JoinHandle<()>,
}

impl AudioHandle {
    /// Convenience: read current speed.
    pub fn speed_load(&self) -> f32 {
        f32::from_bits(self.speed.load(Ordering::Relaxed))
    }

    /// Convenience: write a new speed.
    pub fn speed_store(&self, speed: f32) {
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }
}

// ── Constructor ───────────────────────────────────────────────────────────────

impl AudioHandle {
    pub fn open(path: &Path) -> Result<Self> {
        // ── 1. Decode entire file to memory ───────────────────────────────────
        log::info!("decoding {}", path.display());
        let mut decoder = SymphoniaDecoder::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        let file_sr   = decoder.sample_rate();
        let file_ch   = decoder.channels() as usize;
        let capacity  = decoder
            .total_frames()
            .map(|f| f as usize * file_ch)
            .unwrap_or(44_100 * 2 * 300);

        let mut samples: Vec<f32> = Vec::with_capacity(capacity);
        let mut buf = vec![0f32; 4096 * file_ch];

        loop {
            match decoder.decode(&mut buf)? {
                0 => break,
                frames => samples.extend_from_slice(&buf[..frames * file_ch]),
            }
        }
        log::info!(
            "decoded {} frames ({:.1}s) at {}Hz {}ch",
            samples.len() / file_ch,
            samples.len() as f64 / file_ch as f64 / file_sr as f64,
            file_sr, file_ch,
        );

        let samples = Arc::new(samples);

        // ── 2. Open cpal device ────────────────────────────────────────────────
        let host   = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no output audio device found")?;
        let supported = device
            .default_output_config()
            .context("failed to get default output config")?;

        let device_sr = supported.sample_rate().0;
        let device_ch = supported.channels() as usize;

        if device_sr != file_sr {
            log::warn!(
                "device sample rate {}Hz != file sample rate {}Hz — pitch will be wrong \
                 (resampling not yet implemented)",
                device_sr, file_sr,
            );
        }

        let stream_config = cpal::StreamConfig {
            channels:    device_ch as u16,
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        // ── 3. Shared state ────────────────────────────────────────────────────
        let position   = Arc::new(AtomicU64::new(0));
        let playing    = Arc::new(AtomicBool::new(true));
        let speed      = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let drain_flag = Arc::new(AtomicBool::new(false));

        // ── 4. Ring buffer ─────────────────────────────────────────────────────
        let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(RING_BUFFER_SAMPLES);

        // ── 5. Processor thread ────────────────────────────────────────────────
        let proc_samples    = Arc::clone(&samples);
        let proc_position   = Arc::clone(&position);
        let proc_playing    = Arc::clone(&playing);
        let proc_speed      = Arc::clone(&speed);
        let proc_drain_flag = Arc::clone(&drain_flag);

        let processor = thread::Builder::new()
            .name("audio-proc".into())
            .spawn(move || {
                processor_loop(
                    proc_samples,
                    proc_position,
                    proc_playing,
                    proc_speed,
                    proc_drain_flag,
                    file_sr,
                    file_ch,
                    device_ch,
                    producer,
                );
            })
            .context("failed to spawn processor thread")?;

        // ── 6. cpal stream (RT callback, no allocation) ────────────────────────
        let cpal_playing    = Arc::clone(&playing);
        let stream = device
            .build_output_stream::<f32, _, _>(
                &stream_config,
                move |out: &mut [f32], _info| {
                    // On seek, flush stale buffered audio.
                    if drain_flag.swap(false, Ordering::AcqRel) {
                        while consumer.pop().is_ok() {}
                    }

                    if !cpal_playing.load(Ordering::Relaxed) {
                        out.fill(0.0);
                        return;
                    }

                    for sample in out.iter_mut() {
                        *sample = consumer.pop().unwrap_or(0.0);
                    }
                },
                |err| log::error!("audio stream error: {err}"),
                None,
            )
            .context("failed to build output stream")?;

        stream.play().context("failed to start audio stream")?;
        log::info!("audio playback started (R3 timestretch pipeline)");

        Ok(Self {
            samples,
            position,
            playing,
            speed,
            sample_rate: file_sr,
            channels: file_ch as u8,
            _stream: stream,
            _processor: processor,
        })
    }
}

// ── Processor loop ────────────────────────────────────────────────────────────

fn processor_loop(
    samples:    Arc<Vec<f32>>,
    position:   Arc<AtomicU64>,
    playing:    Arc<AtomicBool>,
    speed:      Arc<AtomicU32>,
    drain_flag: Arc<AtomicBool>,
    sample_rate: u32,
    file_ch:    usize,
    device_ch:  usize,
    mut producer: rtrb::Producer<f32>,
) {
    let mut stretcher = TimestretechStage::new(sample_rate, file_ch as u8);

    // ── Pre-roll: push silence to warm up the RubberBand engine ──────────────
    // The R3 engine needs latency_frames of input before it produces output.
    // Without this, the first ~100ms of playback is silent (ring buffer stays
    // empty while RubberBand fills its internal pipeline).
    {
        let latency = stretcher.latency_frames();
        let silence = vec![0.0f32; latency * file_ch];
        let mut warmup = Vec::new();
        stretcher.process(&silence, &mut warmup);
        log::debug!("proc: pre-rolled {latency} silence frames");
    }

    // Internal read cursor — the processor owns this, UI/HID may jump `position`
    // to trigger a seek.
    let mut proc_pos: u64 = 0;

    // Output buffer for timestretched (file_ch interleaved) audio.
    let mut ts_out: Vec<f32> = Vec::with_capacity(BLOCK_FRAMES * file_ch * 8);

    loop {
        // ── Pause handling ────────────────────────────────────────────────────
        if !playing.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(2));
            continue;
        }

        // ── Seek detection ────────────────────────────────────────────────────
        let shared_pos = position.load(Ordering::Relaxed);
        if shared_pos.abs_diff(proc_pos) > SEEK_THRESHOLD_SAMPLES {
            log::debug!("proc: seek detected {proc_pos} → {shared_pos}");
            stretcher.reset();
            proc_pos = shared_pos;
            // Tell the cpal callback to flush stale buffered audio.
            drain_flag.store(true, Ordering::Release);
        }

        // ── Back-pressure ─────────────────────────────────────────────────────
        // At speeds below 1.0×, RubberBand outputs more frames than it takes in
        // (e.g. at 0.25× it outputs 4× input frames).  BACK_PRESSURE_SLOTS is
        // sized at 8× BLOCK_FRAMES × device_ch to guarantee we always have room
        // for the worst-case output before we process the next block.
        //
        // Sleep duration is proportional to how full the buffer is: when nearly
        // full (just above threshold) sleep ~5ms; when empty sleep ~0ms.  This
        // avoids both glitches (sleeping too long when buffer drains fast) and
        // CPU spin (sleeping too little when buffer is comfortably full).
        let free = producer.slots();
        if free < BACK_PRESSURE_SLOTS {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        // Buffer has room — sleep proportionally so we don't spin hot.
        // At 1× speed, one BLOCK_FRAMES = ~11.6ms of audio, so sleeping
        // (buffer_fill_ratio × 8ms) keeps us well ahead without wasting CPU.
        let fill_ratio = 1.0 - (free as f32 / RING_BUFFER_SAMPLES as f32);
        let yield_ms   = (fill_ratio * 8.0) as u64;
        if yield_ms > 0 {
            thread::sleep(Duration::from_millis(yield_ms));
        }

        // ── End of track ──────────────────────────────────────────────────────
        if proc_pos >= samples.len() as u64 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        // ── Read a block from the source ──────────────────────────────────────
        let src_start = proc_pos as usize;
        let src_end   = (src_start + BLOCK_FRAMES * file_ch).min(samples.len());
        let src_block = &samples[src_start..src_end];

        let final_block = src_end >= samples.len();

        // Advance the shared position so the UI/renderer sees it.
        proc_pos = src_end as u64;
        position.store(proc_pos, Ordering::Relaxed);

        // ── Update speed ───────────────────────────────────────────────────────
        let spd = f32::from_bits(speed.load(Ordering::Relaxed));
        stretcher.set_speed(spd.clamp(0.25, 4.0));

        // ── Timestretch ───────────────────────────────────────────────────────
        ts_out.clear();
        stretcher.process(src_block, &mut ts_out);

        if final_block && ts_out.is_empty() {
            // Flush RubberBand's tail at end of track.
            let silence = vec![0.0f32; BLOCK_FRAMES * file_ch];
            stretcher.process(&silence, &mut ts_out);
        }

        // ── Channel mix & push to ring buffer ─────────────────────────────────
        // Cap to available slots so we never silently drop frames.
        let out_frames    = ts_out.len() / file_ch;
        let slots_free    = producer.slots();
        let frames_to_push = out_frames.min(slots_free / device_ch);

        if frames_to_push < out_frames {
            log::warn!("proc: ring buffer full, dropped {} frames", out_frames - frames_to_push);
        }

        for i in 0..frames_to_push {
            for dev_ch_idx in 0..device_ch {
                let src_ch = dev_ch_idx.min(file_ch - 1);
                // producer.push() can't fail here — we checked slots_free above
                let _ = producer.push(ts_out[i * file_ch + src_ch]);
            }
        }
    }
}
