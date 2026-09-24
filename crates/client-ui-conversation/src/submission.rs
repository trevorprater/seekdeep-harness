//! Busy-Enter settings and composer submission policy.

use serde::{Deserialize, Serialize};

/// Settings namespace owned by conversation UI.
pub const CONVERSATION_SETTINGS_NAMESPACE: &str = "ui-conversation";
/// Durable field controlling plain Enter while an agent is busy.
pub const BUSY_ENTER_FIELD: &str = "busyEnter";

/// Accepted busy-Enter behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusyEnterBehavior {
    /// Plain Enter queues; accelerated Enter steers.
    #[default]
    Queue,
    /// Plain Enter steers; accelerated Enter queues.
    Steer,
}

/// Default preserves Enter-as-Queue for running conversations.
pub const DEFAULT_BUSY_ENTER_BEHAVIOR: BusyEnterBehavior = BusyEnterBehavior::Queue;

/// Keyboard gesture whose mode is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerSubmitGesture {
    /// Plain Enter.
    Enter,
    /// Cmd/Ctrl-accelerated Enter.
    Accelerated,
}

/// Durable conversation settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSettings {
    /// Plain-Enter behavior while busy.
    pub busy_enter: BusyEnterBehavior,
}

/// Live submission policy shared by composer and Settings row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposerSubmissionPolicy {
    busy_enter: BusyEnterBehavior,
}

impl ComposerSubmissionPolicy {
    /// Current plain-Enter preference.
    #[must_use]
    pub const fn busy_enter(self) -> BusyEnterBehavior {
        self.busy_enter
    }

    /// Resolves a keyboard gesture without changing state.
    #[must_use]
    pub const fn resolve(
        self,
        running: bool,
        gesture: ComposerSubmitGesture,
        steering_available: bool,
    ) -> BusyEnterBehavior {
        if !running || !steering_available {
            return BusyEnterBehavior::Queue;
        }
        match gesture {
            ComposerSubmitGesture::Enter => self.busy_enter,
            ComposerSubmitGesture::Accelerated => match self.busy_enter {
                BusyEnterBehavior::Queue => BusyEnterBehavior::Steer,
                BusyEnterBehavior::Steer => BusyEnterBehavior::Queue,
            },
        }
    }

    /// Publishes a local preference change; returns whether durable storage owes a write.
    pub fn set_busy_enter(&mut self, behavior: BusyEnterBehavior) -> bool {
        if self.busy_enter == behavior {
            return false;
        }
        self.busy_enter = behavior;
        true
    }

    /// Adopts a Host-published setting without requesting a write back.
    pub fn adopt(&mut self, settings: ConversationSettings) {
        self.busy_enter = settings.busy_enter;
    }
}
