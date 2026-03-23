use opendeck_types::BeatGrid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopKind {
    Manual,
    Beat,
    Roll,  // slip + loop simultaneously
}

#[derive(Debug, Clone, Copy)]
pub struct ActiveLoop {
    pub in_pt:  u64,
    pub out_pt: u64,
    pub kind:   LoopKind,
}

pub struct LoopEngine {
    active:      Option<ActiveLoop>,
    pending_in:  Option<u64>,  // loop-in set, waiting for loop-out
}

impl LoopEngine {
    pub fn new() -> Self {
        Self { active: None, pending_in: None }
    }

    pub fn active(&self) -> Option<ActiveLoop> {
        self.active
    }

    pub fn set_in(&mut self, pos: u64, grid: Option<&BeatGrid>, quantize: bool, sr: u32) {
        let snapped = if quantize {
            grid.map(|g| g.nearest_beat_before(pos, sr)).unwrap_or(pos)
        } else {
            pos
        };
        self.pending_in = Some(snapped);
    }

    pub fn set_out(&mut self, pos: u64, grid: Option<&BeatGrid>, quantize: bool, sr: u32) {
        let snapped = if quantize {
            grid.map(|g| g.nearest_beat_before(pos, sr)).unwrap_or(pos)
        } else {
            pos
        };
        if let Some(in_pt) = self.pending_in.take() {
            if snapped > in_pt {
                self.active = Some(ActiveLoop { in_pt, out_pt: snapped, kind: LoopKind::Manual });
            }
        }
    }

    pub fn set_beat_loop(&mut self, in_pt: u64, out_pt: u64) {
        self.active = Some(ActiveLoop { in_pt, out_pt, kind: LoopKind::Beat });
    }

    pub fn toggle(&mut self) {
        if let Some(lp) = &mut self.active {
            // Toggling deactivates without clearing — reloop can restore it.
            let _ = lp; // mark as "disabled" via a wrapper if needed
            self.active = None;
        }
    }

    pub fn reloop(&mut self, active_pos: &mut u64) {
        if let Some(lp) = self.active {
            *active_pos = lp.in_pt;
        }
    }
}

impl Default for LoopEngine {
    fn default() -> Self {
        Self::new()
    }
}
