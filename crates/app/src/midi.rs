//! Numark DJ2Go MIDI input.
//!
//! The DJ2Go is a class-compliant USB MIDI device.  It sends:
//!   • Note On/Off    (0x9n / 0x8n)  for buttons
//!   • Control Change (0xBn)          for the jog wheel (relative encoder)
//!   • Pitch Bend     (0xEn)          for the pitch fader (14-bit)
//!
//! Run with RUST_LOG=opendeck::midi=debug to see every incoming MIDI message.
//! Check the byte values to verify or correct the constants below.

use midir::MidiInput;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const DEVICE_NAME: &str = "DJ2Go";

// ── Button mappings — (channel 0-indexed, note) ──────────────────────────────
const MAP_PLAY:       (u8, u8) = (0, 0x33);
const MAP_CUE:        (u8, u8) = (0, 0x3B);
// Pitch ±1% increment buttons (the small buttons next to the pitch slider).
const MAP_PITCH_UP:   (u8, u8) = (0, 0x43);
const MAP_PITCH_DOWN: (u8, u8) = (0, 0x44);

// ── Jog wheel — relative Control Change ──────────────────────────────────────
// Values 1–63 = clockwise (+), 65–127 = counter-clockwise (−).
const JOG_CC:      u8 = 0x19; // 25
const JOG_CHANNEL: u8 = 0;

// ── Pitch slider — absolute Control Change ────────────────────────────────────
// CC 13, 0–127, center = 64.  Lower value = fader up = faster.
const PITCH_CC:      u8 = 0x0D; // 13
const PITCH_CHANNEL: u8 = 0;
const PITCH_CENTER:  u8 = 64;
/// ±% pitch range — 0.16 = ±16%.
const PITCH_FADER_RANGE: f32 = 0.16;

// ── Deck B — second beat grid (channel 1, same CC/note numbers) ──────────────
const JOG_CHANNEL_B:   u8 = 1;
const PITCH_CHANNEL_B: u8 = 1;
const MAP_CUE_B:       (u8, u8) = (1, 0x3B); // sets beat2 anchor to current pos

// ── Jog sensitivity ───────────────────────────────────────────────────────────
/// Scrub (scratch on): samples per jog tick.  Tune to taste.
#[allow(dead_code)]
const SCRUB_SAMPLES_PER_TICK: f64 = 1000.0;
/// Nudge speed offset per jog tick (Deck A).
const NUDGE_SPEED_PER_TICK: f32 = 0.002;
/// Nudge BPM offset per jog tick (Deck B).
const NUDGE_BPM_PER_TICK: f32 = 0.1;
/// Idle time before speed/BPM snaps back to the pitch fader value.
const NUDGE_RELEASE_MS: u64 = 150;

// ─────────────────────────────────────────────────────────────────────────────

struct State {
    // Deck A (audio)
    fader_speed: f32,
    last_nudge:  Option<Instant>,
    // Deck B (second beat grid)
    bpm2_fader: f32,
    bpm2_nudge: Option<Instant>,
}

pub struct MidiHandle {
    _conn:   midir::MidiInputConnection<()>,
    _thread: thread::JoinHandle<()>,
}

