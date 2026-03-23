use std::sync::{atomic::Ordering, Arc};

use opendeck_protocol::ControlEvent;
use opendeck_types::{BeatGrid, CueMap, EngineState, TrackInfo};
use rtrb::{Consumer, Producer};

use crate::{LoopEngine, SlipState};

/// Speed ramp rate: reach target speed within ~3ms at 44.1kHz.
const SPEED_RAMP_RATE: f32 = 1.0 / (44_100.0 * 0.003);

/// Minimum signal magnitude to treat as non-zero speed.
const SPEED_DEADZONE: f32 = 0.001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// No track loaded, outputting silence.
    Empty,
    /// Track loaded, held at cue position.
    Stopped,
    /// Playing forward or in reverse.
    Playing,
    /// Jog wheel held, velocity overrides speed.
    Scratching,
}

/// Commands sent from the decode thread back to the transport.
pub enum DecodeEvent {
    SeekComplete { actual_sample: u64 },
    BufferUnderrun,
}

/// Commands sent from the transport to the decode thread.
pub enum DecodeCmd {
    Seek(u64),
    Load(TrackInfo),
    Eject,
}

pub struct Transport {
    // ── State ─────────────────────────────────────────────────────────────────
    pub state:         TransportState,
    pub active_pos:    u64,    // what the listener hears
    pub current_speed: f32,    // ramped actual speed
    pub target_speed:  f32,    // where we're ramping to

    // ── Track metadata ────────────────────────────────────────────────────────
    pub track:         Option<TrackInfo>,
    pub sample_rate:   u32,

    // ── Sub-engines ──────────────────────────────────────────────────────────
    pub slip:          SlipState,
    pub loops:         LoopEngine,

    // ── Mode flags ───────────────────────────────────────────────────────────
    pub key_lock:            bool,
    pub key_lock_suspended:  bool,  // auto-suspended during jog scratch
    pub quantize:            bool,

    // ── Beat grid ────────────────────────────────────────────────────────────
    pub beat_grid:     Option<Arc<BeatGrid>>,

    // ── Cue points ────────────────────────────────────────────────────────────
    pub cues:          CueMap,
    pub cue_position:  u64,    // the main CUE point (not a hot cue)

    // ── Shared state published to UI ─────────────────────────────────────────
    pub engine_state:  Arc<EngineState>,

    // ── IPC channels ─────────────────────────────────────────────────────────
    pub ctrl_rx:       Consumer<ControlEvent>,
    pub decode_tx:     Producer<DecodeCmd>,
    pub decode_rx:     Consumer<DecodeEvent>,
}

impl Transport {
    /// Called once per audio callback.  Must never allocate or block.
    ///
    /// `pcm_ring`: the pre-decoded PCM ring buffer filled by the decode thread.
    /// `output`:   interleaved stereo f32 output buffer to fill.
    pub fn process(&mut self, pcm_ring: &mut Consumer<f32>, output: &mut [f32]) {
        // 1. Drain control events (bounded — at most a handful per callback).
        while let Ok(ev) = self.ctrl_rx.pop() {
            self.handle_event(ev);
        }

        // 2. Drain decode events.
        while let Ok(ev) = self.decode_rx.pop() {
            self.handle_decode_event(ev);
        }

        // 3. Fill output sample by sample.
        let frames = output.len() / 2;
        for i in 0..frames {
            // Ramp speed toward target.
            self.current_speed = ramp_toward(
                self.current_speed,
                self.target_speed,
                SPEED_RAMP_RATE,
            );

            // Advance ghost position at 1× when slip is active.
            if self.slip.enabled && self.state != TransportState::Stopped {
                self.slip.advance_ghost();
            }

            // Handle crossfade on slip release.
            let [l, r] = if let Some(ref mut xf) = self.slip.crossfade {
                let from = self.read_frame_at(xf.from_pos, pcm_ring);
                let to   = self.read_frame_at(xf.to_pos,   pcm_ring);
                let t    = smooth_step(xf.sample_index as f32 / xf.length as f32);
                xf.from_pos = self.advance(xf.from_pos, 1.0);
                xf.to_pos   = self.advance(xf.to_pos,   1.0);
                xf.sample_index += 1;
                if xf.sample_index >= xf.length {
                    self.active_pos = xf.to_pos;
                    self.slip.crossfade = None;
                }
                [lerp(from[0], to[0], t), lerp(from[1], to[1], t)]
            } else {
                match self.state {
                    TransportState::Empty | TransportState::Stopped => [0.0, 0.0],
                    TransportState::Playing | TransportState::Scratching => {
                        let frame = self.read_frame_at(self.active_pos, pcm_ring);
                        self.active_pos = self.advance(self.active_pos, self.current_speed);
                        self.check_loop_boundary();
                        frame
                    }
                }
            };

            output[i * 2]     = l;
            output[i * 2 + 1] = r;
        }

        // 4. Update shared state for UI / inter-deck protocol (~every 441 frames = 10ms).
        self.publish_state();
    }

