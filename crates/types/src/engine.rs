use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Published by the audio engine at ~100 Hz.
/// Read by the UI at 60 fps and by the inter-deck protocol bridge.
/// Each field is an independent atomic — no snapshot consistency guarantee,
/// but each individual value is always coherent.
#[derive(Default)]
pub struct EngineState {
    /// Current playback position in samples.
    pub position: AtomicU64,

    /// Slip ghost position in samples (valid only when slip_active is true).
    pub ghost_position: AtomicU64,

    /// Playback speed × 100_000  (100_000 = 1.0×, 80_000 = 0.8×, etc.)
    pub speed_fixed: AtomicI32,

    /// BPM × 100  (12_800 = 128.00 BPM)
    pub bpm_fixed: AtomicU32,

    /// Beat phase 0–65535  (maps to 0.0–1.0 within the current beat)
    pub beat_phase: AtomicU16,

    /// Bar phase 0–65535  (maps to 0.0–1.0 across 4 beats)
    pub bar_phase: AtomicU16,

    pub is_playing: AtomicBool,
    pub slip_active: AtomicBool,
    pub key_lock: AtomicBool,

    /// Deck identifier 0–3.
    pub deck_id: AtomicU32,

    /// Wall-clock timestamp (nanoseconds, monotonic) of this update.
    pub timestamp_ns: AtomicU64,
}

impl EngineState {
    pub fn new(deck_id: u8) -> Arc<Self> {
        let s = Arc::new(Self::default());
        s.deck_id.store(deck_id as u32, Ordering::Relaxed);
        s
    }

    /// Snapshot for the UI — reads each field once with Relaxed ordering.
    /// Values are individually coherent but not mutually consistent as a group;
    /// this is acceptable for a 60fps display.
    pub fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            position:     self.position.load(Ordering::Relaxed),
            ghost_position: self.ghost_position.load(Ordering::Relaxed),
            speed:        self.speed_fixed.load(Ordering::Relaxed) as f32 / 100_000.0,
            bpm:          self.bpm_fixed.load(Ordering::Relaxed) as f32 / 100.0,
            beat_phase:   self.beat_phase.load(Ordering::Relaxed) as f32 / 65535.0,
            bar_phase:    self.bar_phase.load(Ordering::Relaxed) as f32 / 65535.0,
            is_playing:   self.is_playing.load(Ordering::Relaxed),
            slip_active:  self.slip_active.load(Ordering::Relaxed),
            key_lock:     self.key_lock.load(Ordering::Relaxed),
            deck_id:      self.deck_id.load(Ordering::Relaxed) as u8,
            timestamp_ns: self.timestamp_ns.load(Ordering::Relaxed),
        }
    }
}

/// Non-atomic copy of EngineState for use in UI / protocol bridge.
#[derive(Debug, Clone, Copy)]
pub struct EngineSnapshot {
    pub position:      u64,
    pub ghost_position: u64,
    pub speed:         f32,
    pub bpm:           f32,
    pub beat_phase:    f32,  // 0.0–1.0
    pub bar_phase:     f32,  // 0.0–1.0
    pub is_playing:    bool,
    pub slip_active:   bool,
    pub key_lock:      bool,
    pub deck_id:       u8,
    pub timestamp_ns:  u64,
}
