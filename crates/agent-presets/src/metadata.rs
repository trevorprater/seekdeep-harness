//! Optional display metadata beside an Agent preset composition.

use std::path::Path;

use indexmap::IndexMap;
use serde_yml::Value;

/// Optional display-metadata filename.
pub const METADATA_FILE: &str = "preset.yml";

/// Display-only preset metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetMetadata {
    /// Optional display name.
    pub name: Option<String>,
    /// Optional purpose sentence.
    pub description: Option<String>,
    /// Optional finite sort position.
    pub order: Option<f64>,
}

/// Reads display metadata, degrading every read, parse, and shape failure to empty.
pub async fn read_preset_metadata(directory: impl AsRef<Path>) -> PresetMetadata {
    let Ok(raw) = tokio::fs::read_to_string(directory.as_ref().join(METADATA_FILE)).await else {
        return PresetMetadata::default();
    };
    let Ok(Value::Mapping(mapping)) = serde_yml::from_str::<Value>(&raw) else {
        return PresetMetadata::default();
    };
    let field = |name: &str| mapping.get(Value::String(name.to_owned()));
    let name = field("name").and_then(text);
    let description = field("description").and_then(text);
    let order = field("order")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite());
    PresetMetadata {
        name,
        description,
        order,
    }
}

/// Renders display metadata, or returns `None` when every field is absent.
#[must_use]
pub fn render_preset_metadata(metadata: &PresetMetadata) -> Option<String> {
    let name = metadata.name.as_deref().and_then(trimmed);
    let description = metadata.description.as_deref().and_then(trimmed);
    let order = metadata.order.filter(|value| value.is_finite());
    if name.is_none() && description.is_none() && order.is_none() {
        return None;
    }
    let mut fields = IndexMap::<String, Value>::new();
    if let Some(name) = name {
        fields.insert("name".to_owned(), Value::String(name.to_owned()));
    }
    if let Some(description) = description {
        fields.insert(
            "description".to_owned(),
            Value::String(description.to_owned()),
        );
    }
    if let Some(order) = order {
        let value = if order.fract() == 0.0 {
            order.to_string().parse::<i64>().ok().map_or_else(
                || Value::Number(order.into()),
                |value| Value::Number(value.into()),
            )
        } else {
            Value::Number(order.into())
        };
        fields.insert("order".to_owned(), value);
    }
    serde_yml::to_string(&fields).ok()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().and_then(trimmed).map(ToOwned::to_owned)
}

fn trimmed(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
