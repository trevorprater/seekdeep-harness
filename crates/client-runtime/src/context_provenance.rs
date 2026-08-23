//! Durable message-source projection for non-user transcript context.

use serde_json::{Map, Value};

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
    let Some(record) = source.as_object() else {
        return unnamed();
    };
    let Some(kind) = read_string(record, "kind") else {
        return unnamed();
    };
    match kind {
        "session-reference" => ContextProvenanceView {
            role: ContextRole::Recall,
            label: joined(&collect(record, "references", "label"))
                .or_else(|| Some(kind.to_owned())),
        },
        "agent-instructions" => ContextProvenanceView {
            role: ContextRole::Inject,
            label: joined(&collect(record, "changes", "path")).or_else(|| Some(kind.to_owned())),
        },
        "plugin" => ContextProvenanceView {
            role: ContextRole::Inject,
            label: read_string(record, "plugin")
                .map(str::to_owned)
                .or_else(|| Some(kind.to_owned())),
        },
        "skill-invocation" => ContextProvenanceView {
            role: ContextRole::Inject,
            label: read_string(record, "name")
                .map(str::to_owned)
                .or_else(|| Some(kind.to_owned())),
        },
        _ => ContextProvenanceView {
            role: ContextRole::Inject,
            label: Some(kind.to_owned()),
        },
    }
}

/// Reads a known presentation form, returning `None` for absent or future values.
#[must_use]
pub fn context_form(source: &Value) -> Option<KnownContextForm> {
    match source
        .as_object()
        .and_then(|record| read_string(record, "form"))
    {
        Some("instructions") => Some(KnownContextForm::Instructions),
        Some("catalog") => Some(KnownContextForm::Catalog),
        Some("snapshot") => Some(KnownContextForm::Snapshot),
        Some("notice") => Some(KnownContextForm::Notice),
        Some("relay") => Some(KnownContextForm::Relay),
        Some("recall") => Some(KnownContextForm::Recall),
        Some(_) | None => None,
    }
}

fn unnamed() -> ContextProvenanceView {
    ContextProvenanceView {
        role: ContextRole::Inject,
        label: None,
    }
}

fn read_string<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    record.get(key)?.as_str().filter(|value| !value.is_empty())
}

fn collect(record: &Map<String, Value>, member: &str, field: &str) -> Vec<String> {
    let Some(entries) = record.get(member).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    for entry in entries {
        let value = entry
            .as_object()
            .and_then(|entry| read_string(entry, field));
        if let Some(value) = value.filter(|value| !seen.iter().any(|seen| seen == value)) {
            seen.push(value.to_owned());
        }
    }
    seen
}

fn joined(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| names.join(", "))
}
