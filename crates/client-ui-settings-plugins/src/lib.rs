//! Configurable-plugin Settings field and staged form semantics.

use serde_json::Value;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-settings-plugins";

/// One field's planned durable mutation.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldWrite {
    /// Set one JSON-compatible value.
    Set(Value),
    /// Clear the user-layer field so it re-inherits.
    Clear,
}

/// Whole-number field conversion contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberField {
    /// Field name inside the Settings namespace.
    pub field: String,
}

impl NumberField {
    /// Formats numeric stored values, otherwise the empty draft.
    #[must_use]
    pub fn format(&self, value: &Value) -> String {
        value
            .as_number()
            .map_or_else(String::new, ToString::to_string)
    }

    /// Empty clears; finite JavaScript-compatible numeric text sets; malformed blocks save.
    #[must_use]
    pub fn parse(&self, text: &str) -> Option<FieldWrite> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Some(FieldWrite::Clear);
        }
        parse_js_number(trimmed)
            .filter(|value| value.is_finite())
            .and_then(serde_json::Number::from_f64)
            .map(|value| FieldWrite::Set(Value::Number(value)))
    }
}

/// Free-text field conversion contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextField {
    /// Field name inside the Settings namespace.
    pub field: String,
}

impl TextField {
    /// Formats string stored values, otherwise the empty draft.
    #[must_use]
    pub fn format(&self, value: &Value) -> String {
        value.as_str().map_or_else(String::new, ToOwned::to_owned)
    }

    /// Empty clears; other text is trimmed before writing.
    #[must_use]
    pub fn parse(&self, text: &str) -> FieldWrite {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            FieldWrite::Clear
        } else {
            FieldWrite::Set(Value::String(trimmed.to_owned()))
        }
    }
}

fn parse_js_number(value: &str) -> Option<f64> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return radix_number(hex, 16);
    }
    if let Some(octal) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        return radix_number(octal, 8);
    }
    if let Some(binary) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        return radix_number(binary, 2);
    }
    value.parse().ok()
}

fn radix_number(value: &str, radix: u32) -> Option<f64> {
    u64::from_str_radix(value, radix).ok().map(|value| {
        #[allow(clippy::cast_precision_loss)]
        {
            value as f64
        }
    })
}

/// Creates a number field spec.
#[must_use]
pub fn number_field(field: impl Into<String>) -> NumberField {
    NumberField {
        field: field.into(),
    }
}

/// Creates a text field spec.
#[must_use]
pub fn text_field(field: impl Into<String>) -> TextField {
    TextField {
        field: field.into(),
    }
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
