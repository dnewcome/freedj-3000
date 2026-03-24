# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Previously working (documented here for completeness)
- **Key lock / timestretching**: pitch-preserving speed change via Rubber Band R3
  (`crates/timestretch/`), active across the full ±16% pitch range.

### Added
- **ProDJ Link listener** (`crates/app/src/prodj.rs`): UDP listener on port 50002
  receives Pioneer CDJ/XDJ beat packets and drives the second beat grid in real
  time. Uses `socket2` with `SO_REUSEADDR`/`SO_REUSEPORT` so the port can be
  shared with other ProDJ Link tools. Falls back gracefully if the port is
  unavailable.
- **`tools/send_beat.py`**: test utility that sends fake ProDJ Link beat packets
  at a configurable BPM to a configurable host:port, for single-machine testing
  without real Pioneer hardware.
- **`fader_speed` atomic** (`Arc<AtomicU32>`): stable pitch-fader speed, separate
  from the instantaneous playback speed that includes jog nudges. Written by the
  MIDI handler when the pitch fader or pitch-increment buttons are used; read by
  the renderer for beat grid scaling.

### Fixed
- **Second beat grid (B2) scroll velocity**: the B2 strip was always animating at
  1× wall-clock rate while the audio beat markers scroll at `fader_speed ×`
  wall-clock rate, causing continuous phase drift whenever the pitch fader was
  not at centre. Fixed by scaling `beat2_period_cols` by `fader_speed` so both
  grids scroll at the same visual velocity when beatmatched.
- **Jog-nudge interference with B2 strip**: after the velocity fix was first
  implemented using instantaneous `speed`, jogging the local deck temporarily
  changed the B2 strip density and caused it to snap back when the nudge
  released. Fixed by using `fader_speed` (stable, no jog component) instead of
  `speed` for the B2 period scaling.
- **Beat grid density mismatch at non-unity speed**: the audio beat grid period
  was computed from the raw MiniBPM-detected BPM while the B2 strip used the
  incoming CDJ BPM; at matching effective tempos (e.g. local deck slowed from
  135 → 130 BPM to match an incoming 130 BPM CDJ) the grids had different pixel
  densities. Fixed by scaling `beat2_period_cols` by `fader_speed`.
- **`send_beat.py` timing drift**: the original `time.sleep(interval)` loop
  accumulated jitter because each sleep fires slightly late. Switched to
  sleeping until an absolute `monotonic` deadline so errors are corrected on the
  next iteration rather than accumulating.
- **B2 strip visibility**: the 20 px strip was barely distinguishable from the
  background (fill colour `0x03, 0x03, 0x07` vs background `0x04, 0x04, 0x04`),
  and 1 px markers were easy to miss. Increased strip height to 40 px, widened
  markers to 3 px, and changed fill to a distinct dark-blue `(0.0, 0.05, 0.15)`.
- **BPM change logging in renderer**: added a one-time log line when `beat2_bpm`
  changes inside `render_frame`, confirming ProDJ data reaches the renderer.
