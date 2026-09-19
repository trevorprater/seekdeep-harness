//! Target-portable popup-select data contracts.

use serde::{Deserialize, Serialize};

/// Copy for an option requiring explicit risk acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectConfirmation {
    /// Dialog title.
    pub title: String,
    /// Risk description.
    pub description: String,
    /// Checkbox label.
    pub acknowledge_label: String,
    /// Cancel action.
    pub cancel_label: String,
    /// Confirm action.
    pub confirm_label: String,
}

/// One popup-select option row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    /// Stable option identity.
    pub id: String,
    /// Primary row label.
    pub label: String,
    /// Optional detail copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional active marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Optional in-page confirmation gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<SelectConfirmation>,
}

/// Popup options-load lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PopupStatus {
    /// Initial/retry fetch is active.
    Pending,
    /// Options are available.
    Ready,
    /// Latest fetch failed and may be retried.
    Failed,
}

/// Complete headless popup state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupState {
    /// Shell open bit.
    pub open: bool,
    /// Command served by the shell.
    pub command: Option<String>,
    /// Options lifecycle.
    pub status: PopupStatus,
    /// Loaded options.
    pub options: Vec<SelectOption>,
    /// Local filter text.
    pub search: String,
    /// Highlight into filtered rows.
    pub active: usize,
    /// Select settlement in flight.
    pub submitting: bool,
    /// Option awaiting risk acknowledgement.
    pub confirming: Option<SelectOption>,
    /// Current checkbox state.
    pub acknowledged: bool,
    /// Load/select failure copy.
    pub error: Option<String>,
}

impl Default for PopupState {
    fn default() -> Self {
        Self {
            open: false,
            command: None,
            status: PopupStatus::Pending,
            options: Vec::new(),
            search: String::new(),
            active: 0,
            submitting: false,
            confirming: None,
            acknowledged: false,
            error: None,
        }
    }
}
