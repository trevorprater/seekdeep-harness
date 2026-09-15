//! Tag-safe JSON serialization for the model-visible reference envelope.

use serde::Serialize;

/// Serializes JSON while preventing source data from spelling an XML-like opening tag.
///
/// The returned JSON parses to the same value and contains no literal less-than
/// character in its data.
///
/// # Panics
///
/// Panics when the value is not JSON-serializable, which cannot happen for the
/// reference data this crate serializes.
#[must_use]
pub fn stringify_tag_safe_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .expect("session-reference data is not JSON-serializable")
        .replace('<', "\\u003c")
}