impl MidiHandle {
    pub fn connect(
        playing:      Arc<AtomicBool>,
        position:     Arc<AtomicU64>,
        speed:        Arc<AtomicU32>,
        fader_speed:  Arc<AtomicU32>,
        _sample_rate: u32,
        channels:     u8,
        samples_len:  usize,
        beat2_bpm:    Arc<AtomicU32>,
        beat2_anchor: Arc<AtomicU64>,
        base_bpm:     f32,
    ) -> Option<Self> {
        let midi_in = MidiInput::new("opendeck")
            .map_err(|e| log::warn!("MIDI: init failed: {e}"))
            .ok()?;

        let ports = midi_in.ports();
        let port = ports.iter().find(|p| {
            midi_in.port_name(p)
                .map(|n| n.contains(DEVICE_NAME))
                .unwrap_or(false)
        });

        let port = match port {
            Some(p) => p.clone(),
            None => {
                log::info!("MIDI: {} not found.  Available ports:", DEVICE_NAME);
                for p in &ports {
                    if let Ok(name) = midi_in.port_name(p) {
                        log::info!("  - {name}");
                    }
                }
                return None;
            }
        };

        log::info!("MIDI: found {} — connecting", DEVICE_NAME);

        let max_pos = samples_len as u64;

        let state = Arc::new(Mutex::new(State {
            fader_speed: 1.0,
            last_nudge:  None,
            bpm2_fader:  base_bpm,
            bpm2_nudge:  None,
        }));

        // Clone refs for the MIDI callback.
        let (playing_cb, position_cb, speed_cb, fader_speed_cb, state_cb) = (
            Arc::clone(&playing),
            Arc::clone(&position),
            Arc::clone(&speed),
            Arc::clone(&fader_speed),
            Arc::clone(&state),
        );
        let (beat2_bpm_cb, beat2_anchor_cb) = (
            Arc::clone(&beat2_bpm),
            Arc::clone(&beat2_anchor),
        );

        let conn = midi_in
            .connect(
                &port,
                "opendeck-dj2go",
                move |_ts, msg, _| {
                    handle_message(
                        msg,
                        &playing_cb,
                        &position_cb,
                        &speed_cb,
                        &fader_speed_cb,
                        &beat2_bpm_cb,
                        &beat2_anchor_cb,
                        &state_cb,
                        max_pos,
                        channels,
                        base_bpm,
                    );
                },
                (),
            )
            .map_err(|e| log::error!("MIDI: connect failed: {e}"))
            .ok()?;

        // Snap-back thread: polls every 20 ms and restores fader values after
        // the jog has been idle for NUDGE_RELEASE_MS.
        let (speed_snap, beat2_bpm_snap, state_snap) = (
            Arc::clone(&speed),
            Arc::clone(&beat2_bpm),
            Arc::clone(&state),
        );
        let snap_thread = thread::Builder::new()
            .name("dj2go-snap".into())
            .spawn(move || loop {
                thread::sleep(Duration::from_millis(20));
                let mut st = state_snap.lock().unwrap();
                if let Some(t) = st.last_nudge {
                    if t.elapsed() > Duration::from_millis(NUDGE_RELEASE_MS) {
                        speed_snap.store(st.fader_speed.to_bits(), Ordering::Relaxed);
                        st.last_nudge = None;
                    }
                }
                if let Some(t) = st.bpm2_nudge {
                    if t.elapsed() > Duration::from_millis(NUDGE_RELEASE_MS) {
                        beat2_bpm_snap.store(st.bpm2_fader.to_bits(), Ordering::Relaxed);
                        st.bpm2_nudge = None;
                    }
                }
            })
            .expect("failed to spawn snap-back thread");

        Some(MidiHandle { _conn: conn, _thread: snap_thread })
    }
}

// ─────────────────────────────────────────────────────────────────────────────

fn handle_message(
    msg:          &[u8],
    playing:      &Arc<AtomicBool>,
    position:     &Arc<AtomicU64>,
    speed:        &Arc<AtomicU32>,
    fader_speed:  &Arc<AtomicU32>,
    beat2_bpm:    &Arc<AtomicU32>,
    beat2_anchor: &Arc<AtomicU64>,
    state:        &Arc<Mutex<State>>,
    max_pos:      u64,
    channels:     u8,
    base_bpm:     f32,
) {
    if msg.is_empty() { return; }

    let status  = msg[0];
    let kind    = status & 0xF0;
    let channel = status & 0x0F;

    log::debug!("MIDI rx: {:02X?}", msg);

    match kind {
        0x90 if msg.len() >= 3 => {
            let note = msg[1];
            let vel  = msg[2];
            // Note On with velocity 0 is treated as Note Off.
            if vel > 0 {
                note_on(channel, note, playing, position, speed, fader_speed, beat2_anchor, state, channels);
            }
        }

        0xB0 if msg.len() >= 3 => {
            let cc    = msg[1];
            let value = msg[2];

            // Deck A — audio playback
            if channel == JOG_CHANNEL && cc == JOG_CC {
                jog_tick(value, position, speed, state, max_pos);
            } else if channel == PITCH_CHANNEL && cc == PITCH_CC {
                let s = cc_to_speed(value);
                let mut st = state.lock().unwrap();
                st.fader_speed = s;
                fader_speed.store(s.to_bits(), Ordering::Relaxed);
                if st.last_nudge.is_none() {
                    speed.store(s.to_bits(), Ordering::Relaxed);
                }
                log::debug!("MIDI pitch slider A: {value} → {s:.3}×");

            // Deck B — second beat grid
            } else if channel == JOG_CHANNEL_B && cc == JOG_CC {
                jog_tick_b(value, beat2_bpm, state);
            } else if channel == PITCH_CHANNEL_B && cc == PITCH_CC {
                let bpm = cc_to_bpm(value, base_bpm);
                let mut st = state.lock().unwrap();
                st.bpm2_fader = bpm;
                if st.bpm2_nudge.is_none() {
                    beat2_bpm.store(bpm.to_bits(), Ordering::Relaxed);
                }
                log::debug!("MIDI pitch slider B: {value} → {bpm:.1} BPM");
            }
        }

        _ => {}
    }
}

