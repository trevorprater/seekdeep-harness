//! Transient panel preferences and exact action semantics.

use serde::{Deserialize, Serialize};

use crate::{
    DETAILS_DEFAULT, DETAILS_MAX, DETAILS_MIN, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN,
    clamp_width,
};

/// Per-root transient panel state. It is intentionally never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutState {
    /// Sidebar preference in pixels, or zero when explicitly closed.
    pub sidebar: f64,
    /// Details preference in pixels, or zero when closed.
    pub details: f64,
    /// Whether the frame is currently below the auto-collapse breakpoint.
    pub narrow: bool,
    /// Manual re-expansion override used only while narrow.
    pub narrow_expanded: bool,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            sidebar: SIDEBAR_DEFAULT,
            details: 0.0,
            narrow: false,
            narrow_expanded: false,
        }
    }
}

impl LayoutState {
    /// Clamps and writes the open sidebar preference.
    pub fn set_sidebar(&mut self, px: f64) {
        self.sidebar = clamp_width(px, SIDEBAR_MIN, SIDEBAR_MAX);
    }

    /// Clamps and writes the open details preference.
    pub fn set_details(&mut self, px: f64) {
        self.details = clamp_width(px, DETAILS_MIN, DETAILS_MAX);
    }

    /// Toggles either the wide preference or the narrow manual override.
    pub fn toggle_sidebar(&mut self) {
        if self.narrow {
            self.narrow_expanded = !self.narrow_expanded;
        } else {
            self.sidebar = if self.sidebar == 0.0 {
                SIDEBAR_DEFAULT
            } else {
                0.0
            };
        }
    }

    /// Mirrors breakpoint state and drops overrides only on a crossing.
    pub fn set_narrow(&mut self, narrow: bool) {
        if self.narrow == narrow {
            return;
        }
        self.narrow = narrow;
        self.narrow_expanded = false;
    }

    /// Opens details at the contract default without disturbing an open width.
    pub fn open_details(&mut self) {
        if self.details == 0.0 {
            self.details = DETAILS_DEFAULT;
        }
    }

    /// Closes details and forgets its drag width.
    pub fn close_details(&mut self) {
        self.details = 0.0;
    }
}
