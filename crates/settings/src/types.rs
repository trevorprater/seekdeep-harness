//! Client-safe settings identifiers and event values.

use serde::{Deserialize, Serialize};

/// Committed resolved-value change event.
pub const SETTINGS_UPDATED_EVENT: &str = "settings/updated";
/// Raw user-section revision change event.
pub const SETTINGS_DOCUMENT_UPDATED_EVENT: &str = "settings/document-updated";

seekdeep_util::string_brand!(
    /// Nominal identifier of one registered settings namespace.
    pub struct SettingsNamespace;
);

/// Origin of one committed settings change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsUpdateSource {
    /// In-process write.
    Update,
    /// Provider-observed document change.
    Provider,
}

/// When a namespace's changes take effect for its owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsApplies {
    /// Apply without restarting the owner.
    #[default]
    Live,
    /// Apply after owner restart.
    Restart,
}
