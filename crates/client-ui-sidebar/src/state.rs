//! Target-portable sidebar fold and pointer-scrollbar state.

/// Axis-aligned sidebar column bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidebarBounds {
    /// Left edge, inclusive.
    pub left: f64,
    /// Right edge, exclusive.
    pub right: f64,
    /// Top edge, inclusive.
    pub top: f64,
    /// Bottom edge, exclusive.
    pub bottom: f64,
}

impl SidebarBounds {
    /// Whether one viewport point lies inside the exact half-open box.
    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollapsePhase {
    Wide,
    Fading,
    Rail,
}

/// Pure fold/pointer facts shared by Rust tests and the WASM shell.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarVisualState {
    phase: CollapsePhase,
    last_wide_width: f64,
    ever_wide: bool,
    pointer_inside: bool,
    linger_armed: bool,
}

impl SidebarVisualState {
    /// Creates the refresh-stable initial posture.
    #[must_use]
    pub fn new(collapsed: bool, width: f64) -> Self {
        Self {
            phase: if collapsed {
                CollapsePhase::Rail
            } else {
                CollapsePhase::Wide
            },
            last_wide_width: width,
            ever_wide: !collapsed,
            pointer_inside: false,
            linger_armed: false,
        }
    }

    /// Applies a layout snapshot. Returns whether a collapse-settle timer is required.
    pub fn apply_layout(&mut self, collapsed: bool, width: f64) -> bool {
        if !collapsed {
            self.phase = CollapsePhase::Wide;
            self.last_wide_width = width;
            self.ever_wide = true;
            return false;
        }
        match self.phase {
            CollapsePhase::Wide => {
                self.phase = CollapsePhase::Fading;
                true
            }
            CollapsePhase::Fading => true,
            CollapsePhase::Rail => false,
        }
    }

    /// Commits the delayed wide-content unmount when still collapsed.
    pub fn settle_collapse(&mut self) {
        if self.phase == CollapsePhase::Fading {
            self.phase = CollapsePhase::Rail;
        }
    }

    /// Whether expanded-width content remains mounted.
    #[must_use]
    pub fn wide(&self) -> bool {
        self.phase != CollapsePhase::Rail
    }

    /// Width frozen during the collapse crossfade.
    #[must_use]
    pub fn rendered_width(&self, current_width: f64) -> Option<f64> {
        self.wide()
            .then_some(if self.phase == CollapsePhase::Fading {
                self.last_wide_width
            } else {
                current_width
            })
    }

    /// Whether rail controls should crossfade in after a live collapse.
    #[must_use]
    pub fn rail_in(&self) -> bool {
        self.phase == CollapsePhase::Rail && self.ever_wide
    }

    /// Whether wide content is currently fading inside a collapsing column.
    #[must_use]
    pub fn fading(&self) -> bool {
        self.phase == CollapsePhase::Fading
    }

    /// Reveals scrollbars immediately and cancels a pending hide.
    pub fn pointer_enter(&mut self) {
        self.pointer_inside = true;
        self.linger_armed = false;
    }

    /// Arms the delayed hide once; repeated leaves remain idempotent.
    pub fn pointer_leave(&mut self) -> bool {
        if self.linger_armed {
            return false;
        }
        self.linger_armed = true;
        true
    }

    /// Re-evaluates the pointer against column geometry while bars are visible.
    pub fn pointer_move(&mut self, bounds: SidebarBounds, x: f64, y: f64) -> bool {
        if !self.pointer_inside {
            return false;
        }
        if bounds.contains(x, y) {
            self.linger_armed = false;
            false
        } else {
            self.pointer_leave()
        }
    }

    /// Completes the linger timer and hides scrollbars.
    pub fn linger_elapsed(&mut self) {
        self.linger_armed = false;
        self.pointer_inside = false;
    }

    /// Whether scrollbar tokens should remain rebound to transparent.
    #[must_use]
    pub fn quiet_bars(&self) -> bool {
        !self.pointer_inside
    }

    /// Whether one hide timer is armed.
    #[must_use]
    pub fn linger_armed(&self) -> bool {
        self.linger_armed
    }
}
