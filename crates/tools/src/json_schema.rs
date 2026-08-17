//! Enforced JSON Schema subset shared by tools, code-mode SDKs, and workflows.

use std::fmt;

use serde_json::{Map, Value};

const SCHEMA_TYPES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];
const CONSTRAINT_KEYWORDS: [&str; 8] = [
    "type",
    "oneOf",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];
const ANNOTATION_KEYWORDS: [&str; 4] = ["description", "title", "default", "examples"];
const ONE_OF_SIBLINGS: [&str; 6] = [
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];

/// Stable code attached to unsupported-schema failures.
pub const UNSUPPORTED_SCHEMA: &str = "UNSUPPORTED_SCHEMA";

/// One validated node in Seekdeep's deliberately small JSON Schema subset.
#[derive(Debug, PartialEq)]
pub struct JsonSchemaNode(Value);

impl JsonSchemaNode {
    /// Returns the original lossless JSON schema.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Consumes the wrapper and returns the lossless JSON schema.
    #[must_use]
    pub fn into_value(mut self) -> Value {
        std::mem::replace(&mut self.0, Value::Null)
    }
}

impl Drop for JsonSchemaNode {
    fn drop(&mut self) {
        let root = std::mem::replace(&mut self.0, Value::Null);
        drop_value_iteratively(root);
    }
}

/// A validated schema whose root is explicitly `type: "object"`.
#[derive(Debug, PartialEq)]
pub struct ObjectJsonSchema(JsonSchemaNode);

impl ObjectJsonSchema {
    /// Returns the validated generic schema.
    #[must_use]
    pub fn as_schema(&self) -> &JsonSchemaNode {
        &self.0
    }

    /// Returns the original lossless JSON schema.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        self.0.as_value()
    }
}

fn drop_value_iteratively(root: Value) {
    let mut pending = vec![root];
    while let Some(mut value) = pending.pop() {
        match &mut value {
            Value::Array(values) => pending.append(values),
            Value::Object(values) => pending.extend(std::mem::take(values).into_values()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

/// Every reason a raw schema fell outside the enforced subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonSchemaError {
    /// Stable machine-readable failure code.
    pub code: &'static str,
    /// Violations in deterministic schema-walk order.
    pub violations: Vec<String>,
}

impl fmt::Display for JsonSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported JSON schema: {}",
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for JsonSchemaError {}

/// Validates a raw JSON value as one supported schema node.
///
/// Annotation-only objects are the canonical unconstrained-JSON schema.
///
/// # Errors
///
/// Returns every unsupported or malformed keyword in deterministic walk order.
pub fn assert_supported_json_schema(schema: Value) -> Result<JsonSchemaNode, JsonSchemaError> {
    check_supported_json_schema(&schema)?;
    Ok(JsonSchemaNode(schema))
}

/// Checks a borrowed raw JSON value without taking ownership.
///
/// # Errors
///
/// Returns every unsupported or malformed keyword in deterministic walk order.
pub fn check_supported_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let violations = schema_violations(schema, false);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError {
            code: UNSUPPORTED_SCHEMA,
            violations,
        })
    }
}

/// Validates the supported subset and requires an object root.
///
/// # Errors
///
/// Returns schema violations, or the structured-output object-root violation.
pub fn assert_object_json_schema(schema: Value) -> Result<ObjectJsonSchema, JsonSchemaError> {
    check_object_json_schema(&schema)?;
    Ok(ObjectJsonSchema(JsonSchemaNode(schema)))
}

/// Checks a borrowed raw schema and its object-root requirement.
///
/// # Errors
///
/// Returns schema violations, or the structured-output object-root violation.
pub fn check_object_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let violations = schema_violations(schema, true);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError {
            code: UNSUPPORTED_SCHEMA,
            violations,
        })
    }
}

