//! Traktor Kontrol S2 MK2 input — raw USB HID.
//!
//! The S2 MK2 reports absolute jog position as a 24-bit counter across
//! bytes [1, 2, 3] of each HID report (byte 3 = MSB, byte 1 = LSB).
//! We compute the signed delta between frames and map it to playhead movement.
//!
//! Run with RUST_LOG=opendeck::midi=debug to see all byte changes when
//! you press buttons you want to map.

use hidapi::HidApi;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const VID: u16 = 0x17CC;
const PID: u16 = 0x1320;

// ── Button mappings — (byte_index, bitmask) ───────────────────────────────────

const MAP_PLAY:          (usize, u8) = (11, 0x01);
const MAP_CUE:           (usize, u8) = (11, 0x02);
// byte[10] bit 0x01 — capacitive touch only; S2 MK2 has no separate press sensor.
const MAP_PLATTER_TOUCH: (usize, u8) = (10, 0x01);
const MAP_SEEK_FWD:      (usize, u8) = (0xFF, 0xFF);
const MAP_SEEK_REV:      (usize, u8) = (0xFF, 0xFF);

// ── Pitch fader — byte 7, absolute 8-bit ─────────────────────────────────────
// Higher value = fader pushed down = slower.  Lower = fader up = faster.
// Center is calibrated from the fader position in the first HID report —
// start the app with the fader at its center detent.

const PITCH_FADER_BYTE:  usize = 7;
/// ±% pitch range — 0.16 = ±16% (wide DJ range).  CDJ-3000 default is 0.08 (±8%).
const PITCH_FADER_RANGE: f32   = 0.16;

// ── Jog wheel sensitivity ─────────────────────────────────────────────────────
// The 24-bit counter spans ~16.7M counts per full revolution.

/// Scrub (platter top touched): direct position control.  ~3 s per revolution.
const SCRUB_SAMPLES_PER_COUNT: f64 = 0.025;
/// Nudge speed offset per jog count.
/// The encoder produces ~10M counts/rev; at a slow nudge spin (~0.25 rev/s)
/// that's ~250K counts per 100ms poll.  5e-8 × 250K ≈ 1.25% offset.
const NUDGE_SPEED_PER_COUNT: f32 = 0.00000005;

/// How long the jog must be idle before speed snaps back to the pitch fader value.
const NUDGE_RELEASE_MS: u128 = 150;

// ─────────────────────────────────────────────────────────────────────────────

pub struct MidiHandle {
    _thread: thread::JoinHandle<()>,
}

impl MidiHandle {
    pub fn connect(
        playing:     Arc<AtomicBool>,
        position:    Arc<AtomicU64>,
        speed:       Arc<AtomicU32>,
        sample_rate: u32,
        channels:    u8,
        samples_len: usize,
    ) -> Option<Self> {
        let api = HidApi::new().map_err(|e| log::warn!("HID: init failed: {e}")).ok()?;

        if api.device_list().find(|d| d.vendor_id() == VID && d.product_id() == PID).is_none() {
            log::info!("HID: Traktor Kontrol S2 MK2 not found");
            return None;
        }

        log::info!("HID: found Traktor Kontrol S2 MK2 — spawning input thread");

        let seek_delta = sample_rate as u64 * channels as u64 * 4;
        let max_pos    = samples_len as u64;

        let t = thread::Builder::new()
            .name("s2-hid".into())
            .spawn(move || run_loop(playing, position, speed, seek_delta, max_pos))
            .expect("failed to spawn S2 thread");

        Some(MidiHandle { _thread: t })
    }
}

