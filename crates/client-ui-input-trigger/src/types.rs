//! Frozen target-portable input-trigger vocabulary.

use serde::{Deserialize, Serialize};

/// Trigger character bound by a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerChar {
    /// Slash command/reference trigger.
    #[serde(rename = "/")]
    Slash,
    /// At-mention trigger.
    #[serde(rename = "@")]
    At,
}

impl TriggerChar {
    /// Wire character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Slash => '/',
            Self::At => '@',
        }
    }
}

/// Where the trigger token sits in the draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerPosition {
    /// First non-whitespace token.
    Leading,
    /// Follows earlier non-whitespace content.
    Inline,
}

/// Trigger availability derived from the input phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerTier {
    /// Slash and at triggers are live.
    Plain,
    /// Slash is suppressed while at stays live.
    Claimed,
    /// Every trigger is suppressed.
    Frozen,
}

/// Trigger availability guard supplied by the input wiring layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerGuard {
    /// Current availability tier.
    pub tier: TriggerTier,
}

impl From<TriggerTier> for TriggerGuard {
    fn from(tier: TriggerTier) -> Self {
        Self { tier }
    }
}

/// Pick path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PickVia {
    /// Candidate menu click/keyboard pick.
    Menu,
    /// Space adjudication.
    Space,
    /// Enter adjudication.
    Enter,
}

/// Pick-moment trigger token span using JavaScript UTF-16 offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSpan {
    /// Trigger character offset.
    pub start: usize,
    /// Caret offset.
    pub end: usize,
    /// Compare-and-swap draft revision.
    pub draft_rev: u64,
}

/// One menu candidate carrying display data only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTriggerCandidate {
    /// Exact source-owned name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional icon identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional right-hand hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl InputTriggerCandidate {
    /// Creates a name-only candidate.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            icon: None,
            hint: None,
        }
    }
}
