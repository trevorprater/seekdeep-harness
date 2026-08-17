//! Human-readable JSON unit format and strict durable-boundary parser.

use indexmap::IndexMap;
use seekdeep_storage::{KvUnitDescriptor, StorageError, StorageErrorCode};
use serde_json::{Map, Number, Value};

/// Authoritative state of one open JSON unit.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitState {
    /// Durable format version.
    pub version: u64,
    /// Global singleton or null before its first write.
    pub global: Value,
    /// Declared tables and insertion-ordered records.
    pub tables: IndexMap<String, Map<String, Value>>,
}

/// Serializes exact `JSON.stringify(document, null, 2)` layout plus one newline.
#[must_use]
pub fn serialize(name: &str, state: &UnitState) -> String {
    let tables = state
        .tables
        .iter()
        .map(|(name, records)| (name.clone(), Value::Object(records.clone())))
        .collect::<Map<_, _>>();
    let document = Value::Object(Map::from_iter([
        (
            "unit".to_owned(),
            Value::Object(Map::from_iter([
                ("name".to_owned(), Value::String(name.to_owned())),
                ("version".to_owned(), Value::Number(state.version.into())),
            ])),
        ),
        ("global".to_owned(), state.global.clone()),
        ("tables".to_owned(), Value::Object(tables)),
    ]));
    let mut output = String::new();
    write_pretty(&document, 0, &mut output);
    output.push('\n');
    output
}

/// Parses and validates one stored unit document.
///
/// # Errors
///
/// Returns typed malformed-medium or version-mismatch failures.
pub fn parse(text: &str, descriptor: &KvUnitDescriptor) -> Result<UnitState, StorageError> {
    let document: Value = serde_json::from_str(text).map_err(|error| {
        StorageError::with_source(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': file is not valid JSON", descriptor.name),
            error.into(),
        )
    })?;
    let object = document.as_object().ok_or_else(|| {
        StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': file is not a JSON object", descriptor.name),
        )
    })?;
    let unit = object.get("unit").and_then(Value::as_object);
    let header_valid = unit.is_some_and(|unit| {
        unit.get("name").and_then(Value::as_str) == Some(descriptor.name.as_str())
            && unit.get("version").is_some_and(Value::is_number)
    });
    if !header_valid {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': missing or foreign unit header", descriptor.name),
        ));
    }
    let Some(version) = unit
        .and_then(|unit| unit.get("version"))
        .and_then(Value::as_number)
    else {
        return Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            format!("unit '{}': missing or foreign unit header", descriptor.name),
        ));
    };
    if !number_equals_u64(version, descriptor.version) {
        return Err(StorageError::new(
            StorageErrorCode::VersionMismatch,
            format!(
                "unit '{}': stored version {} != expected {}",
                descriptor.name,
                javascript_number(version),
                descriptor.version
            ),
        ));
    }
    let stored_tables = object
        .get("tables")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorCode::MalformedMedium,
                format!("unit '{}': tables is not an object", descriptor.name),
            )
        })?;
    let mut tables = IndexMap::new();
    for table in &descriptor.tables {
        let records = match stored_tables.get(table) {
            None => Map::new(),
            Some(Value::Object(records)) => records.clone(),
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
        global: object.get("global").cloned().unwrap_or(Value::Null),
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

fn write_pretty(value: &Value, depth: usize, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&javascript_number(value)),
        Value::String(value) => output.push_str(
            &serde_json::to_string(value).expect("serializing a Rust string cannot fail"),
        ),
        Value::Array(values) => write_array(values, depth, output),
        Value::Object(values) => write_object(values, depth, output),
    }
}

fn write_array(values: &[Value], depth: usize, output: &mut String) {
    if values.is_empty() {
        output.push_str("[]");
        return;
    }
    output.push_str("[\n");
    for (index, value) in values.iter().enumerate() {
        indent(depth + 1, output);
        write_pretty(value, depth + 1, output);
        output.push_str(if index + 1 == values.len() {
            "\n"
        } else {
            ",\n"
        });
    }
    indent(depth, output);
    output.push(']');
}

fn write_object(values: &Map<String, Value>, depth: usize, output: &mut String) {
    if values.is_empty() {
        output.push_str("{}");
        return;
    }
    output.push_str("{\n");
    for (index, (key, value)) in values.iter().enumerate() {
        indent(depth + 1, output);
        output
            .push_str(&serde_json::to_string(key).expect("serializing a Rust string cannot fail"));
        output.push_str(": ");
        write_pretty(value, depth + 1, output);
        output.push_str(if index + 1 == values.len() {
            "\n"
        } else {
            ",\n"
        });
    }
    indent(depth, output);
    output.push('}');
}

fn indent(depth: usize, output: &mut String) {
    for _ in 0..depth {
        output.push_str("  ");
    }
}
