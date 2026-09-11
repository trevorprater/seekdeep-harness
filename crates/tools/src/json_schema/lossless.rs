//! Validation of JSON snapshots whose strings need not be Unicode scalars.

use seekdeep_code_runtime::{
    CodeJsonString, CodeJsonValue,
    json::{CodeJsonRef, CodeJsonToken},
};
use serde_json::{Map, Value};

use super::JsonSchemaNode;

/// Validates a complete JSON snapshot against the supported tool schema subset.
#[must_use]
pub fn validate_code_json_schema_value_at(
    schema: &JsonSchemaNode,
    value: &CodeJsonValue,
    path: &str,
) -> Vec<CodeJsonString> {
    if let Some(value) = value.as_serde_json() {
        return super::validate_json_schema_value_at(schema, value, path)
            .into_iter()
            .map(CodeJsonString::from)
            .collect();
    }
    check_raw_value(schema, value, path)
}

fn check_raw_value(
    schema: &JsonSchemaNode,
    value: &CodeJsonValue,
    path: &str,
) -> Vec<CodeJsonString> {
    let mut frames = vec![Frame::new(schema.as_value(), value.as_ref(), path.into())];
    let mut result = Vec::new();
    while !frames.is_empty() {
        let index = frames.len() - 1;
        if !frames[index].started {
            frames[index].start();
        }
        if let Some((schema, value, path)) = frames[index].children.pop() {
            frames.push(Frame::new(schema, value, path));
            continue;
        }
        let frame = frames.pop().expect("validation frame exists");
        let violations = frame.finish();
        if let Some(parent) = frames.last_mut() {
            if parent.one_of {
                parent.matches += usize::from(violations.is_empty());
            } else {
                parent.violations.extend(violations);
            }
        } else {
            result = violations;
        }
    }
    result
}

struct Frame<'a> {
    schema: &'a Value,
    value: CodeJsonRef<'a>,
    path: CodeJsonString,
    children: Vec<(&'a Value, CodeJsonRef<'a>, CodeJsonString)>,
    violations: Vec<CodeJsonString>,
    tail: Vec<CodeJsonString>,
    invalid_container: Option<&'static str>,
    started: bool,
    one_of: bool,
    matches: usize,
}

impl<'a> Frame<'a> {
    fn new(schema: &'a Value, value: CodeJsonRef<'a>, path: CodeJsonString) -> Self {
        Self {
            schema,
            value,
            path,
            children: Vec::new(),
            violations: Vec::new(),
            tail: Vec::new(),
            invalid_container: None,
            started: false,
            one_of: false,
            matches: 0,
        }
    }

    fn start(&mut self) {
        self.started = true;
        let object = self.schema.as_object().expect("tool schema was asserted");
        if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
            self.one_of = true;
            self.children = branches
                .iter()
                .rev()
                .map(|schema| (schema, self.value, self.path.clone()))
                .collect();
            return;
        }
        match object.get("type").and_then(Value::as_str) {
            None => {
                if !lossless_numbers(self.value) {
                    self.violations
                        .push(issue(&self.path, "must be a lossless JSON value"));
                }
            }
            Some("object") => self.object(object),
            Some("array") => self.array(object),
            Some(kind @ ("string" | "number" | "integer" | "boolean" | "null")) => {
                self.violations = scalar(object, self.value, &self.path, kind);
            }
            Some(unknown) => unreachable!("unsupported validated JSON schema type {unknown}"),
        }
    }

    fn object(&mut self, schema: &'a Map<String, Value>) {
        let Some(entries) = self.value.object_entries() else {
            self.violations.push(issue(&self.path, "must be an object"));
            return;
        };
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if self.value.get(key).is_none() {
                    let mut message = CodeJsonString::from("missing required property \"");
                    message.push_utf16(property_path(&self.path, &key.into()).utf16_units());
                    message.push_str("\"");
                    self.violations.push(message);
                }
            }
        }
        if let Some(properties) = properties {
            for (key, schema) in properties.iter().rev() {
                if let Some(value) = self.value.get(key) {
                    self.children.push((
                        schema,
                        value,
                        property_path(&self.path, &key.as_str().into()),
                    ));
                }
            }
        }
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            for (key, _) in entries {
                let units = key.to_utf16().expect("object key is a JSON string");
                let key = String::from_utf16(&units);
                if key.as_ref().ok().is_none_or(|key| {
                    properties.is_none_or(|properties| !properties.contains_key(key))
                }) {
                    let key = CodeJsonString::from_utf16(&units);
                    self.tail.push(issue(
                        &property_path(&self.path, &key),
                        "is not a declared property (additionalProperties: false)",
                    ));
                }
            }
        }
        if !lossless_numbers(self.value) {
            self.invalid_container = Some("lossless JSON object");
        }
    }

    fn array(&mut self, schema: &'a Map<String, Value>) {
        let Some(items) = self.value.array_items() else {
            self.violations.push(issue(&self.path, "must be an array"));
            return;
        };
        if let Some(schema) = schema.get("items") {
            self.children = items
                .into_iter()
                .enumerate()
                .rev()
                .map(|(index, value)| {
                    let mut path = self.path.clone();
                    path.push_str(&format!("[{index}]"));
                    (schema, value, path)
                })
                .collect();
        }
        if !lossless_numbers(self.value) {
            self.invalid_container = Some("dense lossless JSON array");
        }
    }

    fn finish(mut self) -> Vec<CodeJsonString> {
        if self.one_of {
            return if self.matches == 1 {
                Vec::new()
            } else {
                vec![issue(
                    &self.path,
                    &format!(
                        "must match exactly one oneOf branch (matched {})",
                        self.matches
                    ),
                )]
            };
        }
        self.violations.append(&mut self.tail);
        if self.violations.is_empty()
            && let Some(kind) = self.invalid_container
        {
            self.violations
                .push(issue(&self.path, &format!("must be a {kind}")));
        }
        self.violations
    }
}