    fn handle_event(&mut self, ev: ControlEvent) {
        match ev {
            ControlEvent::Play => self.cmd_play(),
            ControlEvent::Pause => self.cmd_pause(),
            ControlEvent::Cue => self.cmd_cue(),
            ControlEvent::JogDelta { delta, velocity_rpm } => {
                self.cmd_jog_delta(delta, velocity_rpm);
            }
            ControlEvent::JogTouch { touched } => {
                self.cmd_jog_touch(touched);
            }
            ControlEvent::HotCueTrigger { slot, held } => {
                self.cmd_hot_cue_trigger(slot, held);
            }
            ControlEvent::HotCueSet { slot } => {
                self.cmd_hot_cue_set(slot);
            }
            ControlEvent::HotCueDelete { slot } => {
                self.cues.hot_cues[slot as usize] = None;
            }
            ControlEvent::LoopIn => {
                self.loops.set_in(self.active_pos, self.beat_grid.as_deref(), self.quantize, self.sample_rate);
            }
            ControlEvent::LoopOut => {
                self.loops.set_out(self.active_pos, self.beat_grid.as_deref(), self.quantize, self.sample_rate);
            }
            ControlEvent::LoopToggle => {
                self.loops.toggle();
            }
            ControlEvent::Reloop => {
                self.loops.reloop(&mut self.active_pos);
            }
            ControlEvent::BeatLoop { beats, held } => {
                self.cmd_beat_loop(beats, held);
            }
            ControlEvent::BeatJump { beats } => {
                self.cmd_beat_jump(beats);
            }
            ControlEvent::SlipToggle => {
                self.slip.toggle(self.active_pos);
            }
            ControlEvent::KeyLockToggle => {
                self.key_lock = !self.key_lock;
                // Pipeline swap happens in app layer — engine just publishes the flag.
                self.engine_state.key_lock.store(self.key_lock, Ordering::Relaxed);
            }
            ControlEvent::TempoFader { position } => {
                // Map 0.0–1.0 fader to ±8% range (or ±100% if wide mode).
                // TODO: configurable range
                let range = 0.08_f32;
                self.target_speed = 1.0 + (position - 0.5) * 2.0 * range;
            }
            ControlEvent::NeedleSearch { position } => {
                if let Some(track) = &self.track {
                    let target = (position * track.duration_frames as f32) as u64;
                    self.seek(target);
                }
            }
            ControlEvent::Eject => {
                self.state = TransportState::Empty;
                self.track = None;
                self.active_pos = 0;
                self.beat_grid = None;
            }
            _ => {}
        }
    }

    fn handle_decode_event(&mut self, ev: DecodeEvent) {
        match ev {
            DecodeEvent::SeekComplete { actual_sample } => {
                self.active_pos = actual_sample;
            }
            DecodeEvent::BufferUnderrun => {
                log::warn!("decode buffer underrun — output will contain silence gaps");
            }
        }
    }

    // ── Commands ──────────────────────────────────────────────────────────────

    fn cmd_play(&mut self) {
        if self.state == TransportState::Stopped || self.state == TransportState::Empty {
            return;
        }
        self.state = TransportState::Playing;
        self.target_speed = 1.0;
    }

    fn cmd_pause(&mut self) {
        self.target_speed = 0.0;
        // Transition to Stopped happens once speed ramps to zero.
        // For instant stop, set directly:
        // self.state = TransportState::Stopped;
    }

    fn cmd_cue(&mut self) {
        match self.state {
            TransportState::Playing => {
                // Jump to cue and stop.
                self.seek(self.cue_position);
                self.state = TransportState::Stopped;
                self.target_speed = 0.0;
            }
            TransportState::Stopped => {
                // Update cue to current position.
                self.cue_position = self.active_pos;
            }
            _ => {}
        }
    }

    fn cmd_jog_touch(&mut self, touched: bool) {
        if touched {
            self.state = TransportState::Scratching;
            // Auto-suspend key lock during scratch for natural feel.
            if self.key_lock {
                self.key_lock_suspended = true;
                self.engine_state.key_lock.store(false, Ordering::Relaxed);
            }
        } else {
            self.state = TransportState::Playing;
            if self.key_lock_suspended {
                self.key_lock_suspended = false;
                self.engine_state.key_lock.store(true, Ordering::Relaxed);
            }
            // Restore normal playback speed.
            self.target_speed = 1.0;
            // If slip is active, trigger crossfade to ghost position.
            if self.slip.enabled {
                self.slip.begin_release(self.active_pos, self.sample_rate);
            }
        }
    }

