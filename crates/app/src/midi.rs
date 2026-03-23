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
// Observed range: 0x09 (fastest / full positive) → 0x32 (slowest / full negative).
// Higher value = fader pushed down = slower speed.  Lower = fader up = faster.
// Center (0% pitch) is estimated at 0x40; adjust if the detent doesn't land on 1.0×.

const PITCH_FADER_BYTE:   usize = 7;
/// Raw byte value that corresponds to 1.0× (fader at center detent).
const PITCH_FADER_CENTER: u8   = 0x40;
/// ±% pitch range — 0.16 = ±16% (wide DJ range).  CDJ-3000 default is 0.08 (±8%).
const PITCH_FADER_RANGE:  f32  = 0.16;

// ── Jog wheel sensitivity ─────────────────────────────────────────────────────
// The 24-bit counter spans ~16.7M counts per full revolution.

/// Scrub (platter top touched): direct position control.  ~3 s per revolution.
const SCRUB_SAMPLES_PER_COUNT: f64 = 0.025;
/// Nudge (platter edge, no touch): light push, ~0.3 s per revolution.
const NUDGE_SAMPLES_PER_COUNT: f64 = 0.003;

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

    let mut prev_report = [0u8; 64];
    let mut buf         = [0u8; 64];
    let mut prev_jog:   Option<u32> = None;
    let mut prev_btns   = [0u8; 64];
    let mut touched     = false;

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

                if delta != 0 && touched {
                    // Touch active: scrub — directly moves the playhead.
                    // No touch: platter spins freely with no effect.
                    let samples = (delta.abs() as f64 * SCRUB_SAMPLES_PER_COUNT) as u64;
                    let pos = position.load(Ordering::Relaxed);
                    if delta > 0 {
                        position.store(pos.saturating_add(samples).min(max_pos), Ordering::Relaxed);
                    } else {
                        position.store(pos.saturating_sub(samples), Ordering::Relaxed);
                    }
                }
            }
            prev_jog = Some(jog);
        }

        // ── Pitch fader — byte 7, absolute ────────────────────────────────────
        if PITCH_FADER_BYTE < n && report[PITCH_FADER_BYTE] != prev_btns[PITCH_FADER_BYTE] {
            let raw    = report[PITCH_FADER_BYTE];
            // Offset from center: positive = fader up = faster.
            let offset = PITCH_FADER_CENTER as f32 - raw as f32;
            let new_speed = (1.0 + offset / PITCH_FADER_CENTER as f32 * PITCH_FADER_RANGE)
                .clamp(0.25, 4.0);
            speed.store(new_speed.to_bits(), Ordering::Relaxed);
            log::debug!("S2 pitch fader: 0x{raw:02X} → {new_speed:.3}×");
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
