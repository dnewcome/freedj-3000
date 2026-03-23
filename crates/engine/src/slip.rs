/// Crossfade duration when snapping from active to ghost position on slip release.
const CROSSFADE_MS: f32 = 10.0;

pub struct SlipState {
    /// The "shadow" position: always advances at 1× while slip is engaged.
    pub ghost_pos:  u64,
    pub enabled:    bool,
    pub crossfade:  Option<CrossfadeState>,
}

pub struct CrossfadeState {
    pub from_pos:     u64,
    pub to_pos:       u64,
    pub sample_index: usize,
    pub length:       usize,
}

impl SlipState {
    pub fn new() -> Self {
        Self {
            ghost_pos:  0,
            enabled:    false,
            crossfade:  None,
        }
    }

    pub fn toggle(&mut self, active_pos: u64) {
        self.enabled = !self.enabled;
        if self.enabled {
            self.ghost_pos = active_pos;
        } else {
            self.crossfade = None;
        }
    }

    /// Advance ghost by one sample at 1× speed.
    #[inline(always)]
    pub fn advance_ghost(&mut self) {
        self.ghost_pos = self.ghost_pos.saturating_add(1);
    }

    /// Call when the jog wheel is released or a loop/cue hold ends.
    /// Schedules a crossfade from the current active position to the ghost.
    pub fn begin_release(&mut self, active_pos: u64, sample_rate: u32) {
        if !self.enabled {
            return;
        }
        let length = (sample_rate as f32 * CROSSFADE_MS / 1000.0) as usize;
        self.crossfade = Some(CrossfadeState {
            from_pos:     active_pos,
            to_pos:       self.ghost_pos,
            sample_index: 0,
            length,
        });
    }
}

impl Default for SlipState {
    fn default() -> Self {
        Self::new()
    }
}
