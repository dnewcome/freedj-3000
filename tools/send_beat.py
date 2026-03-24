#!/usr/bin/env python3
"""Send fake ProDJ Link beat packets to the opendeck app for testing.

Sends one packet per beat at the given BPM, using an ephemeral source port
so it doesn't conflict with anything bound to port 50002.

Usage:  python3 send_beat.py [bpm] [host] [port]
  bpm   — beats per minute (default 123.0)
  host  — destination host (default 127.0.0.1)
  port  — destination port (default 50052, matches LISTEN_PORT in prodj.rs)
"""

import socket
import struct
import sys
import time

MAGIC    = b"Qspt"
PKT_BEAT = 0x28

def make_beat_packet(bpm: float, player: int = 1) -> bytes:
    """Build a minimal beat packet matching the offsets in parse_beat_packet:
      byte 0x10        — player number
      bytes 0x24-0x28  — BPM * 100 as u32 big-endian
    """
    pkt = bytearray(0x30)          # 48 bytes, all zeros
    pkt[0:4]    = MAGIC
    pkt[4]      = 0x10             # sub-type
    pkt[5]      = PKT_BEAT
    pkt[0x10]   = player           # player number
    bpm_raw     = int(bpm * 100)
    struct.pack_into(">I", pkt, 0x24, bpm_raw)
    return bytes(pkt)

def main():
    bpm    = float(sys.argv[1]) if len(sys.argv) > 1 else 123.0
    host   = sys.argv[2]         if len(sys.argv) > 2 else "127.0.0.1"
    port   = int(sys.argv[3])    if len(sys.argv) > 3 else 50052

    beat_s = 60.0 / bpm
    pkt    = make_beat_packet(bpm)

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    print(f"Sending {bpm} BPM beat packets to {host}:{port}  (Ctrl-C to stop)")
    print(f"Packet ({len(pkt)} bytes): {pkt.hex()}")

    try:
        next_beat = time.monotonic()
        while True:
            sock.sendto(pkt, (host, port))
            next_beat += beat_s
            delay = next_beat - time.monotonic()
            if delay > 0:
                time.sleep(delay)
    except KeyboardInterrupt:
        print("\nStopped.")

if __name__ == "__main__":
    main()
