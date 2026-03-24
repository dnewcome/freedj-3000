# FreeDJ-3000

An open-source, open-hardware digital media player built as a direct alternative to the Pioneer CDJ-3000. Full protocol compatibility. No licensing fees. No locked ecosystems.

> **"The CDJ-3000 costs $2,400. A Raspberry Pi 5 costs $80."**

---

## What is this?

The CDJ-3000 is the industry standard DJ media player — found in every serious club and festival worldwide. It is also a closed, proprietary device that costs thousands of dollars, requires Pioneer's ecosystem to unlock basic features, and charges recurring subscription fees for music analysis software.

FreeDJ-3000 is a complete reimplementation: same protocols, same network sync, same timecode formats — open source, open hardware, buildable for a fraction of the cost. It is not a "compatible" or "inspired by" product. It is a direct alternative designed to be interoperable with Pioneer hardware on the same network.

This project takes the position that DJ equipment protocols are infrastructure, not intellectual property. The ProDJ Link protocol, Serato/Traktor timecode formats, and rekordbox analysis data have been independently documented and implemented by the open source community for years (dysentery, xwax, beat-link). We build on that work and extend it into a complete hardware product.

---

## Status

**Early development — MVP working.** Plays audio with real-time waveform visualization. Not yet ready for live use.

| Feature | Status |
|---|---|
| MP3/FLAC/AAC/OGG decode | ✅ Working |
| Waveform visualization | ✅ Working |
| Play / pause / seek | ✅ Working |
| Variable speed / pitch | ✅ Working |
| Beat detection (MiniBPM) | ✅ Working |
| Beat grid overlay | ✅ Working |
| MIDI controller input | ✅ Working |
| ProDJ Link beat sync (receive) | ✅ Working |
| Cue points / loops | 📋 Planned |
| Key lock / timestretching | ✅ Working |
| Hardware control surface | 📋 Planned |
| rekordbox library import | 📋 Planned |

---

## Hardware targets

**Primary: Raspberry Pi 5 (8GB) + RP2350 MCU**