fn run_loop(
    playing:    Arc<AtomicBool>,
    position:   Arc<AtomicU64>,
    speed:      Arc<AtomicU32>,
    seek_delta: u64,
    max_pos:    u64,
) {
    let api = match HidApi::new() {
        Ok(a)  => a,
        Err(e) => { log::error!("HID: {e}"); return; }
    };
    let device = match api.open(VID, PID) {
        Ok(d)  => d,
        Err(e) => { log::error!("HID: failed to open S2: {e}"); return; }
    };

    log::info!("HID: S2 connected — jog wheel + pitch fader active");

    let mut prev_report  = [0u8; 64];
    let mut buf          = [0u8; 64];
    let mut prev_jog:    Option<u32> = None;
    let mut prev_btns    = [0u8; 64];
    let mut touched      = false;
    // Calibrated on first HID report — start app with fader at center detent.
    let mut pitch_center: Option<u8> = None;
    // The "true" speed set by the pitch fader; nudge offsets from this and
    // snaps back when the jog comes to rest.
    let mut base_speed: f32 = 1.0;
    // Timestamp of the last nudge movement; snap-back fires after NUDGE_RELEASE_MS.
    let mut last_nudge: Option<Instant> = None;

    loop {
        let n = match device.read_timeout(&mut buf, 100) {
            Ok(n)  => n,
            Err(e) => { log::error!("HID read: {e}"); break; }
        };
        if n == 0 { continue; }

        let report = &buf[..n];

        // ── Discovery logging ─────────────────────────────────────────────────
        for i in 0..n {
            if report[i] != prev_report[i] {
                log::debug!("S2 byte[{i:02}] changed: 0x{:02X} → 0x{:02X}",
                    prev_report[i], report[i]);
            }
        }
        prev_report[..n].copy_from_slice(report);

        // ── Platter touch sensor ──────────────────────────────────────────────
        let (t_byte, t_mask) = MAP_PLATTER_TOUCH;
        if t_byte < n {
            let now = (report[t_byte] & t_mask) != 0;
            if now != touched {
                touched = now;
                log::debug!("S2 platter {}", if touched { "touched (scrub)" } else { "released" });
            }
        }

        // ── Jog wheel — 24-bit absolute counter (byte3=MSB, byte1=LSB) ────────
        if n >= 4 {
            let jog = (report[3] as u32) << 16
                    | (report[2] as u32) << 8
                    | (report[1] as u32);

            if let Some(prev) = prev_jog {
                // Signed 24-bit delta with wraparound handling.
                let raw_delta = jog.wrapping_sub(prev) as i32;
                let delta = if raw_delta > 0x7FFFFF {
                    raw_delta - 0x1000000
                } else if raw_delta < -0x7FFFFF {
                    raw_delta + 0x1000000
                } else {
                    raw_delta
                };

                if delta != 0 {
                    if touched {
                        // Touch: scrub — directly moves the playhead.
                        let samples = (delta.abs() as f64 * SCRUB_SAMPLES_PER_COUNT) as u64;
                        let pos = position.load(Ordering::Relaxed);
                        if delta > 0 {
                            position.store(pos.saturating_add(samples).min(max_pos), Ordering::Relaxed);
                        } else {
                            position.store(pos.saturating_sub(samples), Ordering::Relaxed);
                        }
                    } else {
                        // No touch + spinning: nudge.
                        // offset = delta * k so constant velocity → constant pitch %.
                        // Faster spin → larger delta → proportionally larger offset.
                        let offset = delta as f32 * NUDGE_SPEED_PER_COUNT;
                        speed.store((base_speed + offset).clamp(0.25, 4.0).to_bits(), Ordering::Relaxed);
                        last_nudge = Some(Instant::now());
                    }
                } else if !touched {
                    // Jog at rest: snap back after the idle window expires.
                    // Using a time threshold avoids false snap-backs on slow spins
                    // where individual reports can have delta == 0 between ticks.
                    let idle = last_nudge.map_or(true, |t| t.elapsed() > Duration::from_millis(NUDGE_RELEASE_MS as u64));
                    if idle {
                        speed.store(base_speed.to_bits(), Ordering::Relaxed);
                        last_nudge = None;
                    }
                }
            }
            prev_jog = Some(jog);
        }

        // ── Pitch fader — byte 7, absolute ────────────────────────────────────
        if PITCH_FADER_BYTE < n {
            let raw = report[PITCH_FADER_BYTE];
            match pitch_center {
                None => {
                    // First report: latch center, set base_speed = 1.0.
                    pitch_center = Some(raw);
                    log::info!("S2 pitch fader: center calibrated at 0x{raw:02X}");
                    base_speed = 1.0;
                    speed.store(1.0f32.to_bits(), Ordering::Relaxed);
                }
                Some(center) => {
                    // Only update if moved by >1 count; filters single-count ADC
                    // jitter which would otherwise corrupt base_speed and cause
                    // snap-back to land at a slightly wrong value.
                    let prev = prev_btns[PITCH_FADER_BYTE];
                    if (raw as i32 - prev as i32).abs() > 1 {
                        let offset     = center as f32 - raw as f32;
                        let half_range = center.max(1) as f32;
                        let new_speed  = (1.0 + offset / half_range * PITCH_FADER_RANGE).clamp(0.25, 4.0);
                        base_speed = new_speed;
                        speed.store(new_speed.to_bits(), Ordering::Relaxed);
                        log::debug!("S2 pitch fader: 0x{raw:02X} (center 0x{center:02X}) → {new_speed:.3}×");
                    }
                }
            }
        }

        // ── Buttons ───────────────────────────────────────────────────────────
        btn_edge(report, &prev_btns, MAP_PLAY, || {
            let was = playing.load(Ordering::Relaxed);
            playing.store(!was, Ordering::Relaxed);
            log::info!("S2 → {}", if was { "paused" } else { "playing" });
        });

        btn_edge(report, &prev_btns, MAP_CUE, || {
            position.store(0, Ordering::Relaxed);
            log::info!("S2 → cue");
        });

        btn_edge(report, &prev_btns, MAP_SEEK_FWD, || {
            let pos = position.load(Ordering::Relaxed);
            position.store(pos.saturating_add(seek_delta).min(max_pos), Ordering::Relaxed);
        });

        btn_edge(report, &prev_btns, MAP_SEEK_REV, || {
            let pos = position.load(Ordering::Relaxed);
            position.store(pos.saturating_sub(seek_delta), Ordering::Relaxed);
        });

        prev_btns[..n].copy_from_slice(report);
    }
}

/// Fires `f` on the rising edge of a button bit (0→1 transition only).
fn btn_edge(report: &[u8], prev: &[u8], map: (usize, u8), f: impl FnOnce()) {
    let (byte, mask) = map;
    if byte == 0xFF { return; }
    if byte >= report.len() || byte >= prev.len() { return; }
    let now_set  = (report[byte] & mask) != 0;
    let was_set  = (prev[byte]   & mask) != 0;
    if now_set && !was_set {
        f();
    }
}
