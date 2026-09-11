//! Human-readable JSON unit format and strict durable-boundary parser.

use indexmap::IndexMap;
use seekdeep_lossless_json::{JsonRef, JsonValue};
use seekdeep_storage::{KvUnitDescriptor, StorageError, StorageErrorCode};
use serde_json::{Number, Value};

/// Authoritative state of one open JSON unit.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitState {
    /// Durable format version.
    pub version: u64,
    /// Global singleton or null before its first write.
    pub global: JsonValue,
    /// Declared tables and insertion-ordered records.
    pub tables: IndexMap<String, IndexMap<String, JsonValue>>,
}

/// Serializes exact `JSON.stringify(document, null, 2)` layout plus one newline.
#[must_use]
pub fn serialize(name: &str, state: &UnitState) -> String {
    let tables = JsonValue::object(state.tables.iter().map(|(name, records)| {
        (
            name.clone(),
            JsonValue::object(
                records
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            ),
        )
    }));
    let document = JsonValue::object([
        (
            "unit",
            JsonValue::object([
                ("name", Value::String(name.to_owned()).into()),
                ("version", Value::Number(state.version.into()).into()),
            ]),
        ),
        ("global", state.global.clone()),
        ("tables", tables),
    ]);
    let mut output = document.stringify_pretty();
    output.push('\n');
    output
}

/// Parses and validates one stored unit document.
///
/// # Errors
///
/// Returns typed malformed-medium or version-mismatch failures.
pub fn parse(text: &str, descriptor: &KvUnitDescriptor) -> Result<UnitState, StorageError> {
    let document = JsonValue::parse(text.to_owned()).map_err(|error| {
        StorageError::with_source(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': file is not valid JSON", descriptor.name),
            error.into(),
        )
    })?;
    if !document.is_object() {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': file is not a JSON object", descriptor.name),
        ));
    }
    let unit = document.get("unit").filter(|unit| unit.is_object());
    let version = unit
        .and_then(|unit| unit.get("version"))
        .and_then(|version| version.deserialize::<Number>().ok());
    let header_valid = unit.is_some_and(|unit| {
        unit.get("name")
            .is_some_and(|name| name == descriptor.name.as_str())
    });
    let Some(version) = version.filter(|_| header_valid) else {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': missing or foreign unit header", descriptor.name),
        ));
    };
    if !number_equals_u64(&version, descriptor.version) {
        return Err(StorageError::new(
            StorageErrorCode::VersionMismatch,
            format!(
                "unit '{}': stored version {} != expected {}",
                descriptor.name,
                javascript_number(&version),
                descriptor.version
            ),
        ));
    }
    let stored_tables = document
        .get("tables")
        .filter(|tables| tables.is_object())
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorCode::MalformedMedium,
                format!("unit '{}': tables is not an object", descriptor.name),
            )
        })?;
    let mut tables = IndexMap::new();
    for table in &descriptor.tables {
        let records = match stored_tables.get(table) {
            None => IndexMap::new(),
            Some(records) if records.is_object() => records.deserialize().map_err(|error| {
                StorageError::with_source(
                    StorageErrorCode::MalformedMedium,
                    format!(
                        "unit '{}': table '{table}' has an invalid record key",
                        descriptor.name
                    ),
                    error.into(),
                )
            })?,
            Some(_) => {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    format!(
                        "unit '{}': table '{table}' is not an object",
                        descriptor.name
                    ),
                ));
            }
        };
        tables.insert(table.clone(), records);
    }
    Ok(UnitState {
        version: descriptor.version,
        global: document
            .get("global")
            .map_or_else(|| Value::Null.into(), JsonRef::to_owned),
        tables,
    })
}

fn number_equals_u64(number: &Number, expected: u64) -> bool {
    number.as_u64().is_some_and(|value| value == expected)
        || number
            .as_i64()
            .and_then(|value| u64::try_from(value).ok())
            .is_some_and(|value| value == expected)
        || number.as_f64().is_some_and(|value| {
            expected
                .to_string()
                .parse::<f64>()
                .is_ok_and(|expected| value.to_bits() == expected.to_bits())
        })
}

fn javascript_number(number: &Number) -> String {
    number.as_i64().map_or_else(
        || {
            number.as_u64().map_or_else(
                || {
                    let mut buffer = ryu_js::Buffer::new();
                    buffer
                        .format(number.as_f64().expect("JSON number is finite"))
                        .to_owned()
                },
                |value| value.to_string(),
            )
        },
        |value| value.to_string(),
    )
}
