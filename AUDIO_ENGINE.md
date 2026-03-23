# Audio Engine Design

Single-deck, no mixing, no effects chain. Every design decision is made to serve the performance use case: tight latency, correct pitch, no crashes, no skips.

---

## Table of Contents

1. [Design Principles — Lessons From Mixxx](#design-principles)
2. [Component Map](#component-map)
3. [Threading Model](#threading-model)
4. [Decode and I/O Pipeline](#decode-and-io-pipeline)
5. [Transport State Machine](#transport-state-machine)
6. [Pitch, Speed, and Timestretching](#pitch-speed-and-timestretching)
7. [Slip Mode](#slip-mode)
8. [Hot Cues and Loops](#hot-cues-and-loops)
9. [Vinyl Control / DVS](#vinyl-control--dvs)
10. [Beat Analysis Pipeline](#beat-analysis-pipeline)
11. [Waveform Data](#waveform-data)
12. [Inter-Deck Beat Grid Protocol](#inter-deck-beat-grid-protocol)
13. [Library and Browsing](#library-and-browsing)
14. [Data Structures and Interfaces](#data-structures-and-interfaces)

---

## Design Principles

### Lessons From Mixxx

Mixxx has been developed since 2002. It is genuinely good software. These are the patterns we should not repeat:

**1. The monolithic EngineBuffer problem.**
Mixxx's `EngineBuffer` handles seeking, looping, pitch, effects send, scratch, sync, and cue simultaneously in one class. State changes to one feature produce unexpected interactions with another. We separate these concerns into clearly bounded components that communicate through well-defined interfaces.

**2. Allocating inside the audio callback.**
Mixxx has had recurring bugs where loading a track causes an allocation in the audio thread path. In our design, the audio callback touches zero heap allocations. All buffers are pre-allocated at startup. All state is communicated via lock-free primitives.

**3. Waveform rendering on the audio thread.**
Mixxx's older waveform code computed pixel data during the audio callback. Our waveform data is pre-computed at analysis time and stored as a cache. The UI reads from this cache on the UI thread. The audio engine only publishes its current position — it does not participate in rendering.

**4. Overcomplicated sync / effects chain.**
Mixxx's sync engine and effects chain are deeply tangled with the core transport. We remove this entirely from v1. The only inter-deck data we need right now is a beat clock signal for drift visualization. This is a read-only feed published by the transport, consumed independently.

**5. Feature flags in the hot path.**
"If key lock enabled, else..." conditionals in the audio callback produce branch mispredictions and force the hot path to carry dead code. We model this with a pipeline: at the moment key lock is toggled, we swap the active processing stage. The callback always calls the same interface.

**6. No single-responsibility decode thread.**
Mixxx's read-ahead is coupled to its effect processing. Our decode thread has one job: read compressed audio from disk, decode it to PCM, write into a ring buffer. Nothing else.

### Core Rules

- Audio callback: zero allocations, zero I/O, zero blocking calls, zero logging
- All cross-thread communication: lock-free ring buffers or atomics
- All disk I/O: in the decode thread, never the audio thread
- All analysis (BPM, waveform, key): in a background thread pool, never blocking UI or audio
- Position is always tracked in **samples** (integer), never in seconds (floating point drift)
- Every component is independently testable without a running audio device

---

## Component Map

```
┌─────────────────────────────────────────────────────────────────┐
│  ANALYSIS SUBSYSTEM (background thread pool, non-RT)            │
│                                                                 │
│  ┌─────────────────────┐   ┌─────────────────────────────────┐  │
│  │  BeatAnalyzer       │   │  WaveformBuilder                │  │
│  │                     │   │                                 │  │
│  │  Input: PCM stream  │   │  Input: PCM stream              │  │
│  │  Output: BeatGrid   │   │  Output: WaveformCache file     │  │
│  │                     │   │                                 │  │
│  │  Works offline      │   │  Offline: full pass             │  │
│  │  Works streaming    │   │  Online: builds incrementally   │  │
│  └──────────┬──────────┘   └─────────────────────────────────┘  │
│             │ BeatGrid (once computed)                           │
└─────────────┼───────────────────────────────────────────────────┘
              │
┌─────────────▼───────────────────────────────────────────────────┐
│  DECODE SUBSYSTEM (dedicated thread, non-RT)                    │
│                                                                 │
│  MediaReader → Decoder → [PCM ring buffer, 4s capacity]        │
│                                                                 │
│  Formats: MP3 (minimp3), FLAC (symphonia), AAC (fdk-aac/faad), │
│           WAV, AIFF                                             │
│                                                                 │
│  Maintains read-ahead cursor, seeks on demand via command       │
│  channel, never blocks audio thread                             │
└─────────────┬───────────────────────────────────────────────────┘
              │ lock-free SPSC ring buffer
┌─────────────▼───────────────────────────────────────────────────┐
│  AUDIO ENGINE (RT thread, SCHED_FIFO, isolated CPU core)        │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Transport                                               │   │
│  │  State machine: Stopped / Playing / Cued / Scratching   │   │
│  │  Owns: active_pos (AtomicU64), ghost_pos (AtomicU64)    │   │
│  │  Consumes: ControlEvent queue from control surface       │   │
│  └────────────────────────┬─────────────────────────────────┘   │
│                           │ current position + speed             │
│  ┌────────────────────────▼─────────────────────────────────┐   │
│  │  Processing Pipeline (swappable at runtime)              │   │
│  │                                                          │   │
│  │  Mode A (no key lock):  PCM → Resampler → output        │   │
│  │  Mode B (key lock):     PCM → Timestretcher → output    │   │
│  │  Mode C (DVS):          Timecode → velocity → mode A/B  │   │
│  └────────────────────────┬─────────────────────────────────┘   │
│                           │ f32 stereo frames                    │
│  ┌────────────────────────▼─────────────────────────────────┐   │
│  │  Output stage                                            │   │
│  │  - Volume (unity in our case, no channel fader)          │   │
│  │  - DC offset correction                                  │   │
│  │  - Clip limiter (soft clip at -0.1 dBFS)                │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                 │
│  Publishes: EngineState (position, bpm, beat_phase) at ~100Hz   │
│  to shared memory for UI and inter-deck protocol                │
└─────────────────────────────────────────────────────────────────┘
              │ ALSA/JACK write
┌─────────────▼───────────────────────────────────────────────────┐
│  DAC / Audio Hardware                                           │
│  I2S → ES9038Q2M → balanced output stage                       │
└─────────────────────────────────────────────────────────────────┘
```

---

## Threading Model

```
Thread              Priority        CPU         Responsibility
────────────────────────────────────────────────────────────────
audio_rt            FIFO:80         core 3      Audio callback, transport, pipeline
decode              FIFO:40         core 2      Read-ahead decode, seek response
analysis_pool[0-2]  normal          core 0-1    BPM, waveform, key analysis
timecode_rt         FIFO:70         core 3      DVS timecode decode (if active)
control_ipc         FIFO:50         core 2      SPI/USB event read → ControlEvent queue
ui                  normal          core 0      Display, waveform render, browsing
```

**Why two RT threads on core 3?**

The audio output callback and the timecode decode both need RT priority, but only one runs at a time — when DVS is active, the timecode decode is what drives the transport, and the audio callback reads from its output. They are pinned to the same core to share the L1/L2 cache on the decoded PCM data.

**The control_ipc thread** reads from the MCU over SPI or USB HID and writes `ControlEvent` values into a lock-free SPSC queue. The audio_rt thread drains this queue at the top of each callback. This is the only path by which user input enters the RT domain.

**Cross-thread communication:**

| From | To | Mechanism |
|---|---|---|
| decode → audio_rt | PCM samples | SPSC ring buffer (rtrb crate) |
| control_ipc → audio_rt | ControlEvents | SPSC ring buffer |
| audio_rt → decode | seek commands | SPSC ring buffer (reverse direction) |
| audio_rt → ui | EngineState | Shared AtomicU64 fields (position, speed) |
| analysis_pool → audio_rt | BeatGrid | Arc<RwLock<BeatGrid>>, written once, then read-only |
| analysis_pool → ui | WaveformCache | mmap'd file, written by analysis, read by UI |

All paths touching the RT thread are bounded: no dynamic dispatch, no allocation, no mutex.

---

## Decode and I/O Pipeline

### Format Abstraction

```rust
trait Decoder: Send {
    /// Decode the next block of PCM samples into `out`.
    /// Returns number of frames written. Returns 0 at EOF.
    fn decode(&mut self, out: &mut [f32]) -> Result<usize, DecodeError>;

    /// Seek to this sample position. Approximate is acceptable — the
    /// decoder reports actual position after seek.
    fn seek(&mut self, sample: u64) -> Result<u64, DecodeError>;

    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u8;
    fn total_samples(&self) -> Option<u64>;
}
```

Implementations:
- `SymphoniaDecoder` — handles FLAC, WAV, AIFF, MP3, AAC via the `symphonia` Rust crate (pure Rust, no unsafe FFI, good coverage)
- `MiniMp3Decoder` — fallback for edge-case MP3 streams if symphonia misses anything
- Symphonia should cover the full format matrix for v1

We do not link against libav/ffmpeg. The FFI surface area is too large, licensing is complex on static link, and symphonia covers everything we need in safe Rust.

### Read-Ahead Thread

```rust
struct DecodeThread {
    decoder: Box<dyn Decoder>,
    ring:    Producer<f32>,   // writes to audio_rt's consumer
    cmd:     Receiver<DecodeCmd>,
    pos:     u64,             // current decoded position in samples
}

enum DecodeCmd {
    Seek(u64),
    Preroll(u64),  // ensure at least N samples are buffered
    Load(PathBuf),
}
```

The ring buffer holds 4 seconds of stereo float at 44.1kHz = ~1.4 MB. The decode thread fills this whenever it has headroom. If the audio thread is playing at 2x speed, the decode thread reads 2x faster — it monitors the fill level and adjusts its decode batch size to stay 2 seconds ahead of the playback cursor.

**Seek handling:**

When a seek is needed (hot cue jump, scratch past the buffer edges), the audio thread writes a `Seek(target_sample)` command to the decode thread. The decode thread flushes the ring, seeks the decoder, and re-fills from the new position. During this gap (typically <20ms), the audio thread outputs silence or holds the last buffer. A `SeekComplete(actual_sample)` message is written back.

Hot cues within the existing ring buffer are handled without a full seek — if the target sample is within the current ring contents, the audio thread adjusts its read cursor inside the ring without involving the decode thread.

---

## Transport State Machine

The transport owns the playback position and drives the processing pipeline each callback.

```rust
enum TransportState {
    Stopped,           // output silence, position = cue point
    Playing,           // advancing at current_speed
    Cued,              // held at cue point, monitoring for release
    Scratching,        // jog velocity overrides speed
    SlipPlay,          // slip active, ghost advancing, visible at jog position
}

struct Transport {
    state:        TransportState,
    active_pos:   u64,         // samples, visible position (what you hear)
    ghost_pos:    u64,         // samples, slip ghost position
    slip_enabled: bool,

    target_speed: f32,         // 1.0 = normal, range roughly -2.0..=2.0
    current_speed: f32,        // ramped toward target_speed over a few ms
    speed_ramp:   f32,         // per-sample delta for smooth pitch changes

    cue_points:   [Option<CuePoint>; 8],
    active_loop:  Option<Loop>,
    quantize:     bool,
    beat_grid:    Option<Arc<BeatGrid>>,
}
```

**Speed ramping:**

Abrupt speed changes produce clicks. When `target_speed` changes, `current_speed` ramps over 2–4ms (at 44.1kHz, 2ms = 88 samples). This is fast enough that it feels instantaneous but eliminates the click artifact. The ramp rate is a configurable constant.

**Callback per-sample loop (simplified):**

```rust
fn process(&mut self, output: &mut [f32], ring: &mut Consumer<f32>) {
    for frame in output.chunks_mut(2) {
        // 1. Apply control events (zero or one per callback, usually)
        self.drain_events();

        // 2. Advance ghost position if slip active
        if self.slip_enabled && !matches!(self.state, Stopped) {
            self.ghost_pos += 1;
        }

        // 3. Ramp speed
        self.current_speed = ramp_toward(self.current_speed, self.target_speed, RAMP_DELTA);

        // 4. Get sample from pipeline
        let [l, r] = self.pipeline.next_frame(self.active_pos, self.current_speed, ring);

        // 5. Advance active position
        self.active_pos = self.advance_pos(self.active_pos, self.current_speed);

        // 6. Loop boundary check
        if let Some(lp) = &self.active_loop {
            if self.active_pos >= lp.out_point {
                self.active_pos = lp.in_point;
                // If slip: do NOT reset ghost_pos
            }
        }

        frame[0] = l;
        frame[1] = r;
    }
}
```

This per-sample loop is the design. Not per-buffer. Per-sample. This means loop boundaries and cue points fire at sample-accurate positions, not at buffer-granular resolution. At 256-sample buffers, buffer-granular loop points can slip by up to 5.8ms — audible on a tight loop. Per-sample processing eliminates this entirely.

The pipeline is asked for one frame at a time. The timestretch engine internally accumulates input and generates output — it does not process one sample at a time internally, but its interface can return one frame at a time by internally buffering.

---

## Pitch, Speed, and Timestretching

This is the most important quality dimension of the whole system.

### Conceptual Model

There are two independent variables the DJ controls:

| Variable | What it means | Implementation |
|---|---|---|
| **Speed** | How fast the track plays (affects timing/BPM) | Read position advances faster or slower |
| **Key** | The pitch of the audio | Resampling (without key lock) or timestretching (with key lock) |

Without key lock: speed change = pitch change. Like playing vinyl at wrong RPM. Simple resample.
With key lock: speed changes but pitch stays constant. Requires timestretching.

Additionally, the DJ can shift pitch independently (transpose key up/down in semitones). This is pure pitch shifting at constant speed.

The combinations:

```
key_lock=off, speed=1.0, key_shift=0:    pass-through (trivial)
key_lock=off, speed=1.2, key_shift=0:    resample at 1.2x (pitch goes up)
key_lock=on,  speed=1.2, key_shift=0:    timestretch (tempo up, pitch constant)
key_lock=on,  speed=1.0, key_shift=+2:   pitch shift (semitones, speed constant)
key_lock=on,  speed=1.2, key_shift=+2:   timestretch + pitch shift
```

### Pipeline Stages

```rust
trait PipelineStage: Send {
    /// Feed input samples, get output samples back.
    /// Input and output rates may differ (for timestretch/resample).
    fn push(&mut self, input: &[f32]) -> &[f32];
    fn reset(&mut self);
    fn latency_samples(&self) -> usize;
}

enum ActivePipeline {
    Passthrough,
    Resample(ResampleStage),
    Timestretch(TimestrechStage),
    TimestetchAndShift(TimestretechStage, PitchShiftStage),
}
```

Pipeline swapping happens outside the RT thread when mode changes. The new pipeline is handed to the RT thread via an atomic pointer swap (no lock needed — the old pipeline is dropped in a background thread after a safe delay).

### Resampler (No Key Lock)

Use a high-quality SRC (sample rate converter) in polyphase sinc mode. Options:

- **libsamplerate** (SRC_SINC_BEST_QUALITY): proven, BSD-licensed, Rust bindings exist
- **rubato** crate: pure Rust, excellent quality, no unsafe, zero dependencies

Use `rubato` (`SincFixedOut` mode). It takes variable amounts of input to produce a fixed output block size. This matches our per-buffer callback model perfectly. Quality is comparable to libsamplerate's best mode.

Speed ratio range: 0.5x to 2.0x (–50% to +100% tempo). Rubato handles this range cleanly.

### Timestretcher (Key Lock)

**Rubber Band Library** (LGPL 2.1) is the right choice. Rust bindings: `rubberband` crate.

Critical configuration:

```rust
let options = RubberBandOption::ProcessRealTime        // not offline
    | RubberBandOption::EngineFiner                   // R3 engine (higher quality)
    | RubberBandOption::PitchHighConsistency          // prioritize pitch stability
    | RubberBandOption::ChannelsTogether;             // stereo phase coherence

let rb = RubberBandStretcher::new(
    sample_rate,
    2,          // stereo
    options,
    1.0,        // initial time ratio
    1.0,        // initial pitch scale
);
```

**R3 vs R2 engine:**

- R3 (`EngineFiner`): higher quality, more latency (~100ms internal), better for moderate speed ranges (0.7x–1.5x). This is what we want for key-locked playback during normal DJ performance.
- R2: lower latency (~50ms), acceptable quality, better for extreme speed ratios. Used for scratch simulation if key lock stays enabled during scratch (unusual but possible).

**Latency compensation:**

Rubber Band introduces inherent latency. At startup, we pre-roll the stretcher with `latency_samples()` worth of silence so the output is synchronized with the input position. This is accounted for in the position tracking.

**On RPi 5, expected CPU cost:**

R3 stereo at 44.1kHz, ratio 1.0: ~5% of one core. At 1.2x ratio: ~8%. This is the stretched estimate — actual benchmarking on hardware required. R3 is the recommendation but R2 is the fallback if we measure >25% on one core in the worst case.

### Pitch Shift (Key Transpose)

Independent of timestretching — just change the pitch_scale parameter on the Rubber Band stretcher:

```rust
let semitones: i32 = -3;  // example: shift down 3 semitones
let pitch_scale = 2.0_f64.powf(semitones as f64 / 12.0);
rb.set_pitch_scale(pitch_scale);
```

Rubber Band handles simultaneous time ratio and pitch scale changes cleanly.

### Scratch Mode

When the jog wheel is touched and moved, `target_speed` is driven by jog velocity. The jog velocity arrives from the MCU as delta encoder counts per millisecond, converted to a speed ratio.

If key lock is **off**: scratch uses the resampler at variable ratio → authentic vinyl sound.
If key lock is **on**: scratch uses the timestretcher at variable ratio → pitch-locked scratch (less natural). Most DJs disable key lock during scratch. This should be the automatic behavior: key lock is suspended while jog touch is active, restored on release.

```rust
fn on_jog_touch(&mut self) {
    self.key_lock_suspended = self.key_lock_enabled;
    if self.key_lock_suspended {
        self.swap_pipeline(ActivePipeline::Resample(...));
    }
}

fn on_jog_release(&mut self) {
    if self.key_lock_suspended {
        self.swap_pipeline(ActivePipeline::Timestretch(...));
        self.key_lock_suspended = false;
    }
}
```

---

## Slip Mode

Slip mode (Pioneer calls it "Slip", Traktor calls it "Flux") maintains two positions:

- `active_pos`: what the listener hears, manipulated freely
- `ghost_pos`: the "shadow" position, always advances at 1x playback speed

When slip mode is enabled, `ghost_pos` starts advancing independently. When the DJ releases a scratch, exits a loop, or releases a hot cue hold, the output crossfades from `active_pos` to `ghost_pos`.

### Implementation

```rust
struct SlipState {
    ghost_pos:     u64,          // advances at 1x always
    enabled:       bool,
    crossfade:     Option<CrossfadeState>,
}

struct CrossfadeState {
    from_pos:      u64,          // where we're fading from
    to_pos:        u64,          // ghost_pos at moment of release
    sample_index:  usize,        // how far through the crossfade
    length:        usize,        // total crossfade length (10ms = 441 samples)
}
```

**Crossfade on release:**

```rust
fn slip_release(&mut self) {
    if let Some(slip) = &self.slip {
        if slip.enabled {
            self.slip_state.crossfade = Some(CrossfadeState {
                from_pos:     self.active_pos,
                to_pos:       slip.ghost_pos,
                sample_index: 0,
                length:       (SAMPLE_RATE * CROSSFADE_MS / 1000) as usize,
            });
        }
    }
}

fn apply_crossfade(&mut self, xf: &mut CrossfadeState, ring: &Consumer<f32>) -> [f32; 2] {
    let t = xf.sample_index as f32 / xf.length as f32;
    let t = smooth_step(t);  // cubic ease-in-out

    let from = self.read_at(xf.from_pos, ring);
    let to   = self.read_at(xf.to_pos, ring);

    xf.sample_index += 1;
    if xf.sample_index >= xf.length {
        self.active_pos = xf.to_pos;  // snap complete
        self.slip_state.crossfade = None;
    } else {
        xf.from_pos = self.advance(xf.from_pos, 1.0);
        xf.to_pos   = self.advance(xf.to_pos, 1.0);
    }

    [from[0] * (1.0 - t) + to[0] * t,
     from[1] * (1.0 - t) + to[1] * t]
}
```

**Slip + loops:**

Inside a slip loop, `active_pos` bounces between loop points. `ghost_pos` passes through them and continues forward. On loop exit (with slip enabled), the active position jumps to ghost — the loop plays as long as held, then the track resumes from where it would have been. This is the correct Pioneer CDJ behavior.

**Slip + hot cues:**

Pressing a hot cue while slip is enabled: `active_pos` jumps to cue. `ghost_pos` keeps advancing. Release the pad: crossfade back to ghost. This enables sample-trigger style performance while the track continues behind.

---

## Hot Cues and Loops

### Cue Point Data

```rust
struct CuePoint {
    index:    u8,           // 0–7
    position: u64,          // sample offset from track start
    color:    Rgb,          // for LED and UI
    label:    String,       // max 32 chars
    kind:     CueKind,
}

enum CueKind {
    HotCue,
    LoopIn,             // part of a saved loop pair
    FadeIn,             // for auto-mix (future)
    FadeOut,
}

struct SavedLoop {
    index:    u8,
    in_pt:    u64,
    out_pt:   u64,
    label:    String,
    active:   bool,
}
```

### Cue Behavior

**Hot cue set:** Press unlit pad → stores current position. If playing, position quantizes to nearest beat if quantize mode on.

**Hot cue trigger (stopped):** Jump to cue point, remain stopped at cue position (cue preview mode: hold pad to play from cue, release to return).

**Hot cue trigger (playing):** Jump to cue point, continue playing. With slip enabled: resume from ghost on release.

**Hot cue delete:** Hold SHIFT + pad.

### Loop Engine

```rust
struct LoopEngine {
    active:   Option<ActiveLoop>,
    saved:    [Option<SavedLoop>; 8],
    beat_grid: Option<Arc<BeatGrid>>,
    quantize:  bool,
}

struct ActiveLoop {
    in_pt:  u64,
    out_pt: u64,
    kind:   LoopKind,
}

enum LoopKind {
    Manual,              // set by in/out button
    Beat(f32),           // beat-length loop (0.0625 = 1/16, 1.0 = 1 bar, etc.)
    Roll,                // loop roll: slip stays active, loop plays, release → snap to ghost
}
```

**Loop roll** is just slip mode + an active loop simultaneously. When the DJ holds the loop roll button, a loop activates and slip engages. On release, the loop deactivates and slip release crossfade fires. No special casing needed in the transport — these two mechanisms compose naturally.

**Beat loops:**

Given a `BeatGrid`, compute in/out points:

```rust
fn quantize_to_beats(&self, pos: u64, length_beats: f32) -> (u64, u64) {
    let grid = self.beat_grid.as_ref().unwrap();
    let nearest_beat = grid.nearest_beat_before(pos);
    let beat_samples = grid.samples_per_beat_at(nearest_beat);
    let out = nearest_beat + (beat_samples as f32 * length_beats) as u64;
    (nearest_beat, out)
}
```

Variable BPM grids (live recordings with human tempo drift) require the grid to provide `samples_per_beat_at(beat_index)` rather than a single global BPM value.

---

## Vinyl Control / DVS

DVS (Digital Vinyl System) decodes a timecode signal from a record or CD playing on a real turntable or CD deck and uses the encoded position/velocity information to control virtual playback.

The `TimecodeDecoder` trait is the interface — implementations handle each supported timecode format:

```rust
trait TimecodeDecoder: Send {
    /// Process one block of stereo input (timecode audio from line-in).
    fn process(&mut self, left: &[f32], right: &[f32]) -> TimecodeOutput;
    fn reset(&mut self);
}

struct TimecodeOutput {
    /// Normalized speed: 1.0 = forward at reference speed, -1.0 = reverse.
    /// 0.0 = stationary. Range typically -2.0..=2.0.
    speed:      f32,

    /// Absolute position in the timecode signal's frame, if the format supports it.
    /// None for relative-only formats or when signal is lost.
    position:   Option<f64>,   // seconds into the timecode record/CD

    /// 0.0–1.0 signal quality. Below ~0.3, output should be ignored.
    confidence: f32,

    direction:  Direction,  // Forward, Reverse, Stationary
}
```

When DVS is active, the timecode decoder runs at RT priority. `TimecodeOutput.speed` replaces `target_speed` in the transport. When `position` is `Some`, the transport can additionally snap the track position to the corresponding virtual position (absolute mode).

### Supported Timecode Formats

We target the formats DJs already own vinyl and CDs for:

| Format | Type | Notes |
|---|---|---|
| Serato CV02.5 | Absolute | The current Serato standard, 2500Hz carrier |
| Serato 2.5 (old) | Absolute | 1kHz carrier, older pressings |
| Traktor Scratch MK2 | Absolute | 2kHz carrier |
| Mixvibes DVS | Absolute | 2kHz carrier |
| Pioneer RB-VS1-K | Absolute | rekordbox DVS vinyl |

**Reference implementation: xwax**

The open-source `xwax` project (GPL v2, Mark Hills) has clean, well-understood C implementations of all the above decoders. The decoding logic is ~500 lines per format — straightforward to port to Rust or call via FFI. The xwax source is the definitive public reference for how each of these signals works.

The timecode decoding math is not proprietary — it is signal processing on an audio signal. The vinyl pressings themselves are owned by their respective companies; we decode the signal, we do not press or distribute the vinyl.

### How Absolute Timecode Works (general)

All absolute timecode formats use a variation of the same technique:

1. **Pilot tone / carrier**: a constant-frequency sine wave (1–4kHz depending on format) on both channels in quadrature (90° phase offset). Instantaneous frequency deviation = speed. Phase relationship L vs R = direction.

2. **Position encoding**: a pseudo-random binary sequence (typically a maximal-length sequence / LFSR) is amplitude-modulated or phase-modulated onto the carrier at a lower frequency (~500Hz bit rate). The current position in the LFSR sequence = absolute position on the record. A 12-bit LFSR gives 4095 unique positions × bit period = ~8 minutes of unique absolute addressing.

3. **Decoding pipeline**:
   ```
   stereo line-in → bandpass filter at carrier freq →
       quadrature demodulate (I/Q) →
           phase comparator → direction + speed
           AM envelope → bit slicer → LFSR correlator → absolute position
   ```

### Decoder Implementation Strategy

Port xwax's timecode detection to Rust. The core loop is:

```rust
struct TimecodeFormat {
    carrier_freq:    f32,    // Hz
    bits_per_second: f32,    // bit rate of position encoding
    lfsr_taps:       u32,    // LFSR polynomial for this format
    lfsr_length:     u32,    // sequence length (2^n - 1)
}

// Example: Serato CV02.5
const SERATO_2500: TimecodeFormat = TimecodeFormat {
    carrier_freq:    2500.0,
    bits_per_second: 1000.0,
    lfsr_taps:       0x...,  // from xwax source
    lfsr_length:     4095,
};
```

The confidence value is derived from the bandpass filter output amplitude. When the needle is lifted or the signal is noisy, confidence drops below threshold and the transport holds last known state (doesn't drift randomly).

### Two DVS Modes (user-selectable)

**Relative mode**: only speed and direction are used. Absolute position is ignored. Scratching and pitch control work perfectly. Desync can happen if the needle skips or the vinyl is cued to a different position. Matches how most experienced DVS users work.

**Absolute mode**: position is tracked. Cueing the vinyl to any position maps that physical groove position to the corresponding virtual position. No desync possible. Requires a clean, strong signal.

Both modes use the same decoder — absolute mode just additionally acts on the `position` field of `TimecodeOutput`.

---

## Beat Analysis Pipeline

### Design Goal

Same code, two modes:

- **Offline:** feed the entire decoded track into the analyzer, get back a `BeatGrid`
- **Online:** feed samples as they arrive from the decode thread, get a `BeatGrid` that refines itself

```rust
trait BeatAnalyzer: Send {
    /// Feed a block of mono samples (downmixed from stereo).
    fn push(&mut self, samples: &[f32], sample_rate: u32);

    /// Get the current best estimate of the beat grid.
    /// Returns None until enough data has been processed.
    fn beat_grid(&self) -> Option<Arc<BeatGrid>>;

    /// True if the analyzer is confident enough to use the grid for quantize.
    fn is_stable(&self) -> bool;
}
```

The offline path just calls `push()` in a tight loop with the full decoded track, then calls `beat_grid()`. The online path is called incrementally from the analysis thread pool as the decode thread produces samples.

This is a critical architectural decision: **the analyzer does not know or care whether it's online or offline.** The caller manages chunking.

### Algorithm Stack

Beat analysis quality determines whether hot cue quantize and sync feel natural or sloppy.

**Stage 1: Onset Strength Signal**

```
PCM → bandpass filter (80Hz–400Hz, bass focus) → half-wave rectify →
    smooth with 20ms Hann window → differentiate → half-wave rectify again
    → onset strength signal
```

Bass-focused onset detection works well for electronic music. We also run a broadband onset detector in parallel and weight the two. For tracks with no strong bass (acoustic guitar, some hip-hop), the broadband detector takes precedence.

**Stage 2: Tempo Induction**

Apply autocorrelation to the onset strength signal over a 6-second window. The peak in the autocorrelation (in the range corresponding to 60–180 BPM) is the tempo estimate.

This is the same method used by librosa's `beat_track()` and the original BeatRoot paper. It's robust, well-understood, and fast to compute.

**Stage 3: Beat Phase Estimation**

Once we have a tempo period T (in samples), find the phase that maximizes the sum of onset strengths at positions {phase, phase+T, phase+2T, ...}. This is the beat grid anchor.

**Stage 4: Grid Refinement (Variable BPM)**

For live recordings, the BPM drifts. We compute a "warped grid" by:
1. Marking the strongest onset near each expected beat position (within ±15% of the period)
2. Fitting a smoothed tempo curve over the track

For DJ-oriented music (electronic, hip-hop, most club music), BPM is essentially constant. Detect and skip the variable-tempo processing for these tracks.

**Stage 5: Downbeat Detection**

Find the bar structure (group beats into 4-beat bars). Useful for beat jump (4-beat), loop quantize to bar, and visual grid display.

Autocorrelation at the beat-level signal at a period of 4 beats. The dominant phase is the downbeat.

**Online mode accuracy timeline:**

| Data fed | Accuracy |
|---|---|
| 2 seconds | BPM estimate ±5 BPM, unreliable phase |
| 5 seconds | BPM estimate ±1 BPM, rough phase |
| 15 seconds | BPM ±0.1 BPM, usable beat grid |
| 30+ seconds | Full confidence, downbeat detected |

For tracks not yet in the library, the grid becomes usable for quantize within about 15 seconds of loading. The grid is locked (marked stable) and stored to the database after full-track analysis completes in the background.

### BeatGrid Data Structure

```rust
struct BeatGrid {
    /// Sample offset of the first beat (beat 0)
    anchor_sample:  u64,

    /// Constant BPM (for fixed-tempo tracks)
    bpm:            f64,

    /// For variable-tempo tracks: list of (beat_index, sample_offset) pairs.
    /// Empty = use constant BPM from anchor.
    beats:          Vec<u64>,  // sample offsets of each detected beat

    /// 0-indexed position within the bar (0 = downbeat)
    downbeat_offset: u8,

    /// 0.0–1.0 confidence
    confidence:     f32,

    /// Whether this grid was human-verified/edited
    locked:         bool,
}

impl BeatGrid {
    fn sample_of_beat(&self, beat_index: i64) -> u64 { ... }
    fn beat_at_sample(&self, sample: u64) -> f64 { ... }  // fractional beat number
    fn phase_at_sample(&self, sample: u64) -> f32 { ... } // 0.0–1.0
    fn nearest_beat_before(&self, sample: u64) -> u64 { ... }
    fn samples_per_beat_at(&self, sample: u64) -> f64 { ... }
}
```

---

## Waveform Data

The waveform display has two views:

1. **Overview waveform:** full track compressed to ~1800 pixels wide, always visible
2. **Zoomed waveform:** scrolling view around playhead, ~10 seconds visible at a time

Both are computed from the same pre-analyzed data.

### Color Encoding

CDJ-3000-style frequency-mapped coloring: each column represents one time window, colored by spectral content.

```
Frequency bands → color:
  20–200 Hz   (sub/bass):      red    (R channel)
  200–2000 Hz (mid):           green  (G channel)
  2kHz–20kHz  (high):          blue   (B channel)
```

Each color channel's brightness = RMS energy in that band for the window.

This produces visually informative waveforms: drum kicks appear red, snares appear white/bright, hi-hats appear blue, pads appear green.

### Pre-computation

At analysis time, we run an STFT over the full decoded track:

```
Window size:  2048 samples (46ms @ 44.1kHz)
Hop size:     512 samples (11.6ms per column) — ~86 columns/second
```

For each hop:
- Compute FFT (2048 point)
- Sum |X[k]|² in bass/mid/high bins
- Take sqrt (RMS) and normalize
- Store as [r, g, b] bytes

Total storage for a 6-minute track:
```
6 min × 60 sec × 86 cols/sec × 3 bytes = ~93 KB
```

This is stored as a flat binary file alongside the database entry (or as a BLOB). Loading it is a single mmap call.

### Zoomed Waveform Rendering

The UI reads the pre-computed waveform array and renders the region around the playhead at the current zoom level. The playhead position comes from the `EngineState` shared memory.

**No rendering work happens in the audio thread.** The UI reads the atomic position value at 60fps and updates the scroll offset accordingly.

Zoomed waveform can also show:
- Beat grid lines (computed from `BeatGrid`, painted as vertical lines at beat positions)
- Cue point markers (colored by cue color)
- Loop region (shaded region between in/out points)
- Ghost position indicator (when slip is active)
- Beat drift overlay (from inter-deck protocol, see below)

---

## Inter-Deck Beat Grid Protocol

One deck receives the other's beat clock to display beat drift visually and optionally to sync. The internal representation is an `EngineState` struct. Multiple sources can produce this struct — a local second deck instance, or a ProDJ Link bridge reading from a Pioneer CDJ on the network.

### Published State (EngineState)

The audio engine publishes this to shared memory at ~100Hz:

```rust
#[repr(C)]
struct EngineState {
    /// Current play position in samples
    position:     u64,

    /// Playback speed × 100000 (e.g., 100000 = 1.0x)
    speed_fixed:  u32,

    /// Current BPM × 100 (e.g., 12800 = 128.00 BPM)
    bpm_fixed:    u32,

    /// Beat phase 0–65535 (0.0–1.0 within current beat)
    beat_phase:   u16,

    /// Bar phase 0–65535 (0.0–1.0 across 4 beats)
    bar_phase:    u16,

    is_playing:   bool,
    deck_id:      u8,

    /// Monotonic wall-clock timestamp of this update (nanoseconds)
    timestamp_ns: u64,
}
```

### Receiving Deck — Drift Visualization

```
drift_samples = (local_beat_phase - remote.beat_phase) × local_samples_per_beat
drift_ms = drift_samples / sample_rate × 1000
```

Displayed as a colored horizontal ribbon at the playhead line on the waveform: offset indicates how far ahead or behind the remote beat falls relative to local. Green = in sync, red/yellow = drifting, magnitude = ms offset.

### ProDJ Link (Pioneer CDJ compatibility)

The ProDJ Link protocol is extensively documented by the `dysentery` reverse-engineering project (James Elliott, https://djl-analysis.deepsymmetry.org/). The `beat-link` Java library is a complete working implementation. We implement a native Rust bridge.

**What ProDJ Link gives us:**

- Beat clock packets broadcast by any Pioneer CDJ/XDJ on the LAN (UDP, port 50001)
- Absolute track position and BPM from any networked deck
- Track metadata and artwork (rekordbox database protocol over TCP)
- Master/slave sync role negotiation
- We appear on the network as a valid CDJ player (player number 1–4)

**ProDJ Link beat packet (0x28 type):**

Pioneer CDJs broadcast a beat announcement packet at every beat. Fields relevant to us:

```
Offset  Length  Content
0x00    4       Magic: 51 73 70 74 ("Qspt")
0x04    1       Packet type: 0x28
0x05    10      Device name (null-padded)
0x0F    1       0x00
0x10    1       Device number (1–4)
0x11    1       Packet length (low byte)
0x14    4       Next beat number
0x18    4       2nd beat number
0x1C    4       Next bar beat number
0x20    4       Beat number within bar (1–4)
0x24    4       BPM × 100 (big-endian)
0x28    1       Pitch (actually in a separate status packet)
```

The beat packet arrives at each beat onset. We interpolate between packets using the BPM and local wall clock to derive a smooth `beat_phase` value between packets.

**ProDJ Link status packet (0x0A type)** — sent at ~8Hz per device:

Contains: player number, track number, track position (in ms), playback state, BPM, pitch, sync state, master state. This is richer than the beat packet and sufficient for continuous drift display without interpolation.

**Appearing as a CDJ on the network:**

To receive metadata and participate in sync, we must announce ourselves as a player:

```
Device announce packet (0x06):
  - Player number: 1–4 (negotiated, avoid collision with real CDJs)
  - Device name: "OpenDeck"
  - IP + MAC address
  Broadcast every 1.5 seconds on UDP port 50000
```

Once announced, Pioneer CDJs and mixers (DJM-900NXS2, etc.) recognize us as a peer and include us in sync negotiation. We can be set as sync master or slave.

**rekordbox database access (track metadata over ProDJ Link):**

Pioneer CDJs share their rekordbox library over TCP (port 1051). The dysentery project documents the binary protocol. We can read track title, artist, BPM, key, waveform data, and beat grids directly from a networked CDJ's exported USB library — meaning our waveform display can mirror the rekordbox waveform exactly when playing from a Pioneer's USB.

**Implementation approach:**

```rust
struct ProDjLinkBridge {
    socket:      UdpSocket,
    player_num:  u8,
    peers:       HashMap<u8, PeerState>,
}

impl ProDjLinkBridge {
    /// Returns an EngineState for each active peer deck, updated continuously.
    fn poll(&mut self) -> Vec<(u8, EngineState)> { ... }

    /// Broadcast our own state so Pioneer mixers see us.
    fn announce(&self, our_state: &EngineState) { ... }
}
```

The bridge runs in a non-RT network thread. It publishes `EngineState` values via the same shared memory mechanism used by local decks. The drift visualization and sync engine see no difference between a local deck and a Pioneer CDJ across Ethernet.

**Ableton Link:**

Ableton Link (Apache 2.0, official SDK) provides tempo and phase sync across a LAN with no master/slave hierarchy. Less DJ-specific than ProDJ Link (no track position, no metadata) but universally supported by DAWs, drum machines, and many DJ apps.

We implement both. ProDJ Link is the primary protocol for Pioneer-compatible setups; Ableton Link covers everything else (Ableton Live, Native Instruments, Roland gear, Teenage Engineering, etc.).

---

## Library and Browsing

This is explicitly secondary to the audio engine but must be solid enough that it never impacts playback.

### Architecture

All library operations run in a separate process (or at minimum a non-RT thread pool) that is isolated from the audio engine. SQLite WAL mode prevents any library write from blocking a read on the audio thread (which only reads cue point data at load time — never during playback).

### Schema (key tables)

```sql
CREATE TABLE tracks (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    file_hash   BLOB NOT NULL,           -- SHA-256 of first 64KB + file size
    title       TEXT,
    artist      TEXT,
    album       TEXT,
    duration_samples INTEGER,
    sample_rate INTEGER,
    bpm         REAL,
    key         TEXT,                    -- Camelot notation, e.g. "8A"
    analyzed_at INTEGER,                 -- unix timestamp
    play_count  INTEGER DEFAULT 0,
    last_played INTEGER
);

CREATE TABLE beat_grids (
    track_id    INTEGER PRIMARY KEY REFERENCES tracks(id),
    anchor_sample INTEGER NOT NULL,
    bpm         REAL NOT NULL,
    beats_blob  BLOB,                   -- serialized Vec<u64> for variable tempo
    downbeat_offset INTEGER DEFAULT 0,
    confidence  REAL,
    locked      INTEGER DEFAULT 0
);

CREATE TABLE cue_points (
    id          INTEGER PRIMARY KEY,
    track_id    INTEGER REFERENCES tracks(id),
    slot        INTEGER NOT NULL,       -- 0–7
    position    INTEGER NOT NULL,       -- sample offset
    color       INTEGER,               -- RGB packed as u32
    label       TEXT,
    kind        TEXT NOT NULL          -- 'hot', 'memory', 'loop_in', 'loop_out'
);

CREATE TABLE saved_loops (
    id          INTEGER PRIMARY KEY,
    track_id    INTEGER REFERENCES tracks(id),
    slot        INTEGER NOT NULL,
    in_pt       INTEGER NOT NULL,
    out_pt      INTEGER NOT NULL,
    label       TEXT
);

CREATE TABLE playlists (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    parent_id   INTEGER REFERENCES playlists(id),  -- for folder nesting
    position    INTEGER                             -- sort order
);

CREATE TABLE playlist_tracks (
    playlist_id INTEGER REFERENCES playlists(id),
    track_id    INTEGER REFERENCES tracks(id),
    position    INTEGER,
    PRIMARY KEY (playlist_id, track_id)
);
```

### File Discovery and Hashing

The library scanner walks USB/SD mount points. File identity is by hash of first 64KB + file size (not path) — this means moving or renaming a file on the USB stick is handled gracefully; cue points survive.

The full SHA-256 of the file is not computed — first-64KB+size is fast and collision-resistant enough for music libraries.

### Track Loading Sequence

```
1. User selects track in browser UI
2. Library process queries DB: get path, cue points, beat grid, BPM, key
3. Audio engine receives LoadTrack(TrackInfo) command
4. Decode thread opens file, begins read-ahead into ring buffer
5. If beat grid exists in DB and is locked: use immediately
6. If beat grid not in DB (or not locked): start online analysis in parallel
7. Waveform cache file loaded (mmap): if missing, start WaveformBuilder in background
8. Transport transitions to Cued state, position = 0 (or last cue)
9. UI displays waveform immediately (from cache) or incrementally (being built)
```

Load-to-cue time target: <500ms for a track already in the library on a fast SD card/USB3 stick.

### Browsing UI Requirements (minimal v1)

- File tree view: USB / SD / internal storage, folder navigation
- Playlist view: flat list, sortable by title/artist/BPM/key
- Search: incremental text search over title/artist, results as you type (SQLite FTS5)
- Track info panel: BPM, key, duration, play count, waveform thumbnail
- No track management (no delete, no move) — the deck is read-only relative to the media

---

## Data Structures and Interfaces

Summary of the core public interfaces — these are the contracts between components.

```rust
// ── Decoder ──────────────────────────────────────────────────────────────────
trait Decoder: Send {
    fn decode(&mut self, out: &mut [f32]) -> Result<usize, DecodeError>;
    fn seek(&mut self, sample: u64) -> Result<u64, DecodeError>;
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u8;
    fn total_samples(&self) -> Option<u64>;
}

// ── Beat Analyzer ────────────────────────────────────────────────────────────
trait BeatAnalyzer: Send {
    fn push(&mut self, samples: &[f32], sample_rate: u32);
    fn beat_grid(&self) -> Option<Arc<BeatGrid>>;
    fn is_stable(&self) -> bool;
}

// ── Timecode Decoder ─────────────────────────────────────────────────────────
trait TimecodeDecoder: Send {
    fn process(&mut self, left: &[f32], right: &[f32]) -> TimecodeOutput;
    fn reset(&mut self);
}

// ── Pipeline Stage ────────────────────────────────────────────────────────────
trait PipelineStage: Send {
    fn push(&mut self, input: &[f32]) -> &[f32];
    fn reset(&mut self);
    fn latency_samples(&self) -> usize;
}

// ── Engine → UI state (shared memory, read by UI at 60fps) ──────────────────
#[repr(C)]
struct EngineState { ... }  // as defined in Inter-Deck section

// ── Control events (MCU → audio engine, lock-free queue) ─────────────────────
enum ControlEvent {
    JogDelta     { delta: i32, velocity_rpm: f32 },
    JogTouch     { touched: bool },
    PlayPause,
    Cue,
    HotCue       { slot: u8, held: bool },
    HotCueSet    { slot: u8 },
    HotCueDelete { slot: u8 },
    LoopIn,
    LoopOut,
    LoopToggle,
    BeatLoop     { beats: f32 },  // 0.5, 1.0, 2.0, 4.0, ...
    LoopRoll     { beats: f32, held: bool },
    SlipToggle,
    KeyLockToggle,
    PitchBend    { value: f32 },  // -1.0..=1.0
    TempoFader   { position: f32 }, // 0.0..=1.0, maps to ±8% or ±100%
    KeyShift     { semitones: i8 },
    NeedleSearch { position: f32 }, // 0.0..=1.0 (absolute track position)
    Load         { track: TrackInfo },
    Eject,
}
```

---

*Next: define the MCU firmware protocol and the SPI/USB transport packet format, then begin the Rust workspace skeleton.*
