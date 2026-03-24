//! ProDJ Link beat packet listener.
//!
//! Binds UDP on port 50002 and waits for Pioneer beat packets (0x28).
//! On each beat, updates beat2_bpm and bumps beat2_anchor (which triggers
//! a phase reset in the renderer, keeping the second beat grid locked to
//! the incoming beat).
//!
//! Works with real Pioneer CDJs/XDJs on the same LAN, or any tool that
//! sends ProDJ Link beat packets (e.g. dysentery, rekordbox in link mode).
//!
//! Run with RUST_LOG=opendeck::prodj=debug for per-packet logging.

use opendeck_link::prodj::{ProDjLink, PORT_ANNOUNCE, PORT_STATUS};

// When testing on a single machine alongside prolink_virtual_cdj, set this to
// a forwarding port (e.g. 50052) and run:
//   socat UDP4-RECV:50002,reuseport,fork UDP4-SENDTO:127.0.0.1:50052
// For real hardware (CDJ on a separate machine) leave as PORT_STATUS (50002).
const LISTEN_PORT: u16 = PORT_STATUS;
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

pub struct ProDjHandle {
    _thread: thread::JoinHandle<()>,
}

impl ProDjHandle {
    /// Spawn a background thread that listens for ProDJ Link beat packets.
    /// Returns `None` if the socket cannot be bound (e.g. port already in use).
    pub fn listen(
        beat2_bpm:    Arc<AtomicU32>,
        beat2_anchor: Arc<AtomicU64>,
    ) -> Option<Self> {
        // Use socket2 to set SO_REUSEADDR + SO_REUSEPORT before binding so we
        // can share port 50002 with other ProDJ Link tools (e.g. prolink_virtual_cdj).
        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| log::warn!("ProDJ Link: socket create failed: {e}"))
            .ok()?;
        raw.set_reuse_address(true).ok();
        #[cfg(unix)]
        raw.set_reuse_port(true).ok();
        raw.set_broadcast(true).ok();
        let addr: SocketAddr = format!("0.0.0.0:{LISTEN_PORT}").parse().unwrap();
        raw.bind(&addr.into())
            .map_err(|e| log::warn!("ProDJ Link: cannot bind port {LISTEN_PORT}: {e}"))
            .ok()?;
        let sock: UdpSocket = raw.into();
        sock.set_read_timeout(Some(Duration::from_millis(500))).ok();

        log::info!("ProDJ Link: listening for beat packets on port {LISTEN_PORT}");

        // Also sniff the announce port so we can see what the virtual CDJ sends.
        spawn_sniffer(PORT_ANNOUNCE);

        let t = thread::Builder::new()
            .name("prodj-rx".into())
            .spawn(move || {
                let mut buf = [0u8; 1500];
                loop {
                    match sock.recv_from(&mut buf) {
                        Ok((n, addr)) => {
                            log::info!(
                                "ProDJ rx: {} bytes from {} — {:02X?}",
                                n, addr, &buf[..n.min(32)]
                            );
                            if let Some((player, snap)) = ProDjLink::parse_packet(&buf[..n]) {
                                let old_bpm = f32::from_bits(beat2_bpm.load(Ordering::Relaxed));
                                beat2_bpm.store(snap.bpm.to_bits(), Ordering::Relaxed);
                                beat2_anchor.fetch_add(1, Ordering::Relaxed);
                                log::info!(
                                    "ProDJ beat: player {} @ {:.2} BPM (was {:.2}) from {}",
                                    player, snap.bpm, old_bpm, addr
                                );
                            } else {
                                log::info!("ProDJ rx: packet not recognized (full: {:02X?})", &buf[..n]);
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => log::error!("ProDJ Link recv: {e}"),
                    }
                }
            })
            .expect("failed to spawn ProDJ listener thread");

        Some(ProDjHandle { _thread: t })
    }
}

/// Spawn a read-only sniffer on `port` that logs every packet received.
/// Used to verify that the virtual CDJ is actually sending traffic.
fn spawn_sniffer(port: u16) {
    let raw = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
        Ok(s) => s,
        Err(e) => { log::warn!("ProDJ sniffer: socket failed: {e}"); return; }
    };
    raw.set_reuse_address(true).ok();
    #[cfg(unix)]
    raw.set_reuse_port(true).ok();
    raw.set_broadcast(true).ok();
    let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
    if let Err(e) = raw.bind(&addr.into()) {
        log::warn!("ProDJ sniffer: cannot bind port {port}: {e}");
        return;
    }
    let sock: UdpSocket = raw.into();
    sock.set_read_timeout(Some(Duration::from_millis(500))).ok();

    log::info!("ProDJ sniffer: listening on port {port}");

    thread::Builder::new()
        .name(format!("prodj-sniff-{port}"))
        .spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                match sock.recv_from(&mut buf) {
                    Ok((n, addr)) => {
                        log::info!(
                            "ProDJ port {port} rx: {n} bytes from {addr} — {:02X?}",
                            &buf[..n.min(48)]
                        );
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => log::error!("ProDJ sniffer port {port}: {e}"),
                }
            }
        })
        .ok();
}
