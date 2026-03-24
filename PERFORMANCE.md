# Performance Analysis

## Profiling setup

```
cargo build --release -p opendeck-app   # release + debug = 1 (line-level symbols)
sudo sysctl kernel.perf_event_paranoid=1
cargo flamegraph --bin opendeck -- <track.mp3>
```

Generate flamegraph from an existing `perf.data`:

```
perf script | inferno-collapse-perf | inferno-flamegraph > flamegraph.svg
```

## Thread model

| Thread | Name in perf | Role |
|--------|-------------|------|
| Main | `opendeck` | winit event loop, egui layout, wgpu render |
| Audio processor | `audio-proc` | RubberBand R3 timestretch → ring buffer fill |
| Audio device I/O | `data-loop.0` | cpal/PipeWire callback, ring buffer drain → device |
| Clipboard daemon | `smithay-clipboa` | Separate subprocess spawned by egui-winit for Wayland clipboard; not our code |

## March 2026 baseline (post-fix)

Profiled on Linux/Wayland with a 5-minute 44.1 kHz stereo MP3 at 1× speed.
64 497 samples, `cpu_core/cycles` event.

### Top CPU consumers (opendeck process)

| Overhead | Symbol | Notes |
|----------|--------|-------|
| 1.86% | `pthread_mutex_lock` | Mutex contention inside wgpu Vulkan layer |
| 1.73% | `egui::context::Context::create_widget` | egui layout pass, runs every frame |
| 1.65% | `__memmove_avx_unaligned_erms` | egui tessellation memory copies |
| 1.37% | `wgpu_core::queue_submit` | GPU command submission, once per frame |

Nothing above 2%. No single hotspot. RubberBand, `processor_loop`, and
`nanosleep`/`clock_nanosleep` are **absent** from the profile — the audio
thread spends its time in OS sleep, not on-CPU.

### Root cause of earlier high CPU (now fixed)

The render loop was unbounded: `ControlFlow::Poll` + `request_redraw()` in
`about_to_wait` caused the GPU to render as fast as possible (easily 1 000+
fps), pegging one CPU core at ~100%.

**Fix:** switched to `ControlFlow::WaitUntil(last_render + 16.67 ms)` in
`about_to_wait`, capping the render thread to ~60 fps and letting the OS sleep
it between frames.

The audio processor thread was also sleeping only 1 ms between back-pressure
checks. **Fix:** sleep scales proportionally with ring buffer fill level (up to
8 ms when the buffer is nearly full), so the processor thread wakes only as
often as needed to stay ahead of the device callback.

## Known remaining overhead

- **egui runs every frame** even when playback position hasn't changed and
  there is no user input. Could be optimised by skipping the egui pass on
  frames with no state change, but the gain is marginal at 60 fps.
- **wgpu submit per frame** is unavoidable for a continuously scrolling
  waveform.
- **Wayland compositor** and **Vulkan driver** overhead are outside our
  control and account for some of the mutex contention visible in the profile.