enum SchemaTask<'a> {
    Enter(&'a Value, String),
    OneOfTail(&'a Map<String, Value>, String),
    ObjectTail(&'a Map<String, Value>, String),
}

fn schema_violations(schema: &Value, object_root: bool) -> Vec<String> {
    let mut violations = Vec::new();
    let mut tasks = vec![SchemaTask::Enter(schema, "schema".to_owned())];
    while let Some(task) = tasks.pop() {
        match task {
            SchemaTask::OneOfTail(object, path) => {
                for key in ONE_OF_SIBLINGS {
                    if object.contains_key(key) {
                        violations.push(format!("{path}.{key} is not supported beside oneOf"));
                    }
                }
            }
            SchemaTask::ObjectTail(object, path) => {
                check_object_tail(object, &path, &mut violations);
            }
            SchemaTask::Enter(node, path) => {
                let Value::Object(object) = node else {
                    violations.push(format!("{path} must be a schema object"));
                    continue;
                };
                check_schema_object(object, &path, &mut violations, &mut tasks);
            }
        }
    }
    if object_root
        && violations.is_empty()
        && schema
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str)
            != Some("object")
    {
        violations
            .push("schema.type must be \"object\" (structured output is object-rooted)".to_owned());
    }
    violations
}

fn check_schema_object<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    violations: &mut Vec<String>,
    tasks: &mut Vec<SchemaTask<'a>>,
) {
    for (key, value) in object {
        if CONSTRAINT_KEYWORDS.contains(&key.as_str()) {
            continue;
        }
        if ANNOTATION_KEYWORDS.contains(&key.as_str()) {
            if !is_lossless_json_value(value) {
                violations.push(format!(
                    "{path}.{key} annotation must be lossless JSON data"
                ));
            }
            continue;
        }
        violations.push(format!(
            "{path}.{key} is not a supported keyword (subset: type/oneOf/properties/required/additionalProperties/items/enum/const + annotations)"
        ));
    }
    if object.contains_key("description") && !object["description"].is_string() {
        violations.push(format!("{path}.description must be a string"));
    }
    if object.contains_key("title") && !object["title"].is_string() {
        violations.push(format!("{path}.title must be a string"));
    }

    let has_type = object.contains_key("type");
    let has_one_of = object.contains_key("oneOf");
    if has_type && has_one_of {
        violations.push(format!("{path} cannot declare both type and oneOf"));
        return;
    }
    if !has_type && !has_one_of {
        for key in ONE_OF_SIBLINGS {
            if object.contains_key(key) {
                violations.push(format!("{path}.{key} requires type or oneOf"));
            }
        }
        return;
    }
    if has_one_of {
        tasks.push(SchemaTask::OneOfTail(object, path.to_owned()));
        let Some(branches) = object["oneOf"].as_array() else {
            violations.push(format!(
                "{path}.oneOf must be an array of at least two schemas"
            ));
            return;
        };
        if branches.len() < 2 {
            violations.push(format!(
                "{path}.oneOf must be an array of at least two schemas"
            ));
        } else {
            for (index, branch) in branches.iter().enumerate().rev() {
                tasks.push(SchemaTask::Enter(branch, format!("{path}.oneOf[{index}]")));
            }
        }
        return;
    }

    let schema_type = match object["type"].as_str() {
        Some(schema_type) if SCHEMA_TYPES.contains(&schema_type) => schema_type,
        _ if object["type"].is_array() => {
            violations.push(format!(
                "{path}.type must be a single type string (type arrays are not supported)"
            ));
            return;
        }
        _ => {
            violations.push(format!(
                "{path}.type must be one of {}",
                SCHEMA_TYPES.join("/")
            ));
            return;
        }
    };

    check_typed_schema(object, path, schema_type, violations, tasks);
}

fn check_typed_schema<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    schema_type: &str,
    violations: &mut Vec<String>,
    tasks: &mut Vec<SchemaTask<'a>>,
) {
    for (key, types) in [
        ("properties", &["object"][..]),
        ("required", &["object"][..]),
        ("additionalProperties", &["object"][..]),
        ("items", &["array"][..]),
        (
            "enum",
            &["string", "number", "integer", "boolean", "null"][..],
        ),
        (
            "const",
            &["string", "number", "integer", "boolean", "null"][..],
        ),
    ] {
        if object.contains_key(key) && !types.contains(&schema_type) {
            violations.push(format!(
                "{path}.{key} is not supported on type \"{schema_type}\""
            ));
        }
    }

    match schema_type {
        "object" => {
            tasks.push(SchemaTask::ObjectTail(object, path.to_owned()));
            if let Some(properties) = object.get("properties") {
                if let Some(properties) = properties.as_object() {
                    for (key, child) in properties.iter().rev() {
                        tasks.push(SchemaTask::Enter(child, format!("{path}.properties.{key}")));
                    }
                } else {
                    violations.push(format!("{path}.properties must be an object of schemas"));
                }
            }
        }
        "array" => {
            if let Some(items) = object.get("items") {
                tasks.push(SchemaTask::Enter(items, format!("{path}.items")));
            }
        }
        "string" | "number" | "integer" | "boolean" | "null" => {
            check_scalar_schema(object, path, schema_type, violations);
        }
        _ => unreachable!("schema type was validated"),
    }
}

fn check_object_tail(object: &Map<String, Value>, path: &str, violations: &mut Vec<String>) {
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string));
        if valid {
            let declared = object.get("properties").and_then(Value::as_object);
            for key in required.as_array().expect("checked") {
                let key = key.as_str().expect("checked");
                if declared.is_none_or(|properties| !properties.contains_key(key)) {
                    violations.push(format!(
                        "{path}.required names \"{key}\" which is not in properties"
                    ));
                }
            }
        } else {
            violations.push(format!("{path}.required must be an array of strings"));
        }
    }
    if object
        .get("additionalProperties")
        .is_some_and(|value| !value.is_boolean())
    {
        violations.push(format!("{path}.additionalProperties must be a boolean"));
    }
}

