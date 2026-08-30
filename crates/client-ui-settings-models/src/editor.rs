//! Provider-editor and model-catalog pure derivation.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{ProviderRow, api_key::trim_ecmascript_whitespace};

/// One model-catalog validation diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelValidationFailure {
    /// Zero-based invalid row.
    pub index: usize,
    /// Localized message key.
    pub key: ModelValidationKey,
}

/// Closed model-catalog failure key set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelValidationKey {
    /// Model id is absent or blank.
    IdRequired,
    /// Trimmed model id repeats an earlier row.
    IdDuplicate,
    /// Optional name is not a non-empty string.
    NameInvalid,
    /// Context capacity is not a positive integer.
    ContextInvalid,
    /// Output-token capacity is not a positive integer.
    MaxTokensInvalid,
}

impl ModelValidationKey {
    /// Browser locale key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdRequired => "modelIdRequired",
            Self::IdDuplicate => "modelIdDuplicate",
            Self::NameInvalid => "modelNameInvalid",
            Self::ContextInvalid => "modelContextInvalid",
            Self::MaxTokensInvalid => "modelMaxTokensInvalid",
        }
    }
}

/// One path-addressed settings mutation.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsPathOp {
    /// Set one path to a complete JSON value.
    Set {
        /// Section-relative path.
        path: Vec<String>,
        /// Replacement value.
        value: Value,
    },
    /// Remove one path.
    Unset {
        /// Section-relative path.
        path: Vec<String>,
    },
}

/// Reads a K/M-suffixed capacity. Blank input inherits; unreadable input returns `NaN`.
#[must_use]
pub fn parse_capacity(text: &str) -> Option<f64> {
    let trimmed = trim_ecmascript_whitespace(text);
    if trimmed.is_empty() {
        return None;
    }
    let (number, scale) = match trimmed.as_bytes().last().copied() {
        Some(b'k' | b'K') => (&trimmed[..trimmed.len() - 1], 1_000.0),
        Some(b'm' | b'M') => (&trimmed[..trimmed.len() - 1], 1_000_000.0),
        _ => (trimmed, 1.0),
    };
    let mut dot = false;
    if number.is_empty()
        || number.bytes().enumerate().any(|(index, byte)| match byte {
            b'0'..=b'9' => false,
            b'.' if index > 0 && index + 1 < number.len() && !dot => {
                dot = true;
                false
            }
            _ => true,
        })
    {
        return Some(f64::NAN);
    }
    let Ok(value) = number.parse::<f64>() else {
        return Some(f64::NAN);
    };
    let scaled = value * scale;
    let rounded = scaled.round();
    Some(if (scaled - rounded).abs() < 1e-6 {
        rounded
    } else {
        scaled
    })
}

/// Spells a stored capacity in the shortest K/M form that round-trips.
#[must_use]
pub fn format_capacity(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 || value.fract() != 0.0 {
        return javascript_number(value);
    }
    if value % 1_000_000.0 == 0.0 {
        return format!("{}M", javascript_number(value / 1_000_000.0));
    }
    if value % 1_000.0 == 0.0 {
        return format!("{}K", javascript_number(value / 1_000.0));
    }
    javascript_number(value)
}

/// Detaches object model rows, replacing malformed entries with empty drafts.
#[must_use]
pub fn model_drafts(value: Option<&Value>) -> Vec<Map<String, Value>> {
    value
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .map(|model| model.as_object().cloned().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// Validates adapter constraints the serialized schema cannot express.
#[must_use]
pub fn validate_models(value: Option<&Value>) -> Option<ModelValidationFailure> {
    let value = value?;
    let models = model_drafts(Some(value));
    let mut seen = BTreeSet::new();
    for (index, model) in models.iter().enumerate() {
        let id = model
            .get("id")
            .and_then(Value::as_str)
            .map(trim_ecmascript_whitespace);
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return Some(ModelValidationFailure {
                index,
                key: ModelValidationKey::IdRequired,
            });
        };
        if !seen.insert(id.to_owned()) {
            return Some(ModelValidationFailure {
                index,
                key: ModelValidationKey::IdDuplicate,
            });
        }
        if model
            .get("name")
            .is_some_and(|name| name.as_str().is_none_or(str::is_empty))
        {
            return Some(ModelValidationFailure {
                index,
                key: ModelValidationKey::NameInvalid,
            });
        }
        for (field, key) in [
            ("contextWindow", ModelValidationKey::ContextInvalid),
            ("maxTokens", ModelValidationKey::MaxTokensInvalid),
        ] {
            if model.get(field).is_some_and(|capacity| {
                capacity
                    .as_f64()
                    .is_none_or(|capacity| capacity <= 0.0 || capacity.fract() != 0.0)
            }) {
                return Some(ModelValidationFailure { index, key });
            }
        }
    }
    None
}

/// Computes minimal top-level path operations from the observed draft subtree.
#[must_use]
pub fn path_ops(
    base: &[String],
    before: Option<&Value>,
    after: &Map<String, Value>,
) -> Vec<SettingsPathOp> {
    let previous = before.and_then(Value::as_object);
    let mut operations = Vec::new();
    for (key, value) in after {
        if previous.and_then(|previous| previous.get(key)) == Some(value) {
            continue;
        }
        let mut path = base.to_vec();
        path.push(key.clone());
        operations.push(SettingsPathOp::Set {
            path,
            value: value.clone(),
        });
    }
    if let Some(previous) = previous {
        for key in previous.keys() {
            if after.contains_key(key) {
                continue;
            }
            let mut path = base.to_vec();
            path.push(key.clone());
            operations.push(SettingsPathOp::Unset { path });
        }
    }
    operations
}

/// Whether the first-run Models page replaces one row with its setup card.
#[must_use]
pub fn needs_setup(row: &ProviderRow, any_usable: bool) -> bool {
    !any_usable
        && row.entry.settings_path.is_empty()
        && !row
            .credential
            .as_ref()
            .is_some_and(|credential| credential.configured)
}

/// Stable visible provider identity used by action and confirmation copy.
#[must_use]
pub fn provider_target_label(provider: &str, display_name: &str) -> String {
    if provider == display_name {
        provider.to_owned()
    } else {
        format!("{display_name} ({provider})")
    }
}

/// Replaces the localized provider placeholder without replacement-string interpretation.
#[must_use]
pub fn provider_copy(template: &str, provider: &str, display_name: &str) -> String {
    template.replacen(
        "{provider}",
        &provider_target_label(provider, display_name),
        1,
    )
}

/// Whether a custom provider route is safe as both a settings key and credential stem.
#[must_use]
pub fn route_valid(route: &str) -> bool {
    let mut groups = route.split('-');
    let Some(first) = groups.next() else {
        return false;
    };
    if first.is_empty()
        || !first.as_bytes()[0].is_ascii_lowercase()
        || !first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }
    groups.all(|group| {
        !group.is_empty()
            && group
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn javascript_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        ryu_js::Buffer::new().format(value).to_owned()
    }
}