    fn cmd_jog_delta(&mut self, _delta: i32, velocity_rpm: f32) {
        if self.state == TransportState::Scratching {
            // velocity_rpm is signed: positive = forward, negative = reverse.
            // Convert to a speed ratio: 33.3 RPM reference for vinyl feel.
            self.target_speed = velocity_rpm / 33.33;
        }
    }

    fn cmd_hot_cue_trigger(&mut self, slot: u8, held: bool) {
        if let Some(cue) = &self.cues.hot_cues[slot as usize] {
            let target = cue.position;
            self.seek(target);
            if held && self.slip.enabled {
                // Hold: play from cue while slip ghost continues.
                self.state = TransportState::Playing;
            } else if !held {
                self.state = TransportState::Playing;
            }
        }
    }

    fn cmd_hot_cue_set(&mut self, slot: u8) {
        use opendeck_types::{CueKind, CuePoint, Rgb};
        let pos = if self.quantize {
            self.beat_grid
                .as_ref()
                .map(|g| g.nearest_beat_before(self.active_pos, self.sample_rate))
                .unwrap_or(self.active_pos)
        } else {
            self.active_pos
        };
        let colors = [Rgb::RED, Rgb::BLUE, Rgb::GREEN, Rgb::YELLOW,
                      Rgb::CYAN, Rgb::ORANGE, Rgb::PINK, Rgb::WHITE];
        self.cues.hot_cues[slot as usize] = Some(CuePoint {
            slot,
            position: pos,
            color: colors[slot as usize % colors.len()],
            label: String::new(),
            kind: CueKind::HotCue,
        });
    }

    fn cmd_beat_loop(&mut self, beats: f32, _held: bool) {
        if let Some(grid) = &self.beat_grid {
            let in_pt = grid.nearest_beat_before(self.active_pos, self.sample_rate);
            let beat_len = grid.samples_per_beat_at(self.active_pos, self.sample_rate);
            let out_pt = in_pt + (beat_len * beats as f64) as u64;
            self.loops.set_beat_loop(in_pt, out_pt);
        }
    }

    fn cmd_beat_jump(&mut self, beats: f32) {
        if let Some(grid) = &self.beat_grid {
            let beat_len = grid.samples_per_beat_at(self.active_pos, self.sample_rate);
            let delta = (beat_len * beats as f64) as i64;
            let new_pos = (self.active_pos as i64 + delta).max(0) as u64;
            self.seek(new_pos);
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn seek(&mut self, target: u64) {
        // Signal the decode thread to seek.
        let _ = self.decode_tx.push(DecodeCmd::Seek(target));
        // Optimistically update position — SeekComplete will correct if needed.
        self.active_pos = target;
    }

    fn check_loop_boundary(&mut self) {
        if let Some(lp) = self.loops.active() {
            if self.active_pos >= lp.out_pt {
                if self.slip.enabled {
                    // Slip: loop the visible position, ghost continues through.
                }
                self.active_pos = lp.in_pt;
                self.seek(lp.in_pt);
            }
        }
    }

    fn advance(&self, pos: u64, speed: f32) -> u64 {
        if speed.abs() < SPEED_DEADZONE {
            return pos;
        }
        let delta = speed as f64;
        let new_pos = pos as f64 + delta;
        new_pos.max(0.0) as u64
    }

    fn read_frame_at(&self, _pos: u64, _ring: &Consumer<f32>) -> [f32; 2] {
        // TODO: implement sub-sample interpolation and ring buffer read.
        // For now, reads from ring sequentially (no random access).
        // Full implementation requires a ring buffer that supports positional reads,
        // or a separate pre-fetch buffer indexed by sample position.
        [0.0, 0.0]
    }

    fn publish_state(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let s = &self.engine_state;
        s.position.store(self.active_pos, Relaxed);
        s.ghost_position.store(self.slip.ghost_pos, Relaxed);
        s.speed_fixed.store((self.current_speed * 100_000.0) as i32, Relaxed);
        s.is_playing.store(self.state == TransportState::Playing, Relaxed);
        s.slip_active.store(self.slip.enabled, Relaxed);

        if let (Some(grid), _) = (&self.beat_grid, self.sample_rate) {
            let bpm = grid.bpm as f32;
            let phase = grid.phase_at_sample(self.active_pos, self.sample_rate);
            s.bpm_fixed.store((bpm * 100.0) as u32, Relaxed);
            s.beat_phase.store((phase * 65535.0) as u16, Relaxed);
        }

        s.timestamp_ns.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            Relaxed,
        );
    }
}

// ── Math helpers (no allocation, no branches in hot path) ────────────────────

#[inline(always)]
fn ramp_toward(current: f32, target: f32, rate: f32) -> f32 {
    let diff = target - current;
    if diff.abs() <= rate {
        target
    } else {
        current + diff.signum() * rate
    }
}

#[inline(always)]
fn smooth_step(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline(always)]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