The Pi 5 runs the audio engine and UI. An RP2350 microcontroller (Raspberry Pi's own chip) handles the physical control surface — jog wheel, encoders, buttons — over SPI/GPIO. The RP2350 firmware will be open source and part of this repo.

The Pi 5's GPU (VideoCore VII / Mesa V3DV) supports Vulkan 1.2, which is what the renderer requires. No X server needed — wgpu runs directly on DRM/KMS.

**Fallback: Intel NUC or any x86 Linux box**

Any Linux machine with Vulkan support works for development and testing.

**Display: any HDMI screen.** The target form factor uses a high-DPI 1280×480 widescreen panel, but the UI adapts to any resolution.

---

## Building

### Dependencies

```bash
# Debian/Ubuntu
sudo apt install libasound2-dev libvulkan-dev libwayland-dev \
                 libxkbcommon-dev pkg-config build-essential

# Raspberry Pi (additionally)
sudo apt install mesa-vulkan-drivers
```

### Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Compile and run

```bash
git clone https://github.com/freedj/freedj-3000
cd freedj-3000
cargo run --release -p opendeck-app -- /path/to/track.mp3
```

### Controls (MVP)

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `←` / `→` | Seek ±10 seconds |
| `Q` / `Esc` | Quit |

### Controller input (development only)

Until the custom hardware and RP2350 firmware exist, a **Numark DJ2Go** (or any class-compliant USB MIDI controller) can be used as a temporary control surface. It is detected automatically over USB MIDI if plugged in before launch.

| Control | Action |
|---|---|
| Jog wheel | Nudge playback speed (snaps back on release) |
| Pitch fader | Set playback speed (±16%) |
| Pitch +/− buttons | Step speed ±1% |
| Play button | Play / pause |
| Cue button | Return to zero |

Deck B controls drive the second beat grid (for ProDJ Link sync comparison):

| Control | Action |
|---|---|
| Jog wheel (ch 1) | Nudge B2 BPM |
| Pitch fader (ch 1) | Set B2 BPM |
| Cue button (ch 1) | Reset B2 beat phase |

MIDI mappings are constants at the top of `crates/app/src/midi.rs`. To discover mappings for a different controller run with `RUST_LOG=debug` — every incoming MIDI message is logged.

### ProDJ Link (receive-only)

FreeDJ-3000 listens on UDP port 50002 for Pioneer beat packets. When a CDJ on the same network sends a beat, the second beat grid (cyan strip at the bottom of the waveform display) updates to show that deck's tempo and phase. Use this to manually beatmatch your track to an incoming CDJ.

For single-machine testing without real hardware:

```bash
python3 tools/send_beat.py 130.0 127.0.0.1 50002
```

---

## Architecture

The project is a Cargo workspace. Each crate has a single responsibility.

```
crates/
  types/        — Core traits: Decoder, BeatAnalyzer, TimecodeDecoder
  decode/       — Audio decoding via Symphonia (MP3, FLAC, AAC, OGG, WAV, AIFF)
  analysis/     — Waveform FFT, beat grid computation
  engine/       — Real-time audio transport (per-sample loop, slip mode, hot cues)
  timestretch/  — Key-lock (Rubber Band) and speed change (rubato resampler)
  timecode/     — DVS vinyl timecode decoder (stub; not a current priority)
  link/         — ProDJ Link network protocol (beat sync, track metadata broadcast)
  protocol/     — MCU serial protocol for the RP2350 control surface
  db/           — SQLite track library (beat grids, cue points, playlists, FTS5 search)
  ui/           — Shared UI components
  app/          — Main binary: ties everything together, wgpu renderer, winit event loop
```

### Graphics stack

- **wgpu** — GPU abstraction over Vulkan (and OpenGL ES on platforms without Vulkan)
- **winit** — Window and input handling, DRM/KMS capable (no display server required)
- **egui** — Immediate-mode UI overlay (transport info, time display)
- **Custom WGSL shader** — Waveform renderer: frequency-colored bar chart, scrolls in real time

The UI philosophy is a **game HUD**, not an application. Every frame redraws the full screen. Input is polled, not event-driven. Target frame rate is 60fps with sub-10ms audio latency maintained on a separate real-time thread.

### Audio engine

The audio engine runs on an isolated real-time thread and communicates with the UI via lock-free atomics. The callback-based cpal stream advances an `AtomicU64` position counter that the renderer reads each frame.

Speed changes use the Rubber Band R3 time-stretcher (`crates/timestretch/`) for key-locked pitch-preserving playback across the full ±16% pitch range. Planned: slip mode, hot cues, saved loops.

### Waveform analysis

Each waveform column is a 2048-sample FFT window (Hann-windowed, HOP_SIZE=512) mapped to three frequency bands:

- **R** — Bass (20–200 Hz)
- **G** — Mid (200 Hz–2 kHz)
- **B** — High (2 kHz–20 kHz)
- **A** — Overall RMS amplitude (controls bar height)

The entire waveform for a 5-minute track is computed in ~0.1 seconds and stored as a GPU storage buffer, one `u32` per column.

### Beat detection

Beat analysis runs offline before playback starts. A 6-second analysis window is fed to [MiniBPM](https://github.com/breakfastquay/minibpm) for tempo estimation, followed by a half-wave-rectified spectral flux onset detector that finds the beat anchor by maximising onset energy at grid positions. The result is a `BeatGrid` with a confidence score, BPM, and anchor sample position.

The beat grid is displayed as white tick marks on the waveform (red on downbeats). The displayed BPM scales with the current pitch-fader position so it always shows the effective playback tempo.

The second beat grid (cyan strip, bottom 40 px) represents an external source — typically a Pioneer CDJ on the network — and is driven by ProDJ Link beat packets in real time. Its visual scroll velocity is scaled by `fader_speed` so it stays locked relative to the audio grid when beatmatched.

### Protocol compatibility

**ProDJ Link** — Pioneer's proprietary CDJ network protocol, fully documented by the [dysentery](https://github.com/Deep-Symmetry/dysentery) project. FreeDJ-3000 implements announce packets, beat packets, and status packets, allowing it to sync BPM and beat phase with real CDJ hardware on the same network.

---

## Philosophy

Pioneer makes good hardware. They also use their market position to maintain a closed ecosystem that extracts money from DJs and venues at every step: expensive hardware, rekordbox subscription, proprietary USB formats, licensing fees for third-party integration.

The open source DJ tooling community (Mixxx, xwax, beat-link, dysentery) has done remarkable work. This project's goal is to turn that knowledge into a complete, deployable hardware product that any DJ can build, any venue can install, and any developer can extend.

The name is deliberately provocative. The CDJ-3000 is not a trademark we are imitating — it is a benchmark we are matching.

---

## Contributing

The codebase is Rust throughout. Contributions welcome in any area. The most useful near-term work:

- **Beat grid editor** — UI for manually correcting auto-detected beat grids
- **rekordbox USB export parser** — read Pioneer's USB drive format to import existing libraries
- **RP2350 firmware** — no_std Rust for the control surface MCU
- **ProDJ Link status parsing** — receive track info and waveform data from real CDJs
- **Hardware BOM and PCB** — the physical build hasn't started yet

See `AUDIO_ENGINE.md` for detailed design documentation on the audio engine.

---

## License

GPL-2.0-or-later. If you build a product with this, the product must be open source. That is intentional.

Protocol implementations (ProDJ Link, timecode formats) are based on publicly documented reverse-engineered specifications and are not subject to any Pioneer trademark or patent claims we are aware of. If you believe otherwise, open an issue.
