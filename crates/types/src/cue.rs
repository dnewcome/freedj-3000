use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const RED:    Self = Self { r: 255, g: 0,   b: 0   };
    pub const GREEN:  Self = Self { r: 0,   g: 255, b: 0   };
    pub const BLUE:   Self = Self { r: 0,   g: 0,   b: 255 };
    pub const YELLOW: Self = Self { r: 255, g: 220, b: 0   };
    pub const CYAN:   Self = Self { r: 0,   g: 220, b: 255 };
    pub const ORANGE: Self = Self { r: 255, g: 120, b: 0   };
    pub const PINK:   Self = Self { r: 255, g: 0,   b: 180 };
    pub const WHITE:  Self = Self { r: 255, g: 255, b: 255 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CueKind {
    HotCue,
    LoopIn,
    LoopOut,
    FadeIn,
    FadeOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuePoint {
    /// Slot index 0–7.
    pub slot: u8,
    /// Position in samples from track start.
    pub position: u64,
    pub color: Rgb,
    pub label: String,
    pub kind: CueKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedLoop {
    pub slot: u8,
    pub in_pt: u64,
    pub out_pt: u64,
    pub label: String,
}

/// All cue data for one track, ready to use during playback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CueMap {
    pub hot_cues: [Option<CuePoint>; 8],
    pub loops: Vec<SavedLoop>,
}
