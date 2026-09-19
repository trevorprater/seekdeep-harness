//! Durable message-source projection for non-user transcript context.

use seekdeep_lossless_json::{JsonString, JsonValue};
use serde_json::Value;

/// Model-facing role of one logged context message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextRole {
    /// Producer-supplied context.
    Inject,
    /// Material recalled from another Session log.
    Recall,
}

/// Role and producer label shown for one durable source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextProvenanceView {
    /// Model-facing role.
    pub role: ContextRole,
    /// Human-facing producer name.
    pub label: Option<String>,
}

/// Role and exact UTF-16 producer label shown for one durable source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextProvenanceJsonView {
    /// Model-facing role.
    pub role: ContextRole,
    /// Human-facing producer name, preserving all source code units.
    pub label: Option<JsonString>,
}

/// Context forms with dedicated presentation in this Client version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownContextForm {
    /// Workspace or policy instructions.
    Instructions,
    /// Capability catalog.
    Catalog,
    /// State snapshot.
    Snapshot,
    /// Informational notice.
    Notice,
    /// Cross-agent relay.
    Relay,
    /// Cross-Session recall.
    Recall,
}

/// Projects one merge-extensible durable source into role and producer label.
#[must_use]
pub fn context_provenance(source: &Value) -> ContextProvenanceView {
    let view = context_provenance_json(&source.clone().into());
    ContextProvenanceView {
        role: view.role,
        label: view
            .label
            .and_then(|label| label.as_str().map(str::to_owned)),
    }
}

/// Projects a durable source without narrowing its labels or unrelated JSON fields.
#[must_use]
pub fn context_provenance_json(source: &JsonValue) -> ContextProvenanceJsonView {
    let Some(kind) = read_string(source, "kind") else {
        return unnamed();
    };
    match kind.as_str() {
        Some("session-reference") => ContextProvenanceJsonView {
            role: ContextRole::Recall,
            label: joined(&collect(source, "references", "label")).or(Some(kind)),
        },
        Some("agent-instructions") => ContextProvenanceJsonView {
            role: ContextRole::Inject,
            label: joined(&collect(source, "changes", "path")).or(Some(kind)),
        },
        Some("plugin") => ContextProvenanceJsonView {
            role: ContextRole::Inject,
            label: read_string(source, "plugin").or(Some(kind)),
        },
        Some("skill-invocation") => ContextProvenanceJsonView {
            role: ContextRole::Inject,
            label: read_string(source, "name").or(Some(kind)),
        },
        _ => ContextProvenanceJsonView {
            role: ContextRole::Inject,
            label: Some(kind),
        },
    }
}

/// Reads a known presentation form, returning `None` for absent or future values.
#[must_use]
pub fn context_form(source: &Value) -> Option<KnownContextForm> {
    context_form_json(&source.clone().into())
}

/// Reads a known form while retaining opaque source fields outside the projection.
#[must_use]
pub fn context_form_json(source: &JsonValue) -> Option<KnownContextForm> {
    match source.get_value("form").and_then(JsonValue::as_str) {
        Some("instructions") => Some(KnownContextForm::Instructions),
        Some("catalog") => Some(KnownContextForm::Catalog),
        Some("snapshot") => Some(KnownContextForm::Snapshot),
        Some("notice") => Some(KnownContextForm::Notice),
        Some("relay") => Some(KnownContextForm::Relay),
        Some("recall") => Some(KnownContextForm::Recall),
        Some(_) | None => None,
    }
}

fn unnamed() -> ContextProvenanceJsonView {
    ContextProvenanceJsonView {
        role: ContextRole::Inject,
        label: None,
    }
}

fn read_string(record: &JsonValue, key: &str) -> Option<JsonString> {
    record
        .get_value(key)?
        .deserialize::<JsonString>()
        .ok()
        .filter(|value| !value.is_empty())
}

fn collect(record: &JsonValue, member: &str, field: &str) -> Vec<JsonString> {
    let Some(entries) = record.get_value(member).and_then(JsonValue::as_array) else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    for entry in entries {
        let value = read_string(entry, field);
        if let Some(value) = value.filter(|value| !seen.iter().any(|seen| seen == value)) {
            seen.push(value);
        }
    }
    seen
}

fn joined(names: &[JsonString]) -> Option<JsonString> {
    (!names.is_empty()).then(|| JsonString::join(names, ", "))
}