fn check_scalar_schema(
    object: &Map<String, Value>,
    path: &str,
    schema_type: &str,
    violations: &mut Vec<String>,
) {
    let valid_enum = object.get("enum").is_some_and(|value| {
        value.as_array().is_some_and(|values| {
            !values.is_empty()
                && values
                    .iter()
                    .all(|value| scalar_matches(schema_type, value))
        })
    });
    if object.contains_key("enum") && !valid_enum {
        violations.push(format!(
            "{path}.enum must be a non-empty array of {schema_type} values"
        ));
    }
    if let Some(constant) = object.get("const") {
        if !scalar_matches(schema_type, constant) {
            violations.push(format!("{path}.const must be a {schema_type} value"));
        } else if valid_enum
            && !object["enum"]
                .as_array()
                .expect("valid enum")
                .iter()
                .any(|allowed| scalar_equal(allowed, constant))
        {
            violations.push(format!(
                "{path}.const must be one of {path}.enum when both are declared"
            ));
        }
    }
}

fn scalar_matches(schema_type: &str, value: &Value) -> bool {
    match schema_type {
        "string" => value.is_string(),
        "number" => value.as_number().is_some_and(is_json_number),
        "integer" => value
            .as_number()
            .is_some_and(|number| is_json_number(number) && number_is_integer(number)),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn is_json_number(number: &serde_json::Number) -> bool {
    number
        .as_f64()
        .is_some_and(|value| value.is_finite() && !(value == 0.0 && value.is_sign_negative()))
}

fn number_is_integer(number: &serde_json::Number) -> bool {
    number.is_i64()
        || number.is_u64()
        || number.as_f64().is_some_and(|number| number.fract() == 0.0)
}

fn scalar_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| left.total_cmp(&right).is_eq()),
        _ => left == right,
    }
}

fn is_lossless_json_value(root: &Value) -> bool {
    let mut pending = vec![root];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) if !is_json_number(number) => return false,
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    true
}

fn diagnostic_path(path: &str) -> &str {
    if path.is_empty() { "arguments" } else { path }
}

fn property_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueFrameKind {
    OneOf,
    Object,
    Array,
}

struct ValueFrame<'a> {
    schema: &'a Value,
    value: &'a Value,
    path: String,
    phase_children: bool,
    kind: Option<ValueFrameKind>,
    children: Vec<OwnedValueChild<'a>>,
    child_index: usize,
    violations: Vec<String>,
    tail_violations: Vec<String>,
    matches: usize,
}

struct OwnedValueChild<'a> {
    schema: &'a Value,
    value: &'a Value,
    path: String,
}

impl<'a> ValueFrame<'a> {
    fn new(schema: &'a Value, value: &'a Value, path: String) -> Self {
        Self {
            schema,
            value,
            path,
            phase_children: false,
            kind: None,
            children: Vec::new(),
            child_index: 0,
            violations: Vec::new(),
            tail_violations: Vec::new(),
            matches: 0,
        }
    }
}

/// Validates a lossless JSON value against an already asserted schema.
///
/// Returns all violations in deterministic walk order. An empty result means valid.
#[must_use]
pub fn validate_json_schema_value(schema: &JsonSchemaNode, value: &Value) -> Vec<String> {
    validate_json_schema_value_at(schema, value, "value")
}

/// Validates a lossless JSON value with a caller-selected diagnostic root.
#[must_use]
pub fn validate_json_schema_value_at(
    schema: &JsonSchemaNode,
    value: &Value,
    path: &str,
) -> Vec<String> {
    check_value(schema.as_value(), value, path)
}

