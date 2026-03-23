//! Raw FFI bindings for rubberband-c.h (Rubber Band Library 3.x).
//!
//! Only the subset of the API that we actually use is bound here.
//! See /usr/include/rubberband/rubberband-c.h for the full API.

use std::os::raw::{c_double, c_float, c_int, c_uint};

// Opaque handle.
#[repr(C)]
pub struct RubberBandState_ {
    _private: [u8; 0],
}
pub type RubberBandState = *mut RubberBandState_;

// ── Option flags (subset) ───────────────────────────────────────────────────

pub const OPTION_PROCESS_REAL_TIME:       c_int = 0x00000001;
pub const OPTION_TRANSIENTS_CRISP:        c_int = 0x00000000;
pub const OPTION_PHASE_LAMINAR:           c_int = 0x00000000;
pub const OPTION_THREADING_NEVER:         c_int = 0x00010000;
pub const OPTION_PITCH_HIGH_CONSISTENCY:  c_int = 0x04000000;
pub const OPTION_CHANNELS_TOGETHER:       c_int = 0x10000000;
pub const OPTION_ENGINE_FINER:            c_int = 0x20000000; // R3 engine

/// Default high-quality real-time options for key-lock use.
pub const REALTIME_R3_OPTIONS: c_int =
    OPTION_PROCESS_REAL_TIME
    | OPTION_TRANSIENTS_CRISP
    | OPTION_PHASE_LAMINAR
    | OPTION_THREADING_NEVER
    | OPTION_PITCH_HIGH_CONSISTENCY
    | OPTION_CHANNELS_TOGETHER
    | OPTION_ENGINE_FINER;

// ── Extern declarations ─────────────────────────────────────────────────────

#[link(name = "rubberband")]
extern "C" {
    pub fn rubberband_new(
        sample_rate:       c_uint,
        channels:          c_uint,
        options:           c_int,
        initial_time_ratio: c_double,
        initial_pitch_scale: c_double,
    ) -> RubberBandState;

    pub fn rubberband_delete(state: RubberBandState);

    pub fn rubberband_reset(state: RubberBandState);

    pub fn rubberband_set_time_ratio(state: RubberBandState, ratio: c_double);
    pub fn rubberband_set_pitch_scale(state: RubberBandState, scale: c_double);

    pub fn rubberband_get_samples_required(state: RubberBandState) -> c_uint;

    pub fn rubberband_process(
        state:   RubberBandState,
        input:   *const *const c_float,
        samples: c_uint,
        r#final: c_int,
    );

    pub fn rubberband_available(state: RubberBandState) -> c_int;

    pub fn rubberband_retrieve(
        state:   RubberBandState,
        output:  *const *mut c_float,
        samples: c_uint,
    ) -> c_uint;

    pub fn rubberband_get_latency(state: RubberBandState) -> c_uint;

    pub fn rubberband_set_max_process_size(state: RubberBandState, samples: c_uint);
}