fn lossless_numbers(value: CodeJsonRef<'_>) -> bool {
    value.tokens().all(|token| {
        let CodeJsonToken::Scalar(value) = token else {
            return true;
        };
        value.as_f64().is_none_or(|number| {
            number.is_finite() && !(number == 0.0 && number.is_sign_negative())
        })
    })
}

fn scalar(
    schema: &Map<String, Value>,
    value: CodeJsonRef<'_>,
    path: &CodeJsonString,
    kind: &str,
) -> Vec<CodeJsonString> {
    let number = value.as_f64();
    let valid_number = number
        .is_some_and(|number| number.is_finite() && !(number == 0.0 && number.is_sign_negative()));
    let error = match kind {
        "string" if !value.is_string() => Some("must be a string"),
        "number" if number.is_none() => Some("must be a number"),
        "number" if !valid_number => Some("must be a finite JSON number"),
        "integer" if !valid_number || number.is_none_or(|number| number.fract() != 0.0) => {
            Some("must be an integer")
        }
        "boolean" if value.as_bool().is_none() => Some("must be a boolean"),
        "null" if !value.is_null() => Some("must be null"),
        _ => None,
    };
    if let Some(error) = error {
        return vec![issue(path, error)];
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|allowed| scalar_equal(allowed, value))
    {
        return vec![issue(
            path,
            &format!(
                "must be one of {}",
                serde_json::to_string(allowed).expect("validated schema serializes")
            ),
        )];
    }
    if let Some(constant) = schema.get("const")
        && !scalar_equal(constant, value)
    {
        return vec![issue(
            path,
            &format!(
                "must be {}",
                serde_json::to_string(constant).expect("validated schema serializes")
            ),
        )];
    }
    Vec::new()
}

fn issue(path: &CodeJsonString, suffix: &str) -> CodeJsonString {
    let mut message = CodeJsonString::from("\"");
    if path.is_empty() {
        message.push_str("arguments");
    } else {
        message.push_utf16(path.utf16_units());
    }
    message.push_str("\" ");
    message.push_str(suffix);
    message
}

fn property_path(path: &CodeJsonString, key: &CodeJsonString) -> CodeJsonString {
    let mut path = path.clone();
    if !path.is_empty() {
        path.push_str(".");
    }
    path.push_utf16(key.utf16_units());
    path
}

fn scalar_equal(left: &Value, right: CodeJsonRef<'_>) -> bool {
    match left {
        Value::String(left) => right
            .to_utf16()
            .is_some_and(|right| left.encode_utf16().eq(right)),
        Value::Number(left) => left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| left.total_cmp(&right).is_eq()),
        Value::Bool(left) => right.as_bool() == Some(*left),
        Value::Null => right.is_null(),
        Value::Array(_) | Value::Object(_) => unreachable!("enum and const schemas are scalar"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_surrogate_strings_and_keys_without_confusing_literal_escapes() {
        let value =
            CodeJsonValue::parse(r#"{"payload":{"\ud800":["\udfff","😀","\\ud800"]}}"#.to_owned())
                .unwrap();
        let schema = super::super::assert_supported_json_schema(json!({
            "type":"object", "required":["payload"], "additionalProperties":false,
            "properties":{"payload":{"oneOf":[{"type":"object"},{"type":"string"}]}}
        }))
        .unwrap();
        assert!(validate_code_json_schema_value_at(&schema, &value, "arguments").is_empty());
        let string = CodeJsonValue::parse(r#""\ud800""#.to_owned()).unwrap();
        let schema = super::super::assert_supported_json_schema(json!({
            "type":"string", "enum":["\\ud800", "�"]
        }))
        .unwrap();
        assert_eq!(
            validate_code_json_schema_value_at(&schema, &string, "value"),
            [r#""value" must be one of ["\\ud800","�"]"#]
        );
    }

    #[test]
    fn preserves_required_type_union_and_numeric_rejections_with_surrogate_siblings() {
        let schema = super::super::assert_supported_json_schema(json!({
            "type":"object", "required":["missing"], "properties":{
                "missing":{"type":"string"},
                "count":{"type":"integer"},
                "choice":{"oneOf":[{"type":"string"},{"type":"string"}]}
            }, "additionalProperties":false
        }))
        .unwrap();
        let value =
            CodeJsonValue::parse(r#"{"count":"\ud800","choice":"\udfff","extra":true}"#.to_owned())
                .unwrap();
        assert_eq!(
            validate_code_json_schema_value_at(&schema, &value, "arguments"),
            [
                "missing required property \"arguments.missing\"",
                "\"arguments.count\" must be an integer",
                "\"arguments.choice\" must match exactly one oneOf branch (matched 2)",
                "\"arguments.extra\" is not a declared property (additionalProperties: false)",
            ]
        );
        let schema = super::super::assert_supported_json_schema(json!({})).unwrap();
        let value = CodeJsonValue::parse(r#"["\ud800",-0]"#.to_owned()).unwrap();
        assert_eq!(
            validate_code_json_schema_value_at(&schema, &value, "value"),
            ["\"value\" must be a lossless JSON value"]
        );
    }
}