fn check_value<'a>(schema: &'a Value, value: &'a Value, path: &str) -> Vec<String> {
    let mut frames = vec![ValueFrame::new(schema, value, path.to_owned())];
    let mut root_result = None;
    while !frames.is_empty() {
        let index = frames.len() - 1;
        if frames[index].phase_children {
            if frames[index].child_index < frames[index].children.len() {
                let child_index = frames[index].child_index;
                frames[index].child_index += 1;
                let child = &frames[index].children[child_index];
                frames.push(ValueFrame::new(
                    child.schema,
                    child.value,
                    child.path.clone(),
                ));
                continue;
            }
            let frame = frames.pop().expect("frame exists");
            let result = finish_container(frame);
            receive_result(&mut frames, &mut root_result, result);
            continue;
        }

        let (schema, candidate, current_path) = {
            let frame = &frames[index];
            (frame.schema, frame.value, frame.path.clone())
        };
        let object = schema.as_object().expect("schema was asserted");
        if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
            let frame = &mut frames[index];
            frame.kind = Some(ValueFrameKind::OneOf);
            frame.children = branches
                .iter()
                .map(|branch| OwnedValueChild {
                    schema: branch,
                    value: candidate,
                    path: current_path.clone(),
                })
                .collect();
            frame.phase_children = true;
            continue;
        }
        let Some(schema_type) = object.get("type").and_then(Value::as_str) else {
            let result = if is_lossless_json_value(candidate) {
                Vec::new()
            } else {
                vec![format!(
                    "\"{}\" must be a lossless JSON value",
                    diagnostic_path(&current_path)
                )]
            };
            frames.pop();
            receive_result(&mut frames, &mut root_result, result);
            continue;
        };
        match schema_type {
            "object" => start_object(&mut frames[index], object),
            "array" => start_array(&mut frames[index], object),
            "string" | "number" | "integer" | "boolean" | "null" => {
                let result = check_scalar_value(object, candidate, &current_path, schema_type);
                frames.pop();
                receive_result(&mut frames, &mut root_result, result);
            }
            unknown => panic!("unreachable variant in JsonSchemaType: {unknown:?}"),
        }
    }
    root_result.unwrap_or_else(|| {
        vec![format!(
            "\"{}\" must be a lossless JSON value",
            diagnostic_path(path)
        )]
    })
}

fn start_object<'a>(frame: &mut ValueFrame<'a>, schema: &'a Map<String, Value>) {
    let Some(value) = frame.value.as_object() else {
        frame.violations = vec![format!(
            "\"{}\" must be an object",
            diagnostic_path(&frame.path)
        )];
        frame.kind = None;
        frame.phase_children = true;
        return;
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !value.contains_key(key) {
                frame.violations.push(format!(
                    "missing required property \"{}\"",
                    property_path(&frame.path, key)
                ));
            }
        }
    }
    if let Some(properties) = properties {
        for (key, child_schema) in properties {
            if let Some(child_value) = value.get(key) {
                frame.children.push(OwnedValueChild {
                    schema: child_schema,
                    value: child_value,
                    path: property_path(&frame.path, key),
                });
            }
        }
    }
    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for key in value.keys() {
            if properties.is_none_or(|properties| !properties.contains_key(key)) {
                frame.tail_violations.push(format!(
                    "\"{}\" is not a declared property (additionalProperties: false)",
                    property_path(&frame.path, key)
                ));
            }
        }
    }
    frame.kind = Some(ValueFrameKind::Object);
    frame.phase_children = true;
}

fn start_array<'a>(frame: &mut ValueFrame<'a>, schema: &'a Map<String, Value>) {
    let Some(value) = frame.value.as_array() else {
        frame.violations = vec![format!(
            "\"{}\" must be an array",
            diagnostic_path(&frame.path)
        )];
        frame.kind = None;
        frame.phase_children = true;
        return;
    };
    if let Some(items) = schema.get("items") {
        for (index, child_value) in value.iter().enumerate() {
            frame.children.push(OwnedValueChild {
                schema: items,
                value: child_value,
                path: format!("{}[{index}]", frame.path),
            });
        }
    }
    frame.kind = Some(ValueFrameKind::Array);
    frame.phase_children = true;
}

fn finish_container(mut frame: ValueFrame<'_>) -> Vec<String> {
    if frame.kind == Some(ValueFrameKind::OneOf) {
        return if frame.matches == 1 {
            Vec::new()
        } else {
            vec![format!(
                "\"{}\" must match exactly one oneOf branch (matched {})",
                diagnostic_path(&frame.path),
                frame.matches
            )]
        };
    }
    frame.violations.append(&mut frame.tail_violations);
    if frame.violations.is_empty() && !is_lossless_json_value(frame.value) {
        let kind = if frame.kind == Some(ValueFrameKind::Object) {
            "lossless JSON object"
        } else {
            "dense lossless JSON array"
        };
        frame.violations.push(format!(
            "\"{}\" must be a {kind}",
            diagnostic_path(&frame.path)
        ));
    }
    frame.violations
}

