//! Audio decode and playback for the MVP.
//!
//! Decodes an entire file into a `Vec<f32>` (interleaved stereo) then drives
//! a cpal output stream directly from that buffer.  The `position` atomic
//! tracks how many samples have been consumed — shared with the renderer for
//! waveform scrolling.

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opendeck_decode::SymphoniaDecoder;
use opendeck_types::Decoder;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

pub struct AudioHandle {
    /// Interleaved f32 PCM, stereo.
    pub samples:     Arc<Vec<f32>>,
    /// Current position in samples (not frames).
    pub position:    Arc<AtomicU64>,
    /// True = playing, False = paused.
    pub playing:     Arc<AtomicBool>,
    pub sample_rate: u32,
    pub channels:    u8,
    /// Keep the stream alive — dropping it stops playback.
    _stream: cpal::Stream,
}

impl AudioHandle {
    /// Decode the file at `path` and start playback immediately.
    pub fn open(path: &Path) -> Result<Self> {
        // ── 1. Decode entire file to memory ───────────────────────────────────
        log::info!("decoding {}", path.display());
        let mut decoder = SymphoniaDecoder::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        let file_sr  = decoder.sample_rate();
        let channels = decoder.channels();
        let capacity = decoder
            .total_frames()
            .map(|f| f as usize * channels as usize)
            .unwrap_or(44_100 * 2 * 300);  // 5-minute fallback

        let mut samples: Vec<f32> = Vec::with_capacity(capacity);
        let mut buf = vec![0f32; 4096 * channels as usize];

        loop {
            match decoder.decode(&mut buf)? {
                0 => break,
                frames => {
                    samples.extend_from_slice(&buf[..frames * channels as usize]);
                }
            }
        }
        log::info!(
            "decoded {} frames ({:.1}s) at {}Hz {}ch",
            samples.len() / channels as usize,
            samples.len() as f64 / channels as f64 / file_sr as f64,
            file_sr,
            channels,
        );

        let samples   = Arc::new(samples);
        let position  = Arc::new(AtomicU64::new(0));
        let playing   = Arc::new(AtomicBool::new(true));

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
                 (resampling not yet implemented for MVP)",
                device_sr, file_sr
            );
        }

        let stream_config = cpal::StreamConfig {
            channels:    device_ch as u16,
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let s_clone   = Arc::clone(&samples);
        let p_clone   = Arc::clone(&position);
        let pl_clone  = Arc::clone(&playing);
        let file_ch   = channels as usize;

        let stream = device
            .build_output_stream::<f32, _, _>(
                &stream_config,
                move |out: &mut [f32], _info| {
                    fill_output(out, &s_clone, &p_clone, &pl_clone, file_ch, device_ch);
                },
                |err| log::error!("audio stream error: {err}"),
                None,
            )
            .context("failed to build output stream")?;

        stream.play().context("failed to start audio stream")?;
        log::info!("audio playback started");

        Ok(Self {
            samples,
            position,
            playing,
            sample_rate: file_sr,
            channels,
            _stream: stream,
        })
    }
}

/// Called by cpal on the audio thread — must not allocate or block.
fn fill_output(
    out:      &mut [f32],
    samples:  &Arc<Vec<f32>>,
    position: &Arc<AtomicU64>,
    playing:  &Arc<AtomicBool>,
    file_ch:  usize,
    dev_ch:   usize,
) {
    if !playing.load(Ordering::Relaxed) {
        out.fill(0.0);
        return;
    }

    let pos = position.load(Ordering::Relaxed) as usize;
    let src = samples.as_slice();

    // Number of device frames requested.
    let dev_frames = out.len() / dev_ch;

    for frame in 0..dev_frames {
        let src_frame = pos / file_ch + frame;
        let src_base  = src_frame * file_ch;

        for ch in 0..dev_ch {
            // For mono files play on both channels; for stereo map 1:1.
            let src_ch = ch.min(file_ch - 1);
            out[frame * dev_ch + ch] = src.get(src_base + src_ch).copied().unwrap_or(0.0);
        }
    }

    let consumed = (dev_frames * file_ch) as u64;
    let new_pos  = (pos as u64 + consumed).min(samples.len() as u64);
    position.store(new_pos, Ordering::Relaxed);
}
