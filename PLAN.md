# OpenDeck — Open Source CDJ-3000 Alternative

## Vision

A battle-hardened, open-source digital media player (DMP) targeting professional DJ performance use cases. Feature parity with the Pioneer CDJ-3000/XDJ-RX3, with a decoupled hardware/software architecture, better physical controls, and a software stack designed to never skip or crash under load.

---

## Table of Contents

1. [Goals and Non-Goals](#goals-and-non-goals)
2. [Feature Parity Matrix](#feature-parity-matrix)
3. [Compute Platform Options](#compute-platform-options)
4. [Software Architecture](#software-architecture)
5. [Audio Engine](#audio-engine)
6. [Hardware Interface Architecture](#hardware-interface-architecture)
7. [Physical Hardware Design](#physical-hardware-design)
8. [Robustness and Real-Time Design](#robustness-and-real-time-design)
9. [Open Questions and Risk Areas](#open-questions-and-risk-areas)
10. [Phased Roadmap](#phased-roadmap)

---

## Goals and Non-Goals

### Goals

- Purely digital media player — USB sticks, SD cards, internal SSD/NVMe
- Sub-10ms input-to-audio-output latency target
- High-quality timestretching (pitch-correct key-locked playback at ±100% speed range)
- Flux/Slip mode (play position advances behind the scenes while scratching or looping)
- Beat detection, auto-BPM, beat grid editing
- Hot cues, memory cues, loops, saved loops
- DVS/HID control compatibility (act as a source for Rekordbox, Serato, Traktor)
- Link/ProDJ Link protocol compatibility for sync with other CDJs and mixers
- Open hardware design — no locked-down firmware or proprietary boot chain
- Decoupled hardware/software — the control surface can be separate from the audio compute unit

### Non-Goals

- CD/optical disc playback
- Streaming services integration (out of scope for v1, revisit later)
- Internal mixer (this is a deck, not a standalone all-in-one)
- Windows/macOS driver support for the control surface (Linux USB HID first)

---

## Feature Parity Matrix

| Feature | CDJ-3000 | OpenDeck Target | Notes |
|---|---|---|---|
| BPM detection | Yes | Yes | Phase-aware beat grid |
| Key detection | Yes | Yes | Use Essentia or librosa offline |
| Hot cues (8) | 8 | 8+ | Expandable |
| Saved loops | Yes | Yes | Per-track, stored in database |
| Beat loop | Yes | Yes | 1/32 to 64 bars |
| Slip/Flux mode | Yes | Yes | Core feature, see Audio Engine |
| Waveform display | Yes | Yes | Full + mini overview |
| Quantize | Yes | Yes | Snap cues/loops to beat grid |
| Sync (Link) | Yes | Yes | Ableton Link + ProDJ Link |
| DVS timecode | No | Stretch | Useful for hybrid setups |
| USB media | Yes | Yes | FAT32, exFAT, ext4 |
| SD card | No | Yes | Additional media slot |
| NVMe internal | No | Yes | For library/cache |
| Ethernet (ProDJ Link) | Yes | Yes | |
| WiFi | No | Optional | For library management app |
| HDMI/display output | No | Optional | Mirror waveform to external screen |
| 8" jog wheel | Yes | Yes | Same diameter as CDJ-3000 |
| Platter feel | Tension adjust | Magnetic haptic | See Hardware Design |
| Needle search | Yes | Yes | Capacitive strip or touchpad |
| RGB pad buttons | Yes | Yes | Per-pad addressable LED |
| XLR/RCA audio out | Yes | Yes | Balanced + unbalanced |
| Digital coax out | Yes | Optional | Stretch goal |
| Firmware updates | SD | OTA + USB | |

---

## Compute Platform Options

### Option A: Raspberry Pi 5

**Specs:** Quad-core Cortex-A76 @ 2.4 GHz, 4–8 GB LPDDR4X, PCIe 2.0 (M.2 HAT available), USB 3.0, GPIO 40-pin

**Pros:**
- Enormous community, Linux RT kernel patches well-tested on RPi
- GPIO for direct low-latency control surface connection
- PCIe for NVMe internal storage
- 8 GB variant handles library indexing + audio engine simultaneously
- Power envelope (12W typical) means simple PSU design
- Active cooling HATs available
- PREEMPT_RT patches exist and are stable

**Cons:**
- No built-in high-quality audio DAC — requires I2S DAC HAT or USB audio interface
- GPU (VideoCore VII) is not well-suited for accelerating signal processing
- Real-time performance under heavy I/O load (USB stick + display + audio) requires careful kernel config
- Not the fastest option for waveform rendering with a rich UI

**Audio output path:** RPi I2S → ES9038Q2M or PCM5122 DAC HAT → balanced output stage. This is a known-good path used in audiophile builds. Total BOM cost ~$15–40 for DAC section.

**Verdict:** Best overall choice for v1. Large community, GPIO-native control surface path, proven RT kernel. Choose 8 GB variant.

---

### Option B: Android (Snapdragon/MediaTek SoC)

**Pros:**
- Mature audio stack (AAudio/Oboe) with round-trip latency as low as 5ms on modern devices
- Touch display integration is first-class
- Very high CPU performance (Snapdragon 8-gen series)
- Can reuse Android DJ app codebases (Mixvibes, djay, etc. are Android-native)

**Cons:**
- Audio HAL and driver stack is opaque and vendor-specific — hard to guarantee across hardware generations
- GPIO access for physical controls is non-existent on stock Android — requires custom BSP or companion MCU
- Bootloader locking is a real concern for custom hardware builds; most Android SoC eval boards (RK3588, i.MX8) require significant BSP work
- App distribution model is not appropriate for embedded appliance use
- Latency consistency is good on flagship phones but degrades on cheaper boards
- ProDJ Link and Ableton Link need to be implemented from scratch on Android
- AOSP bring-up on custom hardware is substantial engineering effort

**Verdict:** Not recommended for v1 or v2. The control surface problem alone (no GPIO) makes this painful. Revisit only if targeting a product that runs on an existing Android tablet form factor.

---

### Option C: Intel NUC / x86-64 Mini PC

**Examples:** NUC 13 Pro, Beelink Mini S12, Trigkey G4

**Pros:**
- Fastest CPU option by a wide margin — no concern about audio DSP overhead
- PCIe NVMe, multiple USB 3.2, Thunderbolt on NUC
- Full Linux desktop distribution runs without modification
- JACK or PipeWire with low-latency kernel straightforward
- Can run any Linux DJ software as a backend (Mixxx, etc.) without porting effort

**Cons:**
- No GPIO — control surface must be USB HID (adds latency hop, typically 1ms @ 1000 Hz poll)
- Power draw 15–35W — requires larger PSU, more heat management
- Form factor is larger and harder to integrate into a CDJ-style enclosure
- More expensive ($150–400 for the compute module vs ~$80 for RPi 5)
- Cooling fan noise requires acoustic isolation in the enclosure

**Verdict:** Best fallback if RPi 5 proves insufficient for timestretching quality at target latency. The NUC path trades GPIO latency for raw CPU headroom. Using a $50 STM32 or RP2350 MCU as a GPIO bridge over USB HID or UART would recover most of the control surface latency.

---

### Option D: Hybrid — RPi 5 + RP2350/STM32 MCU

**Architecture:** RPi 5 handles all audio/DSP/UI. A dedicated microcontroller (RP2350 or STM32F7) handles all physical control inputs at <1ms interrupt latency and communicates with the RPi over SPI or UART at high speed.

**Pros:**
- Completely decouples control surface timing from Linux scheduler jitter
- MCU handles encoder debouncing, LED multiplexing, capacitive sensing, jog wheel quadrature decoding — all deterministic
- SPI at 10+ MHz between MCU and RPi means sub-millisecond control event delivery
- Same physical design works with NUC backend (swap SPI for USB HID from same MCU firmware)
- Jog wheel scratch detection requires sub-millisecond position updates — MCU is the right place for this
- MCU can implement safety watchdog: if RPi hangs, MCU can blink/alert without requiring a full reboot

**Cons:**
- More complex firmware split — need to define a clean protocol between MCU and host
- Two firmware targets to maintain
- Adds $10–15 BOM cost

**Verdict:** This is the recommended architecture. The MCU handles everything physical; the RPi handles everything computational. This also means the control surface is inherently portable — you can talk to any backend that speaks the protocol.

---

### Platform Recommendation

**Primary: Raspberry Pi 5 (8 GB) + RP2350 MCU** for v1 prototype.

Keep NUC as a documented fallback path. The MCU bridge means the software architecture does not change between the two backends.

---

## Software Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        OpenDeck Host (RPi 5 / NUC)         │
│                                                             │
│  ┌──────────────┐   ┌──────────────┐   ┌────────────────┐  │
│  │  Media DB    │   │  Analysis    │   │  UI Process    │  │
│  │  (SQLite)    │   │  Worker      │   │  (Slint/Qt)    │  │
│  │              │   │  (offline    │   │                │  │
│  │  - library   │   │   BPM/key)   │   │  - waveform    │  │
│  │  - cue pts   │   │              │   │  - browser     │  │
│  │  - beat grid │   └──────────────┘   │  - metadata    │  │
│  └──────┬───────┘                      └───────┬────────┘  │
│         │                                      │           │
│  ┌──────▼──────────────────────────────────────▼────────┐  │
│  │                    Audio Engine (Rust)               │  │
│  │                                                      │  │
│  │  ┌────────────┐  ┌──────────────┐  ┌─────────────┐  │  │
│  │  │  Decoder   │  │  Timestretch │  │  Beat/Loop  │  │  │
│  │  │  (MP3/AAC/ │  │  Engine      │  │  Engine     │  │  │
│  │  │  FLAC/WAV/ │  │  (rubberband │  │             │  │  │
│  │  │  AIFF)     │  │  or custom)  │  │  Slip track │  │  │
│  │  └────────────┘  └──────────────┘  └─────────────┘  │  │
│  │                                                      │  │
│  │  Real-time audio callback (JACK / ALSA direct)      │  │
│  │  Target: 128 or 256 sample buffer @ 44.1/48 kHz     │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │              Control Surface IPC                     │  │
│  │   SPI daemon (GPIO path) or USB HID daemon           │  │
│  │   Publishes events to audio engine via lock-free     │  │
│  │   ring buffer (SPSC)                                 │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
         ▲ SPI/UART or USB HID
         │
┌────────┴────────────────────────────────────────────────────┐
│                  RP2350 / STM32 MCU                         │
│                                                             │
│  - Jog wheel quadrature decode (position + velocity)       │
│  - All button/pad matrix scanning (1ms debounce)           │
│  - Encoder reading (BPM adjust, loop size, browse)         │
│  - Capacitive needle search strip                          │
│  - RGB LED PWM output (per-pad, ring, indicators)          │
│  - Watchdog heartbeat to RPi                               │
└─────────────────────────────────────────────────────────────┘
```

### Process Isolation Strategy

- **Audio engine**: isolated process with `SCHED_FIFO` RT priority, `mlockall()` to prevent page faults, runs on a dedicated CPU core (via `isolcpus` kernel param)
- **UI process**: separate process, communicates with audio engine over shared memory ring buffer (no syscalls in the audio path)
- **Media DB / Analysis**: lowest priority background process, I/O-bound, never touches audio thread
- **Control IPC**: runs at high RT priority but below audio; events are placed into lock-free SPSC queue consumed by audio engine

### Language Choices

| Component | Language | Rationale |
|---|---|---|
| Audio engine | Rust | Memory safety without GC pauses; excellent for lock-free concurrent structures; `cpal` crate for audio I/O |
| MCU firmware | Rust (embassy) or C | embassy-rp for RP2350 is mature; C as fallback |
| UI | Rust + wgpu + egui | Immediate mode game-style rendering. wgpu for custom waveform/jog shaders; egui (egui-wgpu backend) for pads, text, track browser. Full frame redraw every 60fps, no retained widget tree. |
| Media analysis | Python (offline) | librosa/essentia for BPM/key analysis during library scan — not in real-time path |
| Build system | Cargo workspace | Unified build for all Rust components |

### Database Schema (SQLite)

- `tracks` — path, hash, BPM, key, duration, sample rate, analyzed_at
- `beat_grids` — track_id, anchor_time, bpm, grid_type (fixed/variable)
- `cue_points` — track_id, index (0–7), position_samples, color, label, type (hot/memory/loop_in/loop_out)
- `loops` — track_id, in_point, out_point, label, active
- `play_history` — track_id, played_at, deck_id

---

## Audio Engine

### Timestretching [DECIDED]

The quality of the timestretch algorithm is the most audible differentiator between mediocre and professional DMPs. Unlike a DAW where timestretch is applied occasionally, on a DJ deck the signal is **always running through the timestretcher** — even at 1× speed it must be in the path for seamless pitch nudge response. This means artifacts are permanently audible and algorithm quality is a primary product differentiator.

**Decision: Rubber Band Library R3 ("Finer" engine) via C FFI.**

- No `rubberband` crate exists on crates.io — bindings written by hand against `rubberband-c.h`
- System dependency: `librubberband-dev` (v3.3.0 on Ubuntu/Debian apt, LGPL 2.1+)
- R3 engine selected (`RubberBandOptionEngineFiner`) — highest quality, acceptable latency (~100ms, handled by pre-roll)
- R2 engine (`RubberBandOptionEngineDefault`) retained as a fallback option for constrained hardware

**WSOLA explicitly rejected.** Good quality only for ±8% ratios, audible transient smearing at larger shifts. Not acceptable for a product competing with Pioneer.

**SoundTouch explicitly rejected.** Lower quality than Rubber Band, used in Mixxx as a pragmatic choice, not a quality one.

**Audio pipeline architecture (required for variable-speed playback):**

```
Decoded PCM (Arc<Vec<f32>>)
    │
    ▼
Processor thread  ──── speed: AtomicF32 (0.5–2.0)
    │                   key_lock: AtomicBool
    │   key-lock ON:  Rubber Band R3 (pitch preserved)
    │   key-lock OFF: read at adjusted rate (pitch follows speed, vinyl feel)
    ▼
rtrb SPSC ring buffer  (~8192 frames, pre-allocated)
    │
    ▼
cpal callback  (drains ring buffer, outputs to hardware)
```

The position `AtomicU64` is updated by the processor thread after each consumed block. The waveform renderer reads it each frame for scroll position — no change needed there.

**Build requirement:** `sudo apt install librubberband-dev`

### Slip/Flux Mode

Slip mode maintains two independent playback positions:

- **Visible position**: current position the user sees/controls (scratch, loop, hot cue jump)
- **Ghost position**: where the track would be if playing normally, advances at 1x always

When slip mode is active and the user releases the jog or exits a loop, the visible position snaps to the ghost position (with a short crossfade to avoid click).

Implementation:
```
struct SlipState {
    visible_pos: AtomicU64,   // in samples, manipulated by jog/loop engine
    ghost_pos: AtomicU64,     // advances every audio callback by buffer_size
    slip_active: AtomicBool,
    crossfade_buf: [f32; XFADE_SAMPLES],  // populated on slip release
}
```

The audio callback always outputs from `visible_pos`. On slip release, it schedules a crossfade from current buffer into ghost position stream over ~10ms.

### Beat Detection and Quantize [DECIDED]

**Decision: pure Rust implementation in `crates/analysis`, runs synchronously at track load.**

Pipeline (implemented in `crates/analysis/src/beat.rs`):
1. Bass-focused onset strength signal (80–400 Hz bandpass IIR + half-wave rectify + differentiate)
2. Autocorrelation over 6-second window → BPM + confidence score
3. Phase estimation by maximising onset energy at grid positions → anchor sample
4. Downbeat detection (bar structure) — TODO

Result stored as `BeatGrid` (anchor_sample + BPM for constant-tempo, per-beat sample array for variable). Runs in <0.1s on a 5-minute track.

**Beat grid displayed on waveform** as vertical tick marks in the WGSL fragment shader. Beat period computed from BPM + sample rate, ticks drawn as faint vertical lines at the correct column offsets from the anchor.

**BPM shown in egui header overlay** alongside time/filename.

Essentia/madmom path remains viable for a future "deep analysis" mode on library import — better accuracy for complex material. Not needed for MVP.

- Offline analysis: full beat grid computed when track is loaded into library ✅
- Online (live) analysis: streaming BPM tap / beat detection for tracks not yet analyzed — TODO
- Quantize mode: cue points and loop points snap to nearest beat grid position — TODO
- Beat grid can be manually corrected via UI — TODO

### Latency Budget

Target: 10ms input-to-output from jog wheel touch to audible pitch change.

| Stage | Budget |
|---|---|
| MCU jog wheel interrupt → SPI packet to RPi | 0.2ms |
| RPi SPI interrupt → audio engine event queue | 0.3ms |
| Audio engine reads event, applies pitch change | ~0ms (next callback) |
| Audio buffer size (256 samples @ 44.1kHz) | 5.8ms |
| DAC output latency (I2S + output stage) | 0.5ms |
| **Total** | **~6.8ms** |

At 256 samples @ 44.1kHz, one buffer period is 5.8ms. This means the absolute floor is ~6ms end-to-end with the above architecture. 128-sample buffers get to ~3.5ms total but increase CPU load by ~2x for the audio thread. 256 samples is the recommended starting point.

---

## Hardware Interface Architecture

### Option A: GPIO/SPI (RPi native)

```
RPi GPIO ──── RP2350 MCU ──── jog wheel encoder
                         ──── button matrix
                         ──── LED drivers
                         ──── capacitive strip
```

- SPI clock 8–20 MHz
- MCU sends structured binary packets: `[type(1), timestamp_us(4), value(4)]`
- Interrupt-driven on RPi side, `SCHED_FIFO` priority SPI daemon
- Round-trip MCU→RPi→audio engine: ~0.5ms

**Pros:** Lowest latency path, no USB overhead, simple power delivery from RPi 5V rail
**Cons:** Ties the design to RPi; NUC path requires a different physical board

### Option B: USB HID (Universal)

```
RPi/NUC USB Host ──── RP2350 MCU ──── all physical controls (same as above)
```

- MCU presents as USB HID composite device (gamepad + custom HID descriptor)
- Linux reads via `hidraw` at 1000 Hz polling (1ms)
- Works identically on RPi, NUC, or any Linux machine
- MCU firmware is identical; only the host-side SPI daemon is replaced with a hidraw reader

**Pros:** Backend-agnostic; same MCU firmware everywhere; standard USB connection
**Cons:** +1ms latency vs SPI; USB adds a small amount of scheduler jitter

### Option C: UART (SPI fallback)

- If SPI is too complex to debug, UART at 921600 baud is ~0.1ms per 9-byte packet
- Easier to debug (can monitor with a logic analyzer without needing full SPI decode)
- RP2350 UART ↔ RPi UART (no USB needed, but 3.3V level compatible)

### Recommendation

Design the MCU board with both USB and SPI/UART headers. The firmware supports both transport modes selectable at build time (or even runtime). Default to USB for ease of development and testing; switch to SPI for final production hardware for minimum latency.

**Protocol spec (draft):**

```
Packet (9 bytes, little-endian):
  [0]    type:   JOG_DELTA=0x01, JOG_TOUCH=0x02, BUTTON=0x03, ENCODER=0x04, STRIP=0x05
  [1-4]  timestamp: microseconds since MCU boot (u32)
  [5-8]  value: i32 (delta for jog/encoder, button bitfield, strip position 0–1023)
```

---

## Physical Hardware Design

### Jog Wheel / Platter

**Size:** 206mm diameter (same as CDJ-3000)

**Feeling mechanism options:**

#### Option A: Magnetic Haptic / Eddy Current Braking (NI Traktor-style)
Native Instruments uses a permanent magnet disc spinning near a conductive aluminum plate — the interaction creates eddy currents that provide resistance proportional to spin speed. This is the mechanism in the Kontrol S4/S2 jogwheels.

- **Patent status:** NI has patents on specific implementations. The fundamental physics of eddy current braking is not patentable. A sufficiently different mechanical implementation should be clear of NI's specific claims. **Legal review recommended before manufacturing.**
- **Feel:** Excellent inertia simulation, proportional resistance, no wear parts
- **Implementation:** Aluminum disc on bearing, permanent magnet array on fixed plate below, gap adjustable via set screw for tension adjustment

#### Option B: Magnetic Torque Motor
Brushless DC motor used as a haptic actuator — can both resist and add force. Enables future "vinyl simulation" with accurate pitch/resistance coupling.

- More complex, requires motor driver IC and control loop
- Higher BOM cost (~$20–40 for motor + driver)
- Best feel possible; used in high-end turntable simulation systems

#### Option C: Friction Belt with Tension Knob (Pioneer-style)
Simple felt or polymer brake pad, adjustable tension knob.

- Cheap, reliable, no electronics
- Feel is good but not as dynamic as magnetic options
- Proven in field (every CDJ since the CDJ-500)

**Recommendation for v1:** Eddy current approach (Option A). Lower complexity than motor control, excellent feel, no wear. Commission a mechanical engineer for the geometry to ensure we stay clear of NI patents. Add a tension adjustment screw.

**Platter position sensing:**
- Optical encoder disc on platter shaft, 2400 PPR minimum (higher = better scratch resolution)
- Quadrature decode in MCU
- Touch detection via capacitive sensor ring on platter surface (conductive top plate, MCU reads capacitance via dedicated pin)

### Button Design

Pioneer uses standard 6×6mm or 12×12mm tactile switches with rubber dome keycaps. The feel is mediocre.

**Better alternatives:**
| Type | Feel | Cost | Notes |
|---|---|---|---|
| Cherry MX Low Profile RGB | Clicky/tactile, 45g actuation | ~$1.50/switch | Requires keycap design |
| Kailh Choc v2 | Low travel, good tactile | ~$0.50/switch | Popular in compact keyboards |
| Alps SKQG (SMD) | Soft tactile, low noise | ~$0.30/switch | Industry standard for audio gear |
| Omron B3FS | Smooth, low noise, SMD | ~$0.40/switch | Used in pro audio consoles |

**Recommendation:** Alps SKQG or Omron B3FS for most buttons (industry-appropriate feel). Kailh Choc v2 for the 8 hot cue pads where tactile feedback during performance is critical. All pads should have per-key RGB via SK6812-EC15 LEDs (same footprint as WS2812B but with brighter white channel).

### Display

- 7" or 9" IPS LCD, 1280×800, mounted at CDJ-3000 waveform display angle
- Direct MIPI DSI connection to RPi 5 (eliminates USB display latency)
- Capacitive multitouch for UI interaction
- Custom UI in Slint (native GPU rendering via DRM/KMS, no X11 overhead)

### Audio Output

- Balanced XLR (master out): via PCM5122 or ES9038Q2M I2S DAC + INA1620 differential output op-amp
- Unbalanced RCA: same DAC, unbalanced tap
- Ground lift on unbalanced outputs
- Output level: +6 dBu nominal, +21 dBu max (matches CDJ-3000 spec)
- 24-bit/96kHz DAC path (downsample from internal processing rate if needed)

---

## Robustness and Real-Time Design

### Linux Kernel Configuration

```
# Key kernel config for audio RT performance
CONFIG_PREEMPT_RT=y              # Full RT preemption
CONFIG_HZ=1000                   # 1ms timer granularity
CONFIG_CPU_ISOLATION=y           # isolcpus for audio core
CONFIG_NO_HZ_FULL=y              # tickless on isolated core
CONFIG_IRQ_TIME_ACCOUNTING=y
```

Boot parameters:
```
isolcpus=3 nohz_full=3 rcu_nocbs=3 irqaffinity=0-2
```

This dedicates CPU core 3 exclusively to the audio engine. No kernel threads, no IRQs, no timer ticks on that core during playback.

### Audio Engine Hardening

- `mlockall(MCL_CURRENT | MCL_FUTURE)` — no page faults during playback
- Pre-fault all audio buffers at startup
- All allocations in audio callback are forbidden — pre-allocate everything
- Use `SCHED_FIFO` priority 80 for audio thread
- Double-buffered decode: background thread decodes ahead by 2 seconds, audio thread reads from pre-decoded ring buffer
- File I/O never happens in audio thread — media reader runs in separate thread

### Crash Recovery

- Audio engine is a separate process; if UI crashes, audio continues
- Watchdog timer in MCU: if RPi stops sending heartbeat for >2 seconds, MCU puts LEDs into "fault" state and can optionally trigger a safe restart (configurable)
- Systemd service with `Restart=always`, `RestartSec=500ms`
- Audio output process uses `O_DIRECT` or `mmap` for file access to avoid kernel buffer cache issues
- SQLite WAL mode for all database writes (no blocking on library scan while playing)

### Memory Layout

- Audio ring buffer: 4 seconds of stereo 32-bit float = 4 × 44100 × 2 × 4 = ~1.4 MB per deck
- Waveform data (overview, zoomed): pre-computed, stored in DB as binary blob
- Peak/RMS cache for display: computed at load time, memory-mapped file

### Testing Strategy

- Unit tests for all DSP components (timestretch, beat grid, slip state machine)
- Integration test suite that generates synthetic audio with known BPM and verifies beat detection accuracy
- Latency measurement test: hardware GPIO pulse → audio output spike → measure with oscilloscope or ADC loopback
- Soak test: run for 72 hours with automated track loading/scratching script, monitor for xruns, CPU spikes, memory growth
- Fuzzing: malformed media files (fuzz the decoder), malformed USB HID packets (fuzz the control IPC)

---

## Graphics Stack

### Philosophy

The UI is a game HUD, not an application. Every frame redraws everything. Input is polled, not event-driven. Most of what's on screen is custom GPU rendering that no widget toolkit knows about — a retained-mode widget tree is the wrong model entirely.

### Stack: wgpu + egui

**wgpu** is the rendering foundation. Pure Rust, no FFI. Targets Vulkan on RPi 5 (via Mesa V3DV driver, solid since kernel 6.1) and on Android. Runs via `winit` which supports DRM/KMS on Linux — no X11 or Wayland needed on a dedicated embedded device. This was a friction point with `sokol_app` (used in the related `fast-vj` project), which expects a display server on Linux.

**egui** (`egui-wgpu` backend) provides the immediate mode UI layer. The key property: egui has a `Painter` API that exposes arbitrary triangles, textured quads, bezier curves, and line segments — so custom elements (pads, overlays, beat markers) are draw calls, not widgets. Standard egui widgets handle only the parts that benefit from them: the track browser scroll list and text labels.

### Frame Structure

```
each frame (60fps):
  ┌─ wgpu render pass 1: waveform texture quad (custom WGSL shader)
  │    color-mapped FFT data scrolling with playhead
  │    beat grid lines, cue markers, loop region as additional geometry
  │
  ├─ wgpu render pass 2: jog wheel (custom WGSL shader)
  │    circle, platter position ring, touch indicator
  │
  └─ egui pass (egui-wgpu integration):
       Painter::rect        — 8 hot cue pads (colored, per-pad RGB)
       Painter::text        — BPM, key, time elapsed/remaining
       Painter::line        — beat drift overlay on waveform
       ScrollArea + Label   — track browser list
       Painter::*           — loop region handles, slip mode ghost indicator
```

### Graphics API Considered and Rejected

| Option | Reason rejected |
|---|---|
| sokol + sokol_gfx | C library (FFI in a Rust project), no compute shaders, sokol_app fights DRM/KMS on embedded Linux |
| GLFW + raw OpenGL ES | C FFI, no type safety, same DRM/KMS issue, no advantage over wgpu |
| Slint | Retained-mode widget toolkit — wrong model for a game-style HUD. No custom shader integration. |
| imgui-rs + imgui-wgpu | C wrapper with thin Rust bindings, imgui-wgpu historically lags wgpu API versions |
| Pure wgpu + glyphon (no IMGUI) | Viable but track browser list (scroll + text layout) is painful to write from scratch |

### RPi 5 GPU Notes

- VideoCore VII supports OpenGL ES 3.1 and Vulkan 1.2 via Mesa V3DV
- wgpu targets Vulkan on RPi 5 — not GLES, not a compatibility layer
- DRM/KMS via winit eliminates the display server requirement entirely
- MIPI DSI panel connects directly; the compositor layer is our render loop

### Waveform Rendering Detail

The waveform is a pre-computed RGB texture (frequency-mapped, ~93KB for a 6-minute track). Each frame:
1. Compute scroll offset from current playhead position (atomic read, no audio thread involvement)
2. `write_texture` for any newly-analyzed columns not yet on GPU (incremental upload)
3. Draw a fullscreen quad with a WGSL fragment shader that samples the texture with the scroll offset
4. Overlay geometry: beat grid lines, cue point markers, loop region shading, slip ghost position

The fragment shader does almost nothing — the waveform color data is pre-baked. GPU load for the waveform is negligible.

---

## Open Questions and Risk Areas

### High Risk

1. **Timestretching quality on RPi 5:** Rubber Band R3 is CPU-intensive. Need to benchmark two-deck simultaneous playback at 128/256 buffer sizes on actual RPi 5 hardware. If insufficient, NUC or dedicated DSP chip (ADSP-21489) fallback.

2. **Platter patent landscape:** NI's eddy current / magnetic platter patents need specific review before committing to that design. Budget for a patent attorney search.

3. **ProDJ Link protocol:** Documented by the `dysentery` project (James Elliott). The `beat-link` Java library is a complete working implementation. We implement a native Rust port for network sync and metadata sharing with Pioneer CDJs.

### Medium Risk

4. **Ableton Link:** Fully open protocol (Apache 2.0 licensed SDK). Low risk, straightforward to integrate.

5. **Audio codec licensing:** MP3 (expired patents as of 2017), AAC (patent pools still active — use FAAD2 which is GPL, or negotiate license), FLAC/WAV/AIFF (free). AAC is legally complex for a hardware product — consult licensing.

6. **Display latency:** Waveform animation at 60fps on RPi 5 with Slint is achievable but requires care. Offloading waveform rendering to a texture updated from the audio thread's position pointer is the right approach.

7. **USB storage reliability:** FAT32/exFAT drivers on Linux are mature. Need to handle hot-plug gracefully — tracks in the play queue must be fully buffered before the stick is removed (4-second read-ahead covers most cases).

### Low Risk

8. **MCU firmware complexity:** RP2350 with embassy-rs is well-suited. Quadrature decoding, LED PWM, SPI/USB at this scale is well-trodden territory.

9. **SQLite at scale:** A DJ library of 10,000 tracks fits in ~50MB. SQLite handles this trivially.

---

## Phased Roadmap

### Phase 0: Feasibility (1–2 months)
- [ ] Benchmark Rubber Band R2/R3 on RPi 5 (two decks, 256-sample buffer)
- [x] Prototype audio engine skeleton in Rust (cpal → ALSA, RT thread, slip state machine)
- [ ] Prototype MCU firmware on RP2350 devboard (jog wheel encoder + USB HID)
- [ ] Verify 10ms latency target with oscilloscope measurement
- [ ] Patent search: eddy current platter mechanism

### Phase 1: Software MVP (3–4 months)
- [x] Audio decode (MP3/FLAC/AAC/OGG/WAV/AIFF via Symphonia)
- [x] Waveform visualization (FFT-based frequency-colored, GPU storage buffer, WGSL shader)
- [x] Play / pause / seek (keyboard + S2 MK2 HID)
- [x] Jog wheel scrub + nudge (S2 MK2, touch-sensitive)
- [ ] **Beat grid detection + BPM display** ← in progress
- [ ] **Rubber Band R3 timestretching + speed control** ← in progress
- [ ] Beat grid markers on waveform
- [ ] Slip/flux mode
- [ ] Hot cues (8), memory cues, basic loops
- [ ] SQLite media library with USB/SD mount support
- [ ] Full UI (waveform + track browser) on 7" display
- [ ] Custom RP2350 MCU firmware + hardware
- [ ] Ableton Link sync

### Phase 2: Feature Complete (3–4 months)
- [ ] ProDJ Link (Ethernet sync, track metadata sharing)
- [ ] Beat-synced loops (quantize)
- [ ] Key detection and display
- [ ] Loop roll, beat jump
- [ ] Full waveform color (frequency-mapped, like CDJ-3000)
- [ ] Performance mode UI (large waveforms, pad performance modes)

### Phase 3: Hardware v1 Prototype (2–3 months)
- [ ] PCB design: MCU board, DAC/output board, power board
- [ ] Enclosure: CNC aluminum front panel + ABS/aluminum chassis
- [ ] Jog wheel mechanical design and prototype
- [ ] Assembly and soak testing

### Phase 4: Open Source Release
- [ ] Documentation (build guide, BOM, firmware flashing)
- [ ] Certification considerations (CE/FCC for any commercial kits)
- [ ] Community contribution guidelines

---

## Key Dependencies and References

- **Rubber Band Library:** https://breakfastquay.com/rubberband/ (LGPL 2.1)
- **Ableton Link SDK:** https://github.com/Ableton/link (Apache 2.0)
- **dysentery (ProDJ Link documentation):** https://djl-analysis.deepsymmetry.org/
- **beat-link (Java ProDJ Link implementation):** https://github.com/Deep-Symmetry/beat-link
- **xwax (open-source DVS, reference timecode decoders):** https://xwax.org
- **Mixxx (open-source DJ software):** https://mixxx.org — study for architecture patterns
- **JACK Audio:** https://jackaudio.org
- **cpal (Rust audio I/O):** https://github.com/RustAudio/cpal
- **wgpu (Rust GPU API):** https://github.com/gfx-rs/wgpu
- **egui (immediate mode UI):** https://github.com/emilk/egui
- **glyphon (wgpu text rendering):** https://github.com/grovesNL/glyphon
- **embassy (Rust async embedded):** https://embassy.dev
- **Essentia (audio analysis):** https://essentia.upf.edu

---

*This document is a living plan. Decisions should be updated here before implementation begins. Nothing in this plan is final until marked with [DECIDED].*

---

## Decisions Log

Decisions made during active development. Cross-referenced to the sections above.

| Date | Decision | Rationale |
|---|---|---|
| 2026-03-23 | Graphics: wgpu 22 + egui 0.29 + winit 0.30 [DECIDED] | Slint rejected (retained mode, wrong model). sokol rejected (C FFI, DRM/KMS friction). wgpu + egui delivers game-HUD rendering with no C++ deps. |
| 2026-03-23 | Waveform storage: GPU storage buffer, not texture [DECIDED] | GLES max texture dimension is 2048 — waveform for a 5-min track at HOP=512 exceeds this. Storage buffers have no dimension limit. Each column packed as u32 (RGBA bytes). |
| 2026-03-23 | Beat detection: pure Rust in crates/analysis, not Python [DECIDED] | Autocorrelation + onset strength runs in <0.1s on full track. No Python dep needed for offline analysis. Python/Essentia path retained as optional future upgrade for higher accuracy. |
| 2026-03-23 | Timestretching: Rubber Band R3 via hand-written C FFI [DECIDED] | No `rubberband` crate on crates.io. WSOLA explicitly rejected — not good enough quality for a pro product where timestretch is always in the signal path. Rubber Band R3 ("Finer" engine) is the correct choice. FFI bindings written directly against the Rubber Band C API (`rubberband-c.h`). System dep: `librubberband-dev` (v3.3.0 available in apt). |
| 2026-03-23 | Audio pipeline: processor thread + rtrb ring buffer [DECIDED] | Direct callback read from sample buffer cannot support variable speed. Processor thread runs Rubber Band, writes to rtrb SPSC queue, cpal callback drains it. Position AtomicU64 updated by processor thread. |
| 2026-03-23 | Dev control surface: Traktor Kontrol S2 MK2 (temporary) [DECIDED] | USB HID, proprietary NI protocol. Jog wheel = 24-bit absolute position counter in bytes [3,2,1] (MSB first). Touch sensor = byte[10] bit 0x01. Play = byte[11] bit 0x01. Cue = byte[11] bit 0x02. Not a long-term target — replaced by custom RP2350 hardware. |