fn receive_result(
    frames: &mut [ValueFrame<'_>],
    root_result: &mut Option<Vec<String>>,
    result: Vec<String>,
) {
    if let Some(parent) = frames.last_mut() {
        if parent.kind == Some(ValueFrameKind::OneOf) {
            if result.is_empty() {
                parent.matches += 1;
            }
        } else {
            parent.violations.extend(result);
        }
    } else {
        *root_result = Some(result);
    }
}

fn check_scalar_value(
    schema: &Map<String, Value>,
    value: &Value,
    path: &str,
    schema_type: &str,
) -> Vec<String> {
    let diagnostic = diagnostic_path(path);
    let type_error = match schema_type {
        "string" if !value.is_string() => Some("must be a string"),
        "number" if !value.is_number() => Some("must be a number"),
        "number" if !value.as_number().is_some_and(is_json_number) => {
            Some("must be a finite JSON number")
        }
        "integer"
            if !value
                .as_number()
                .is_some_and(|number| is_json_number(number) && number_is_integer(number)) =>
        {
            Some("must be an integer")
        }
        "boolean" if !value.is_boolean() => Some("must be a boolean"),
        "null" if !value.is_null() => Some("must be null"),
        _ => None,
    };
    if let Some(error) = type_error {
        return vec![format!("\"{diagnostic}\" {error}")];
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|allowed| scalar_equal(allowed, value))
    {
        return vec![format!(
            "\"{diagnostic}\" must be one of {}",
            serde_json::to_string(allowed).expect("JSON values serialize")
        )];
    }
    if let Some(constant) = schema.get("const")
        && !scalar_equal(constant, value)
    {
        return vec![format!(
            "\"{diagnostic}\" must be {}",
            serde_json::to_string(constant).expect("JSON values serialize")
        )];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use serde_json::{Number, json};

    use super::*;

    fn violations(schema: Value) -> Vec<String> {
        assert_supported_json_schema(schema)
            .expect_err("invalid schema")
            .violations
    }

    fn asserted(schema: Value) -> JsonSchemaNode {
        assert_supported_json_schema(schema).expect("supported schema")
    }

    fn validate(schema: Value, value: Value) -> Vec<String> {
        let violations = validate_json_schema_value(&asserted(schema), &value);
        drop(value);
        violations
    }

    fn negative_zero() -> Value {
        Value::Number(Number::from_f64(-0.0).expect("serde number retains negative zero"))
    }

    #[test]
    fn accepts_every_json_root_and_every_supported_node() {
        for schema in [
            json!({"type": "string"}),
            json!({"type": "number"}),
            json!({"type": "integer"}),
            json!({"type": "boolean"}),
            json!({"type": "null"}),
            json!({"type": "array", "items": {"type": "string"}}),
            json!({
                "type": "object",
                "properties": {
                    "nested": {"type": "object", "properties": {}, "additionalProperties": false},
                    "free": {},
                },
                "required": ["nested"],
                "additionalProperties": true,
            }),
            json!({"oneOf": [{"type": "string"}, {"type": "number"}]}),
            json!({
                "description": "any JSON",
                "title": "JSON",
                "default": null,
                "examples": [1, "x"],
            }),
        ] {
            assert_supported_json_schema(schema).expect("supported node");
        }
    }

    #[test]
    fn retains_object_root_guard_only_for_object_consumers() {
        assert_eq!(
            assert_object_json_schema(json!({"type": "object"}))
                .expect("object")
                .as_value()["type"],
            "object"
        );
        for schema in [
            json!({}),
            json!({"type": "string"}),
            json!({"type": "array"}),
            json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
        ] {
            assert_eq!(
                assert_object_json_schema(schema)
                    .expect_err("non-object root")
                    .violations,
                ["schema.type must be \"object\" (structured output is object-rooted)"]
            );
        }
    }

    #[test]
    fn rejects_non_schema_nodes_unknown_types_and_type_arrays() {
        for schema in [Value::Null, json!([]), json!("no")] {
            assert_eq!(violations(schema), ["schema must be a schema object"]);
        }
        assert!(violations(json!({"type": "tuple"}))[0].contains("type must be one of"));
        assert_eq!(
            violations(json!({"type": ["string", "null"]})),
            ["schema.type must be a single type string (type arrays are not supported)"]
        );
    }

    #[test]
    fn enforces_one_of_vocabulary_and_minimum_branch_count() {
        for schema in [
            json!({"oneOf": []}),
            json!({"oneOf": [{}]}),
            json!({"oneOf": "x"}),
        ] {
            assert_eq!(
                violations(schema),
                ["schema.oneOf must be an array of at least two schemas"]
            );
        }
        assert_eq!(
            violations(json!({
                "type": "string",
                "oneOf": [{}, {}],
            })),
            ["schema cannot declare both type and oneOf"]
        );
        assert_eq!(
            violations(json!({
                "oneOf": [{"type": "string"}, {"type": "number"}],
                "items": {},
            })),
            ["schema.items is not supported beside oneOf"]
        );
        assert!(
            violations(json!({
                "oneOf": [{"type": "string"}, {"type": "weird"}],
            }))[0]
                .contains("schema.oneOf[1].type")
        );
        // Rust's `Vec<Value>` structurally excludes sparse, decorated, exotic,
        // and proxy arrays; every representable branch array is dense JSON.
    }

    #[test]
    fn rejects_unknown_and_misplaced_keywords() {
        for keyword in [
            "anyOf",
            "allOf",
            "not",
            "pattern",
            "minimum",
            "maxLength",
            "$ref",
        ] {
            let mut schema =
                Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]);
            schema.insert(keyword.to_owned(), Value::Array(Vec::new()));
            assert!(
                violations(Value::Object(schema))[0]
                    .contains(&format!("schema.{keyword} is not a supported keyword"))
            );
        }
        for (schema, expected) in [
            (
                json!({"type": "object", "items": {}}),
                "schema.items is not supported on type \"object\"",
            ),
            (
                json!({"type": "array", "properties": {}}),
                "schema.properties is not supported on type \"array\"",
            ),
            (
                json!({"type": "object", "enum": ["x"]}),
                "schema.enum is not supported on type \"object\"",
            ),
            (
                json!({"type": "array", "const": null}),
                "schema.const is not supported on type \"array\"",
            ),
        ] {
            assert_eq!(violations(schema), [expected]);
        }
        assert_eq!(
            violations(json!({
                "properties": {},
                "required": [],
                "additionalProperties": true,
                "items": {},
                "enum": [],
                "const": null,
            })),
            [
                "schema.properties requires type or oneOf",
                "schema.required requires type or oneOf",
                "schema.additionalProperties requires type or oneOf",
                "schema.items requires type or oneOf",
                "schema.enum requires type or oneOf",
                "schema.const requires type or oneOf",
            ]
        );
    }

    #[test]
    fn reports_every_independent_schema_violation() {
        assert_eq!(
            violations(json!({
                "type": "object",
                "pattern": "x",
                "properties": {
                    "a": {"type": "weird"},
                    "b": {"type": "string", "minimum": 1}
                }
            }))
            .len(),
            3
        );
    }

    #[test]
    fn validates_object_properties_required_names_and_openness() {
        assert_eq!(
            violations(json!({"type": "object", "properties": []})),
            ["schema.properties must be an object of schemas"]
        );
        assert_eq!(
            violations(json!({"type": "object", "properties": {"a": "x"}})),
            ["schema.properties.a must be a schema object"]
        );
        for required in [json!("a"), json!([1])] {
            assert_eq!(
                violations(json!({"type": "object", "required": required})),
                ["schema.required must be an array of strings"]
            );
        }
        assert_eq!(
            violations(json!({
                "type": "object",
                "properties": {},
                "required": ["missing"],
            })),
            ["schema.required names \"missing\" which is not in properties"]
        );
        assert_eq!(
            violations(json!({"type": "object", "additionalProperties": "yes"})),
            ["schema.additionalProperties must be a boolean"]
        );
        // `undefined` and sparse arrays are unrepresentable in `Value`; an
        // explicit null remains present and is rejected with the same field diagnostic.
        assert_eq!(
            violations(json!({"type": "object", "properties": null})),
            ["schema.properties must be an object of schemas"]
        );
    }

    #[test]
    fn requires_type_correct_scalar_enum_and_const_values() {
        for schema in [
            json!({"type": "string", "enum": ["a"], "const": "a"}),
            json!({"type": "number", "enum": [1.5], "const": 1.5}),
            json!({"type": "integer", "enum": [1], "const": 1}),
            json!({"type": "boolean", "enum": [true], "const": true}),
            json!({"type": "null", "enum": [null], "const": null}),
        ] {
            assert_supported_json_schema(schema).expect("valid scalar literals");
        }
        for (schema, expected) in [
            (
                json!({"type": "string", "enum": []}),
                "schema.enum must be a non-empty array of string values",
            ),
            (
                json!({"type": "number", "enum": ["1"]}),
                "schema.enum must be a non-empty array of number values",
            ),
            (
                json!({"type": "integer", "enum": [1.5]}),
                "schema.enum must be a non-empty array of integer values",
            ),
            (
                json!({"type": "boolean", "const": 1}),
                "schema.const must be a boolean value",
            ),
            (
                json!({"type": "string", "enum": ["a"], "const": "b"}),
                "schema.const must be one of schema.enum when both are declared",
            ),
        ] {
            assert_eq!(violations(schema), [expected]);
        }
        let mut negative_const = Map::from_iter([
            ("type".to_owned(), json!("number")),
            ("const".to_owned(), negative_zero()),
        ]);
        assert_eq!(
            violations(Value::Object(std::mem::take(&mut negative_const))),
            ["schema.const must be a number value"]
        );
        assert!(Number::from_f64(f64::NAN).is_none());
    }

    #[test]
    fn validates_annotation_types_and_lossless_json_payloads() {
        assert_eq!(
            violations(json!({"description": 1})),
            ["schema.description must be a string"]
        );
        assert_eq!(
            violations(json!({"title": 1})),
            ["schema.title must be a string"]
        );
        let mut schema = Map::new();
        schema.insert("default".to_owned(), negative_zero());
        assert_eq!(
            violations(Value::Object(schema)),
            ["schema.default annotation must be lossless JSON data"]
        );
        // `Value` excludes undefined, dates, getters, hidden/symbol keys, and cycles.
    }

    #[test]
    fn accepts_lossless_annotation_containers_independent_of_origin() {
        assert_supported_json_schema(json!({
            "type": "object",
            "properties": {"value": {"type": "string", "enum": ["x"]}},
            "required": ["value"],
            "default": {"x": 1},
            "examples": [[{"ok": true}]],
        }))
        .expect("lossless annotations");
    }

    #[test]
    fn structural_value_boundary_excludes_exotic_schemas_and_permits_reuse() {
        let leaf = json!({"type": "string"});
        assert_supported_json_schema(json!({
            "type": "object",
            "properties": {"a": leaf.clone(), "b": leaf},
        }))
        .expect("sibling reuse by value");
        assert_eq!(
            violations(json!({"type": "object", "properties": []})),
            ["schema.properties must be an object of schemas"]
        );
        assert_eq!(
            violations(json!({"type": "object", "properties": {"at": "date"}})),
            ["schema.properties.at must be a schema object"]
        );
        // Cycles, prototypes, maps, proxies, non-enumerable and symbol keys
        // cannot inhabit serde_json::Value, making this boundary stricter by type.
    }

    #[test]
    fn asserts_deeply_nested_raw_unions_without_the_call_stack() {
        let schema = deep_union(5_000);
        assert_supported_json_schema(schema).expect("deep schema");
    }

    #[test]
    fn map_lookup_has_own_property_semantics_for_required_declarations() {
        assert_eq!(
            violations(json!({
                "type": "object",
                "properties": {},
                "required": ["toString"],
            })),
            ["schema.required names \"toString\" which is not in properties"]
        );
    }

    #[test]
    fn validates_scalar_array_object_and_null_roots() {
        for (schema, value) in [
            (json!({"type": "string"}), json!("x")),
            (json!({"type": "number"}), json!(1.5)),
            (json!({"type": "integer"}), json!(2)),
            (json!({"type": "boolean"}), json!(true)),
            (json!({"type": "null"}), Value::Null),
            (
                json!({"type": "array", "items": {"type": "string"}}),
                json!(["x"]),
            ),
            (json!({"type": "object"}), json!({"x": 1})),
        ] {
            assert!(validate(schema, value).is_empty());
        }
    }

    #[test]
    fn rejects_wrong_scalar_types_and_lossy_numbers() {
        for (schema, value, expected) in [
            (
                json!({"type": "string"}),
                json!(1),
                "\"value\" must be a string",
            ),
            (
                json!({"type": "number"}),
                json!("1"),
                "\"value\" must be a number",
            ),
            (
                json!({"type": "number"}),
                negative_zero(),
                "\"value\" must be a finite JSON number",
            ),
            (
                json!({"type": "integer"}),
                json!(1.5),
                "\"value\" must be an integer",
            ),
            (
                json!({"type": "boolean"}),
                json!("true"),
                "\"value\" must be a boolean",
            ),
            (json!({"type": "null"}), json!(0), "\"value\" must be null"),
        ] {
            assert_eq!(validate(schema, value), [expected]);
        }
    }

    #[test]
    fn enforces_scalar_enum_and_const_together() {
        let schema = asserted(json!({
            "type": "string",
            "enum": ["a", "b"],
            "const": "a",
        }));
        assert!(validate_json_schema_value(&schema, &json!("a")).is_empty());
        assert_eq!(
            validate_json_schema_value(&schema, &json!("c")),
            ["\"value\" must be one of [\"a\",\"b\"]"]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!("b")),
            ["\"value\" must be \"a\""]
        );
    }

    #[test]
    fn validates_object_requiredness_nested_values_and_open_defaults() {
        let schema = asserted(json!({
            "type": "object",
            "properties": {
                "file": {"type": "string"},
                "nested": {
                    "type": "object",
                    "properties": {"line": {"type": "integer"}},
                    "required": ["line"],
                    "additionalProperties": false,
                },
            },
            "required": ["file"],
        }));
        assert!(
            validate_json_schema_value(
                &schema,
                &json!({"file": "a", "extra": [1], "nested": {"line": 2}})
            )
            .is_empty()
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!({"nested": {"line": 1}})),
            ["missing required property \"value.file\""]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!({"file": 1, "nested": {}})),
            [
                "\"value.file\" must be a string",
                "missing required property \"value.nested.line\"",
            ]
        );
        assert_eq!(
            validate_json_schema_value(
                &schema,
                &json!({"file": "a", "nested": {"line": 1, "extra": true}})
            ),
            ["\"value.nested.extra\" is not a declared property (additionalProperties: false)"]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!("x")),
            ["\"value\" must be an object"]
        );
    }

    #[test]
    fn rust_value_excludes_present_undefined_and_lossy_objects() {
        let required = asserted(json!({
            "type": "object",
            "properties": {"x": {}},
            "required": ["x"],
        }));
        assert_eq!(
            validate_json_schema_value(&required, &json!({})),
            ["missing required property \"value.x\""]
        );
        // A `Value::Object` is intrinsically plain and cannot contain undefined or a Date.
        assert!(validate(json!({"type": "object"}), json!({"x": null})).is_empty());
    }

    #[test]
    fn validation_is_total_for_every_structural_json_container() {
        let schema = asserted(json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
        }));
        assert!(validate_json_schema_value(&schema, &json!({"answer": 42})).is_empty());
        // Hostile getters cannot inhabit an owned serde_json::Map.
    }

    #[test]
    fn validates_dense_arrays_per_index() {
        let schema = asserted(json!({"type": "array", "items": {"type": "integer"}}));
        assert!(validate_json_schema_value(&schema, &json!([1, 2])).is_empty());
        assert_eq!(
            validate_json_schema_value(&schema, &json!([1, 1.5])),
            ["\"value[1]\" must be an integer"]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!("x")),
            ["\"value\" must be an array"]
        );
        // `Vec<Value>` is always dense and has no JSON-invisible decorations.
    }

    #[test]
    fn validates_exact_one_one_of_semantics_including_overlap() {
        let disjoint = asserted(json!({
            "oneOf": [{"type": "string"}, {"type": "number"}],
        }));
        assert!(validate_json_schema_value(&disjoint, &json!("x")).is_empty());
        assert_eq!(
            validate_json_schema_value(&disjoint, &Value::Null),
            ["\"value\" must match exactly one oneOf branch (matched 0)"]
        );

        let overlap = asserted(json!({
            "oneOf": [{"type": "number"}, {"type": "integer"}]
        }));
        assert_eq!(
            validate_json_schema_value(&overlap, &json!(1)),
            ["\"value\" must match exactly one oneOf branch (matched 2)"]
        );
        assert!(validate_json_schema_value(&overlap, &json!(1.5)).is_empty());
    }

    #[test]
    fn validates_deeply_nested_unions_without_the_call_stack() {
        let schema = assert_supported_json_schema(deep_union(5_000)).expect("deep schema");
        assert!(validate_json_schema_value(&schema, &json!("leaf")).is_empty());
        assert_eq!(
            validate_json_schema_value(&schema, &json!(42)),
            ["\"value\" must match exactly one oneOf branch (matched 0)"]
        );
    }

    #[test]
    fn unconstrained_schema_accepts_every_structural_lossless_json_value() {
        let schema = asserted(json!({}));
        for value in [
            Value::Null,
            json!(true),
            json!(1),
            json!("x"),
            json!([1]),
            json!({"x": null}),
        ] {
            assert!(validate_json_schema_value(&schema, &value).is_empty());
        }
        assert_eq!(
            validate_json_schema_value(&schema, &negative_zero()),
            ["\"value\" must be a lossless JSON value"]
        );
        // Undefined, functions, maps, proxy traps, and cyclic graphs are not Values.
    }

    #[test]
    fn map_semantics_are_always_own_for_required_recursion_and_closed_checks() {
        assert_eq!(
            validate(
                json!({
                    "type": "object",
                    "properties": {"toString": {"type": "string"}},
                    "required": ["toString"],
                }),
                json!({}),
            ),
            ["missing required property \"value.toString\""]
        );
        assert_eq!(
            validate(
                json!({"type": "object", "additionalProperties": false}),
                json!({"toString": 1}),
            ),
            ["\"value.toString\" is not a declared property (additionalProperties: false)"]
        );
        assert!(
            validate(
                json!({
                    "type": "object",
                    "properties": {"constructor": {"type": "string"}},
                }),
                json!({}),
            )
            .is_empty()
        );
    }

    #[test]
    fn forged_schema_backstop_panics_on_an_unreachable_type() {
        let forged = JsonSchemaNode(json!({"type": "tuple"}));
        let panic = std::panic::catch_unwind(|| {
            let _ = validate_json_schema_value(&forged, &json!(1));
        })
        .expect_err("forged type must hit exhaustive backstop");
        let message = panic.downcast_ref::<String>().map_or_else(
            || panic.downcast_ref::<&str>().map_or("", |message| *message),
            String::as_str,
        );
        assert!(message.contains("tuple"));
    }

    fn deep_union(depth: usize) -> Value {
        let mut schema = json!({"type": "string"});
        for _ in 0..depth {
            let mut object = Map::new();
            object.insert(
                "oneOf".to_owned(),
                Value::Array(vec![schema, json!({"type": "null"})]),
            );
            schema = Value::Object(object);
        }
        schema
    }
}
