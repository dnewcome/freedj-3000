//! ProDJ Link — Pioneer CDJ/XDJ network sync protocol.
//!
//! Protocol documentation: https://djl-analysis.deepsymmetry.org/
//! Reference implementation: beat-link (Java) by Deep Symmetry
//!
//! We appear on the network as player number 1–4.
//! Pioneer mixers (DJM-900NXS2 etc.) and CDJs see us as a peer.

use opendeck_types::EngineSnapshot;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// UDP ports used by ProDJ Link.
pub const PORT_ANNOUNCE:  u16 = 50000;
pub const PORT_STATUS:    u16 = 50002;

/// Packet type bytes.
pub const PKT_ANNOUNCE:   u8 = 0x06;
pub const PKT_BEAT:       u8 = 0x28;
pub const PKT_STATUS:     u8 = 0x0A;

/// Magic header present in all ProDJ Link packets.
pub const MAGIC: &[u8] = b"Qspt";

pub struct ProDjLink {
    player_num: u8,
    device_name: [u8; 20],
}

impl ProDjLink {
    pub fn new(player_num: u8) -> Self {
        let mut device_name = [0u8; 20];
        let name = b"OpenDeck";
        device_name[..name.len()].copy_from_slice(name);
        Self { player_num, device_name }
    }

    /// Build a device announce packet (0x06).
    /// Broadcast on PORT_ANNOUNCE every 1.5 seconds.
    pub fn build_announce(&self, ip: Ipv4Addr, mac: [u8; 6]) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(54);
        pkt.extend_from_slice(MAGIC);
        pkt.push(0x10);         // sub-type
        pkt.push(PKT_ANNOUNCE);
        pkt.extend_from_slice(&self.device_name);
        pkt.push(0x01);
        pkt.push(0x36);         // packet length
        pkt.extend_from_slice(&ip.octets());
        pkt.extend_from_slice(&mac);
        pkt.push(self.player_num);
        pkt.push(0x01);         // device type (CDJ)
        pkt
    }

    /// Build a beat announcement packet (0x28).
    /// Sent at every beat onset, derived from our EngineState.
    pub fn build_beat(&self, snap: &EngineSnapshot) -> Vec<u8> {
        let bpm_raw = (snap.bpm * 100.0) as u32;
        let mut pkt = Vec::with_capacity(0x60);
        pkt.extend_from_slice(MAGIC);
        pkt.push(0x10);
        pkt.push(PKT_BEAT);
        pkt.extend_from_slice(&self.device_name);
        pkt.push(self.player_num);
        pkt.push(0x60);         // length
        // Next beat number, 2nd beat, next-bar beat (placeholders)
        pkt.extend_from_slice(&1u32.to_be_bytes());
        pkt.extend_from_slice(&2u32.to_be_bytes());
        pkt.extend_from_slice(&1u32.to_be_bytes());
        // Beat within bar 1–4
        let beat_in_bar = ((snap.bar_phase * 4.0) as u32 % 4) + 1;
        pkt.extend_from_slice(&beat_in_bar.to_be_bytes());
        pkt.extend_from_slice(&bpm_raw.to_be_bytes());
        pkt
    }

    /// Parse an incoming packet and return the peer's EngineSnapshot if it's
    /// a beat or status packet we care about.
    pub fn parse_packet(data: &[u8]) -> Option<(u8, EngineSnapshot)> {
        if data.len() < 5 || &data[0..4] != MAGIC {
            return None;
        }
        let pkt_type = data[5];
        match pkt_type {
            PKT_BEAT => parse_beat_packet(data),
            PKT_STATUS => parse_status_packet(data),
            _ => None,
        }
    }
}

fn parse_beat_packet(data: &[u8]) -> Option<(u8, EngineSnapshot)> {
    if data.len() < 0x28 + 4 { return None; }
    let player_num = data[0x10];
    let bpm_raw = u32::from_be_bytes(data[0x24..0x28].try_into().ok()?) as f32 / 100.0;
    // TODO: extract full fields per dysentery spec
    Some((player_num, EngineSnapshot {
        position: 0,
        ghost_position: 0,
        speed: 1.0,
        bpm: bpm_raw,
        beat_phase: 0.0,
        bar_phase: 0.0,
        is_playing: true,
        slip_active: false,
        key_lock: false,
        deck_id: player_num,
        timestamp_ns: 0,
    }))
}

fn parse_status_packet(data: &[u8]) -> Option<(u8, EngineSnapshot)> {
    // TODO: implement per dysentery protocol spec chapter 5
    let _ = data;
    None
}
