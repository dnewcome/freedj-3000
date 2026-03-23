/// Output from a DVS timecode decoder each audio block.
#[derive(Debug, Clone, Copy)]
pub struct TimecodeOutput {
    /// Normalised playback speed.
    /// 1.0 = forward at reference speed, -1.0 = reverse, 0.0 = stationary.
    pub speed: f32,

    /// Absolute position within the timecode signal (seconds from start),
    /// if the format encodes absolute position. None for relative-only signals
    /// or when signal quality is below threshold.
    pub position: Option<f64>,

    /// Signal quality 0.0–1.0. Ignore output below ~0.3.
    pub confidence: f32,

    pub direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
    Stationary,
}

/// Which timecode format/vinyl pressing to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimecodeFormat {
    SeratoCv025,        // current Serato standard, 2500 Hz carrier
    SeratoLegacy,       // older Serato pressings, 1000 Hz carrier
    TraktorMk2,         // Traktor Scratch MK2, 2000 Hz carrier
    Mixvibes,           // Mixvibes DVS
    PioneerRekordbox,   // rekordbox DVS (RB-VS1-K)
}