fn note_on(
    channel:      u8,
    note:         u8,
    playing:      &Arc<AtomicBool>,
    position:     &Arc<AtomicU64>,
    speed:        &Arc<AtomicU32>,
    fader_speed:  &Arc<AtomicU32>,
    beat2_anchor: &Arc<AtomicU64>,
    state:        &Arc<Mutex<State>>,
    channels:     u8,
) {
    let (play_ch,  play_note)  = MAP_PLAY;
    let (cue_ch,   cue_note)   = MAP_CUE;
    let (pup_ch,   pup_note)   = MAP_PITCH_UP;
    let (pdown_ch, pdown_note) = MAP_PITCH_DOWN;

    if channel == play_ch && note == play_note {
        let was = playing.load(Ordering::Relaxed);
        playing.store(!was, Ordering::Relaxed);
        log::info!("DJ2Go → {}", if was { "paused" } else { "playing" });
    } else if channel == cue_ch && note == cue_note {
        position.store(0, Ordering::Relaxed);
        log::info!("DJ2Go → cue");
    } else if channel == pup_ch && note == pup_note {
        // Update fader_speed so snap-back doesn't undo the change.
        let mut st = state.lock().unwrap();
        let new = (st.fader_speed + 0.01).clamp(0.25, 4.0);
        st.fader_speed = new;
        fader_speed.store(new.to_bits(), Ordering::Relaxed);
        speed.store(new.to_bits(), Ordering::Relaxed);
        log::debug!("DJ2Go → pitch +1% ({new:.3}×)");
    } else if channel == pdown_ch && note == pdown_note {
        let mut st = state.lock().unwrap();
        let new = (st.fader_speed - 0.01).clamp(0.25, 4.0);
        st.fader_speed = new;
        fader_speed.store(new.to_bits(), Ordering::Relaxed);
        speed.store(new.to_bits(), Ordering::Relaxed);
        log::debug!("DJ2Go → pitch −1% ({new:.3}×)");
    } else {
        let (cue_b_ch, cue_b_note) = MAP_CUE_B;
        if channel == cue_b_ch && note == cue_b_note {
            // Set beat2 anchor to current playhead (in frames).
            let pos_frames = position.load(Ordering::Relaxed) / channels as u64;
            beat2_anchor.store(pos_frames, Ordering::Relaxed);
            log::info!("DJ2Go Deck B → beat2 anchor set at frame {pos_frames}");
        } else {
            log::debug!("MIDI note on ch{channel} note 0x{note:02X} (unmapped)");
        }
    }
}

fn jog_tick(
    value:    u8,
    position: &Arc<AtomicU64>,
    speed:    &Arc<AtomicU32>,
    state:    &Arc<Mutex<State>>,
    max_pos:  u64,
) {
    // Two's complement relative: 1–63 = CW (+), 65–127 = CCW (−).
    let delta: i32 = if value < 64 { value as i32 } else { value as i32 - 128 };
    log::debug!("DJ2Go jog: {delta:+}");

    let mut st = state.lock().unwrap();
    // Nudge: accumulate speed offset from the current value so rapid spinning
    // keeps growing, then snap-back restores fader_speed after idle.
    let cur    = f32::from_bits(speed.load(Ordering::Relaxed));
    let offset = delta as f32 * NUDGE_SPEED_PER_TICK;
    speed.store((cur + offset).clamp(0.25, 4.0).to_bits(), Ordering::Relaxed);
    st.last_nudge = Some(Instant::now());

    // If you want scrub (direct seek) instead of nudge, swap in this block:
    //   let samples = (delta.abs() as f64 * SCRUB_SAMPLES_PER_TICK) as u64;
    //   let pos = position.load(Ordering::Relaxed);
    //   if delta > 0 { position.store(pos.saturating_add(samples).min(max_pos), ...) }
    //   else         { position.store(pos.saturating_sub(samples), ...) }
    let _ = (position, max_pos);
}

/// Deck B jog — nudges beat2_bpm by a small amount per tick.
fn jog_tick_b(value: u8, beat2_bpm: &Arc<AtomicU32>, state: &Arc<Mutex<State>>) {
    let delta: i32 = if value < 64 { value as i32 } else { value as i32 - 128 };
    log::debug!("DJ2Go Deck B jog: {delta:+}");

    let mut st  = state.lock().unwrap();
    let cur_bpm = f32::from_bits(beat2_bpm.load(Ordering::Relaxed));
    let new_bpm = (cur_bpm + delta as f32 * NUDGE_BPM_PER_TICK).clamp(20.0, 300.0);
    beat2_bpm.store(new_bpm.to_bits(), Ordering::Relaxed);
    st.bpm2_nudge = Some(Instant::now());
}

/// CC pitch slider → speed multiplier (Deck A).
/// value = 0..=127, center = 64.  Lower value = fader up = faster.
fn cc_to_speed(value: u8) -> f32 {
    let offset = PITCH_CENTER as f32 - value as f32;
    (1.0 + offset / PITCH_CENTER as f32 * PITCH_FADER_RANGE).clamp(0.25, 4.0)
}

/// CC pitch slider → BPM (Deck B).
/// Center (64) = base_bpm, range ±PITCH_FADER_RANGE %.
fn cc_to_bpm(value: u8, base_bpm: f32) -> f32 {
    let offset = PITCH_CENTER as f32 - value as f32;
    let factor = 1.0 + offset / PITCH_CENTER as f32 * PITCH_FADER_RANGE;
    (base_bpm * factor).clamp(20.0, 300.0)
}
