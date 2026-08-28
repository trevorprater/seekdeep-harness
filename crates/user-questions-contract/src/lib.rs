//! Target-portable structured user-question wire contracts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One selectable answer offered to the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionOption {
    /// User-facing label.
    pub label: String,
    /// Optional extra context rendered by capable UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Extensible presentation intent; unknown tags and fields remain intact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionIntent {
    /// Presentation tag, currently `plan-review`.
    pub kind: String,
    /// Label whose selection means approval.
    pub approve: String,
    /// Future intent fields preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One question in a user-questions request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionItem {
    /// Stable caller-provided identity echoed in the answer.
    pub id: String,
    /// Specific question displayed to the user.
    pub question: String,
    /// Optional supporting detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional short heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Optional choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<AskUserQuestionOption>>,
    /// Whether more than one option may be selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    /// Optional presentation-only intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<AskUserQuestionIntent>,
}

/// Answer to one question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionAnswerItem {
    /// Echoed question identity.
    pub id: String,
    /// Selected option labels.
    pub selected: Vec<String>,
    /// Optional free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

/// Structured human answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionAnswer {
    /// Answers keyed by their echoed IDs.
    pub answers: Vec<AskUserQuestionAnswerItem>,
}
