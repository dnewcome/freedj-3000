//! MCU ↔ host protocol.
//!
//! Designed to be `no_std` compatible so this crate can be shared with the
//! RP2350 MCU firmware (embassy-rs) once that work begins.
//!
//! Wire format: 10-byte little-endian packets over SPI or USB HID.
//!
//!   [0]    PacketType (u8)
//!   [1-4]  timestamp_us — microseconds since MCU boot (u32 LE)
//!   [5-8]  value — i32 LE  (delta, bitfield, or fixed-point position)
//!   [9]    checksum — XOR of bytes 0–8
//!
//! The host sends LED update packets back to the MCU in the same format.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

/// Wire packet: 10 bytes, little-endian.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McuPacket {
    pub kind:         u8,
    pub timestamp_us: u32,
    pub value:        i32,
    pub checksum:     u8,
}

impl McuPacket {
    pub const SIZE: usize = 10;

    pub fn new(kind: PacketKind, timestamp_us: u32, value: i32) -> Self {
        let bytes = [
            kind as u8,
            (timestamp_us & 0xFF) as u8,
            ((timestamp_us >> 8) & 0xFF) as u8,
            ((timestamp_us >> 16) & 0xFF) as u8,
            ((timestamp_us >> 24) & 0xFF) as u8,
            (value & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 24) & 0xFF) as u8,
        ];
        let checksum = bytes.iter().fold(0u8, |acc, &b| acc ^ b);
        Self { kind: kind as u8, timestamp_us, value, checksum }
    }

    pub fn verify(&self) -> bool {
        let bytes: [u8; 9] = [
            self.kind,
            (self.timestamp_us & 0xFF) as u8,
            ((self.timestamp_us >> 8) & 0xFF) as u8,
            ((self.timestamp_us >> 16) & 0xFF) as u8,
            ((self.timestamp_us >> 24) & 0xFF) as u8,
            (self.value & 0xFF) as u8,
            ((self.value >> 8) & 0xFF) as u8,
            ((self.value >> 16) & 0xFF) as u8,
            ((self.value >> 24) & 0xFF) as u8,
        ];
        bytes.iter().fold(0u8, |acc, &b| acc ^ b) == self.checksum
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind {
    // MCU → host
    JogDelta    = 0x01,  // value = encoder delta (signed, counts since last packet)
    JogVelocity = 0x02,  // value = velocity in RPM × 10 (signed, negative = reverse)
    JogTouch    = 0x03,  // value = 1 (touched) or 0 (released)
    Button      = 0x04,  // value = ButtonId (low byte) | state (high byte: 1=down 0=up)
    Encoder     = 0x05,  // value = EncoderId (low byte) | delta (high 3 bytes, signed)
    StripPos    = 0x06,  // value = 0–65535 (capacitive needle-search strip position)
    Heartbeat   = 0x07,  // value = MCU uptime seconds; sent every 500ms

    // Host → MCU
    LedPad      = 0x80,  // value = slot (low byte) | R G B (bytes 1-3)
    LedRing     = 0x81,  // value = position (low byte) | R G B (bytes 1-3)
    LedIndicator= 0x82,  // value = IndicatorId (low byte) | state (high byte)
    LedFlush    = 0x83,  // value = unused; MCU updates physical LEDs after this
}

/// Button identifiers — match the physical layout on the deck.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonId {
    Play        = 0x01,
    Cue         = 0x02,
    Sync        = 0x03,
    Slip        = 0x04,
    KeyLock     = 0x05,
    Loop        = 0x06,
    LoopIn      = 0x07,
    LoopOut     = 0x08,
    Reloop      = 0x09,
    HotCue0     = 0x10,
    HotCue1     = 0x11,
    HotCue2     = 0x12,
    HotCue3     = 0x13,
    HotCue4     = 0x14,
    HotCue5     = 0x15,
    HotCue6     = 0x16,
    HotCue7     = 0x17,
    BeatLoop025 = 0x20,  // 1/4 beat
    BeatLoop050 = 0x21,
    BeatLoop1   = 0x22,
    BeatLoop2   = 0x23,
    BeatLoop4   = 0x24,
    BeatLoop8   = 0x25,
    BeatLoop16  = 0x26,
    BeatLoop32  = 0x27,
    BeatJumpL   = 0x30,
    BeatJumpR   = 0x31,
    Shift       = 0x40,
    Browse      = 0x41,
    Load        = 0x42,
    Back        = 0x43,
}

/// Encoder identifiers.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderId {
    Browse   = 0x01,  // track browser knob
    Tempo    = 0x02,  // tempo fader encoder (if fader is optical)
    PitchBend= 0x03,  // pitch bend wheel (if present)
}

/// Parsed, typed control event — produced by the host-side IPC layer from
/// raw McuPackets. This is what the audio engine's ControlEvent queue receives.
#[derive(Debug, Clone)]
pub enum ControlEvent {
    JogDelta     { delta: i32, velocity_rpm: f32 },
    JogTouch     { touched: bool },
    Play,
    Pause,
    Cue,
    HotCueTrigger { slot: u8, held: bool },
    HotCueSet     { slot: u8 },
    HotCueDelete  { slot: u8 },
    LoopIn,
    LoopOut,
    LoopToggle,
    Reloop,
    BeatLoop     { beats: f32, held: bool },
    BeatJump     { beats: f32 },
    SlipToggle,
    KeyLockToggle,
    TempoFader   { position: f32 },  // 0.0–1.0 normalised
    KeyShift     { semitones: i8 },
    NeedleSearch { position: f32 },  // 0.0–1.0 absolute track position
    BrowseEncoderDelta { delta: i32 },
    Load,
    Eject,
}
