//! Meta validation: re-checks the typed workflow meta against the
//! `WorkflowMeta` contract and rejects every violation by name.

use seekdeep_workflow::{WorkflowError, WorkflowErrorCode, WorkflowMeta, WorkflowPhase};
use serde_json::Value;

fn meta_error(violations: &[String]) -> anyhow::Error {
    WorkflowError::new(
        format!("invalid meta: {}", violations.join("; ")),
        WorkflowErrorCode::MetaInvalid,
    )
    .into()
}

/// Validates lossless caller data and returns a normalized workflow meta copy.
///
/// This is the compatibility boundary for dynamically typed bindings. It
/// collects every shape violation so unknown or mistyped fields are never
/// accepted and silently discarded.
///
/// # Errors
///
/// Returns `META_INVALID` naming every discovered violation.
pub fn validate_meta_value(value: &Value) -> anyhow::Result<WorkflowMeta> {
    let Some(record) = value.as_object() else {
        return Err(meta_error(&["meta must be an object".to_owned()]));
    };
    let mut violations = Vec::new();
    for key in record.keys() {
        if !["name", "description", "whenToUse", "phases"].contains(&key.as_str()) {
            violations.push(format!(
                "meta.{key} is not a recognized field (name/description/whenToUse/phases)"
            ));
        }
    }
    let name = record
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if name.is_none() {
        violations.push("meta.name must be a non-empty string".to_owned());
    }
    let description = record
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if description.is_none() {
        violations.push("meta.description must be a non-empty string".to_owned());
    }
    let when_to_use = match record.get("whenToUse") {
        None => None,
        Some(value) => {
            if let Some(value) = value.as_str() {
                Some(value)
            } else {
                violations.push("meta.whenToUse must be a string".to_owned());
                None
            }
        }
    };
    let phases = match record.get("phases") {
        None => None,
        Some(Value::Array(entries)) => {
            let mut phases = Vec::new();
            for (index, value) in entries.iter().enumerate() {
                let Some(entry) = value.as_object() else {
                    violations.push(format!("meta.phases[{index}] must be an object"));
                    continue;
                };
                for key in entry.keys() {
                    if !["title", "detail", "provider", "model"].contains(&key.as_str()) {
                        violations.push(format!(
                            "meta.phases[{index}].{key} is not a recognized field"
                        ));
                    }
                }
                let title = entry
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty());
                if title.is_none() {
                    violations.push(format!(
                        "meta.phases[{index}].title must be a non-empty string"
                    ));
                }
                let detail = optional_phase_string(entry, index, "detail", &mut violations);
                let provider = optional_phase_string(entry, index, "provider", &mut violations);
                let model = optional_phase_string(entry, index, "model", &mut violations);
                if let Some(title) = title {
                    phases.push(WorkflowPhase {
                        title: title.to_owned(),
                        detail: detail.map(str::to_owned),
                        provider: provider.map(str::to_owned),
                        model: model.map(str::to_owned),
                    });
                }
            }
            Some(phases)
        }
        Some(_) => {
            violations.push("meta.phases must be an array".to_owned());
            None
        }
    };
    if !violations.is_empty() {
        return Err(meta_error(&violations));
    }
    let (Some(name), Some(description)) = (name, description) else {
        return Err(meta_error(&violations));
    };
    Ok(WorkflowMeta {
        name: name.to_owned(),
        description: description.to_owned(),
        when_to_use: when_to_use.map(str::to_owned),
        phases,
    })
}

fn optional_phase_string<'a>(
    entry: &'a serde_json::Map<String, Value>,
    index: usize,
    field: &str,
    violations: &mut Vec<String>,
) -> Option<&'a str> {
    match entry.get(field) {
        None => None,
        Some(value) => {
            if let Some(value) = value.as_str() {
                Some(value)
            } else {
                violations.push(format!("meta.phases[{index}].{field} must be a string"));
                None
            }
        }
    }
}

/// Validate a caller-provided meta block against the `WorkflowMeta` contract.
///
/// The typed `WorkflowMeta` already enforces field presence, types, and the
/// closed field set through serde; this re-check enforces the non-emptiness
/// invariants the wire decoder cannot, returning a normalized copy so the
/// engine never aliases the caller's value.
///
/// # Errors
///
/// Returns a meta-invalid failure naming every violation.
pub fn validate_meta(meta: &WorkflowMeta) -> anyhow::Result<WorkflowMeta> {
    validate_meta_value(&serde_json::to_value(meta)?)
}
