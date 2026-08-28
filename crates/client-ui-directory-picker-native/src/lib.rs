//! Native directory-picker flow ownership and browser registration.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-directory-picker-native";
/// Exact Client dependency order.
pub const INJECT: &[&str] = &["slots", "workspaces"];

/// Rising-edge and lifetime state for one renderless flow occupant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeDirectoryFlowState {
    armed: bool,
    alive: bool,
}

impl NativeDirectoryFlowState {
    /// Creates the initial alive, unarmed state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            armed: false,
            alive: true,
        }
    }

    /// Re-arms `StrictMode` setup after a replay cleanup.
    pub fn mount(&mut self) {
        self.alive = true;
    }

    /// Discards every later settlement.
    pub fn unmount(&mut self) {
        self.alive = false;
    }

    /// Returns `true` exactly once for each rising open edge.
    #[must_use]
    pub fn reconcile_open(&mut self, open: bool) -> bool {
        if !open {
            self.armed = false;
            return false;
        }
        if self.armed {
            return false;
        }
        self.armed = true;
        true
    }

    /// Whether a pending pick may still report through the latest owner callbacks.
    #[must_use]
    pub const fn accepts_settlement(self) -> bool {
        self.alive
    }
}

/// Builds the no-op Host half of this browser-owned plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
