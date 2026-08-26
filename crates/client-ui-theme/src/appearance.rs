//! Appearance-row mirror state.

use serde::{Deserialize, Serialize};

use crate::ThemePreference;

/// Store state mirrored from the theme snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceRowState {
    /// Persisted built-in preference.
    pub preference: ThemePreference,
    /// Theme service revision; `-1` permits the initial revision zero sync.
    pub revision: i64,
}

impl Default for AppearanceRowState {
    fn default() -> Self {
        Self {
            preference: ThemePreference::System,
            revision: -1,
        }
    }
}

impl AppearanceRowState {
    /// Mirrors only a strictly newer service snapshot.
    pub fn sync(&mut self, preference: ThemePreference, revision: i64) {
        if revision <= self.revision {
            return;
        }
        self.preference = preference;
        self.revision = revision;
    }
}
