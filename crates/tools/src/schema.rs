//! Author-facing value-schema DSL and its enforced JSON Schema projection.

use std::fmt;

use serde_json::{Map, Value};

use crate::json_schema::{
    JsonSchemaError, JsonSchemaNode, assert_supported_json_schema, validate_json_schema_value_at,
};

const ANNOTATION_KEYS: [&str; 4] = ["description", "title", "default", "examples"];

/// Invalid model-generated arguments for a typed tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolArgsError {
    /// Individual violations in schema-walk order.
    pub violations: Vec<String>,
}

impl fmt::Display for ToolArgsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid arguments: {}",
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for ToolArgsError {}

#[derive(Debug)]
enum CompiledKind {
    Json,
    Scalar {
        schema_type: String,
        enumeration: Option<Value>,
        constant: Option<Value>,
    },
    Array {
        items: Option<usize>,
    },
    Object {
        properties: Option<Vec<(String, usize, bool)>>,
        additional_properties: bool,
    },
    OneOf {
        branches: Vec<usize>,
    },
    ParameterRoot {
        properties: Vec<(String, usize, bool)>,
    },
}

#[derive(Debug)]
struct CompiledNode {
    annotations: Map<String, Value>,
    kind: CompiledKind,
}

struct CompileTask {
    id: usize,
    input: Value,
    path: String,
    allow_required: bool,
}

/// Compiles one author-facing value-schema JSON object into the enforced subset.
///
/// The author-only `type: "json"` node projects to an annotation-only schema.
///
/// # Errors
///
/// Returns author-vocabulary or enforced-subset violations.
pub fn value_schema_spec_to_json_schema(spec: Value) -> Result<JsonSchemaNode, JsonSchemaError> {
    let mut nodes = vec![CompiledNode {
        annotations: Map::new(),
        kind: CompiledKind::Json,
    }];
    compile_tasks(
        &mut nodes,
        vec![CompileTask {
            id: 0,
            input: spec,
            path: "schema".to_owned(),
            allow_required: false,
        }],
    )?;
    assert_supported_json_schema(assemble(nodes, 0))
}

/// Compiles an implicit open parameter-object map into the enforced subset.
///
/// # Errors
///
/// Returns author-vocabulary or enforced-subset violations.
pub fn parameter_schema_spec_to_json_schema(
    spec: Value,
) -> Result<JsonSchemaNode, JsonSchemaError> {
    let Value::Object(properties) = spec else {
        return author_error("parameters must be an object of value schemas");
    };
    let mut nodes = vec![CompiledNode {
        annotations: Map::new(),
        kind: CompiledKind::ParameterRoot {
            properties: Vec::new(),
        },
    }];
    let mut tasks = Vec::with_capacity(properties.len());
    for (key, property) in properties {
        let required = property
            .as_object()
            .and_then(|object| object.get("required"))
            .is_some_and(|value| value == &Value::Bool(true));
        if property
            .as_object()
            .is_some_and(|object| object.contains_key("required") && !required)
        {
            return author_error(format!(
                "parameters.{key}.required must be true when present"
            ));
        }
        let id = allocate(&mut nodes);
        let CompiledKind::ParameterRoot { properties } = &mut nodes[0].kind else {
            unreachable!("root kind")
        };
        properties.push((key.clone(), id, required));
        tasks.push(CompileTask {
            id,
            input: property,
            path: format!("parameters.{key}"),
            allow_required: true,
        });
    }
    tasks.reverse();
    compile_tasks(&mut nodes, tasks)?;
    assert_supported_json_schema(assemble(nodes, 0))
}

/// Validates model-generated arguments against an author parameter schema.
///
/// # Errors
///
/// Returns author-schema compilation errors. Argument violations are returned as data.
pub fn validate_args(spec: Value, args: &Value) -> Result<Vec<String>, JsonSchemaError> {
    let schema = parameter_schema_spec_to_json_schema(spec)?;
    Ok(validate_json_schema_value_at(&schema, args, ""))
}

fn allocate(nodes: &mut Vec<CompiledNode>) -> usize {
    let id = nodes.len();
    nodes.push(CompiledNode {
        annotations: Map::new(),
        kind: CompiledKind::Json,
    });
    id
}

fn compile_tasks(
    nodes: &mut Vec<CompiledNode>,
    mut tasks: Vec<CompileTask>,
) -> Result<(), JsonSchemaError> {
    while let Some(task) = tasks.pop() {
        compile_one(nodes, &mut tasks, task)?;
    }
    Ok(())
}

fn compile_one(
    nodes: &mut Vec<CompiledNode>,
    tasks: &mut Vec<CompileTask>,
    task: CompileTask,
) -> Result<(), JsonSchemaError> {
    let CompileTask {
        id,
        input,
        path,
        allow_required,
    } = task;
    let Value::Object(mut input) = input else {
        return author_error(format!("{path} must be a value schema object"));
    };
    let annotations = take_annotations(&mut input);
    if input.contains_key("oneOf") {
        return compile_one_of(nodes, tasks, id, &path, allow_required, input, annotations);
    }
    let input_type = input.get("type").and_then(Value::as_str).map(str::to_owned);
    match input_type.as_deref() {
        Some("json") => {
            assert_keys(&input, &path, &allowed(allow_required, &["type"]))?;
            nodes[id] = CompiledNode {
                annotations,
                kind: CompiledKind::Json,
            };
        }
        Some("object") => {
            compile_object(nodes, tasks, id, &path, allow_required, input, annotations)?;
        }
        Some("array") => {
            compile_array(nodes, tasks, id, &path, allow_required, input, annotations)?;
        }
        Some("string" | "number" | "integer" | "boolean" | "null") => {
            assert_keys(
                &input,
                &path,
                &allowed(allow_required, &["type", "enum", "const"]),
            )?;
            let schema_type = input_type.expect("matched type");
            let enumeration = input.remove("enum");
            let constant = input.remove("const");
            nodes[id] = CompiledNode {
                annotations,
                kind: CompiledKind::Scalar {
                    schema_type,
                    enumeration,
                    constant,
                },
            };
        }
        _ => {
            return author_error(format!(
                "{path}.type must be string/number/integer/boolean/null/array/object/json, or use oneOf"
            ));
        }
    }
    Ok(())
}

fn compile_one_of(
    nodes: &mut Vec<CompiledNode>,
    tasks: &mut Vec<CompileTask>,
    id: usize,
    path: &str,
    allow_required: bool,
    mut input: Map<String, Value>,
    annotations: Map<String, Value>,
) -> Result<(), JsonSchemaError> {
    assert_keys(&input, path, &allowed(allow_required, &["oneOf", "type"]))?;
    if input.contains_key("type") {
        return author_error(format!("{path} cannot declare both type and oneOf"));
    }
    let branches = match input.remove("oneOf") {
        Some(Value::Array(branches)) if branches.len() >= 2 => branches,
        _ => {
            return author_error(format!(
                "{path}.oneOf must be an array of at least two value schemas"
            ));
        }
    };
    let mut ids = Vec::with_capacity(branches.len());
    let mut child_tasks = Vec::with_capacity(branches.len());
    for (index, branch) in branches.into_iter().enumerate() {
        let id = allocate(nodes);
        ids.push(id);
        child_tasks.push(CompileTask {
            id,
            input: branch,
            path: format!("{path}.oneOf[{index}]"),
            allow_required: false,
        });
    }
    nodes[id] = CompiledNode {
        annotations,
        kind: CompiledKind::OneOf { branches: ids },
    };
    tasks.extend(child_tasks.into_iter().rev());
    Ok(())
}

fn compile_object(
    nodes: &mut Vec<CompiledNode>,
    tasks: &mut Vec<CompileTask>,
    id: usize,
    path: &str,
    allow_required: bool,
    mut input: Map<String, Value>,
    annotations: Map<String, Value>,
) -> Result<(), JsonSchemaError> {
    assert_keys(
        &input,
        path,
        &allowed(
            allow_required,
            &["type", "properties", "additionalProperties"],
        ),
    )?;
    let Some(additional_properties) = input
        .remove("additionalProperties")
        .and_then(|v| v.as_bool())
    else {
        return author_error(format!(
            "{path}.additionalProperties must be explicitly true or false"
        ));
    };
    let properties = match input.remove("properties") {
        None => None,
        Some(Value::Object(properties)) => Some(properties),
        Some(_) => {
            return author_error(format!(
                "{path}.properties must be an object of value schemas"
            ));
        }
    };
    let mut compiled_properties = properties
        .as_ref()
        .map(|properties| Vec::with_capacity(properties.len()));
    let mut child_tasks = Vec::with_capacity(properties.as_ref().map_or(0, Map::len));
    for (key, property) in properties.into_iter().flatten() {
        let required = property
            .as_object()
            .and_then(|object| object.get("required"))
            .is_some_and(|value| value == &Value::Bool(true));
        if property
            .as_object()
            .is_some_and(|object| object.contains_key("required") && !required)
        {
            return author_error(format!(
                "{path}.properties.{key}.required must be true when present"
            ));
        }
        let id = allocate(nodes);
        compiled_properties
            .as_mut()
            .expect("present property map")
            .push((key.clone(), id, required));
        child_tasks.push(CompileTask {
            id,
            input: property,
            path: format!("{path}.properties.{key}"),
            allow_required: true,
        });
    }
    nodes[id] = CompiledNode {
        annotations,
        kind: CompiledKind::Object {
            properties: compiled_properties,
            additional_properties,
        },
    };
    tasks.extend(child_tasks.into_iter().rev());
    Ok(())
}

fn compile_array(
    nodes: &mut Vec<CompiledNode>,
    tasks: &mut Vec<CompileTask>,
    id: usize,
    path: &str,
    allow_required: bool,
    mut input: Map<String, Value>,
    annotations: Map<String, Value>,
) -> Result<(), JsonSchemaError> {
    assert_keys(&input, path, &allowed(allow_required, &["type", "items"]))?;
    let items = input.remove("items").map(|item| {
        let id = allocate(nodes);
        tasks.push(CompileTask {
            id,
            input: item,
            path: format!("{path}.items"),
            allow_required: false,
        });
        id
    });
    nodes[id] = CompiledNode {
        annotations,
        kind: CompiledKind::Array { items },
    };
    Ok(())
}

fn allowed(allow_required: bool, node_keys: &[&str]) -> Vec<&'static str> {
    let mut keys = ANNOTATION_KEYS.to_vec();
    if allow_required {
        keys.push("required");
    }
    for key in node_keys {
        keys.push(match *key {
            "type" => "type",
            "oneOf" => "oneOf",
            "properties" => "properties",
            "additionalProperties" => "additionalProperties",
            "items" => "items",
            "enum" => "enum",
            "const" => "const",
            _ => unreachable!("static vocabulary"),
        });
    }
    keys
}

fn assert_keys(
    input: &Map<String, Value>,
    path: &str,
    allowed: &[&str],
) -> Result<(), JsonSchemaError> {
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            return author_error(format!(
                "{path}.{key} is not supported by the value schema DSL"
            ));
        }
    }
    Ok(())
}

fn take_annotations(input: &mut Map<String, Value>) -> Map<String, Value> {
    let mut annotations = Map::new();
    for key in ANNOTATION_KEYS {
        if let Some(value) = input.remove(key) {
            annotations.insert(key.to_owned(), value);
        }
    }
    annotations
}

fn author_error<T>(message: impl Into<String>) -> Result<T, JsonSchemaError> {
    Err(JsonSchemaError {
        violations: vec![message.into()],
    })
}

fn assemble(mut nodes: Vec<CompiledNode>, root: usize) -> Value {
    let mut values = (0..nodes.len()).map(|_| None).collect::<Vec<_>>();
    for id in (0..nodes.len()).rev() {
        let node = std::mem::replace(
            &mut nodes[id],
            CompiledNode {
                annotations: Map::new(),
                kind: CompiledKind::Json,
            },
        );
        let value = assemble_node(node, &mut values);
        values[id] = Some(value);
    }
    values[root].take().expect("root assembled")
}

fn assemble_node(node: CompiledNode, values: &mut [Option<Value>]) -> Value {
    let mut object = node.annotations;
    match node.kind {
        CompiledKind::Json => {}
        CompiledKind::Scalar {
            schema_type,
            enumeration,
            constant,
        } => {
            object.insert("type".to_owned(), Value::String(schema_type));
            if let Some(enumeration) = enumeration {
                object.insert("enum".to_owned(), enumeration);
            }
            if let Some(constant) = constant {
                object.insert("const".to_owned(), constant);
            }
        }
        CompiledKind::Array { items } => {
            object.insert("type".to_owned(), Value::String("array".to_owned()));
            if let Some(items) = items {
                object.insert("items".to_owned(), take_value(values, items));
            }
        }
        CompiledKind::Object {
            properties,
            additional_properties,
        } => {
            object.insert("type".to_owned(), Value::String("object".to_owned()));
            object.insert(
                "additionalProperties".to_owned(),
                Value::Bool(additional_properties),
            );
            if let Some(properties) = properties {
                insert_properties(&mut object, values, properties);
            }
        }
        CompiledKind::OneOf { branches } => {
            object.insert(
                "oneOf".to_owned(),
                Value::Array(
                    branches
                        .into_iter()
                        .map(|id| take_value(values, id))
                        .collect(),
                ),
            );
        }
        CompiledKind::ParameterRoot { properties } => {
            object.insert("type".to_owned(), Value::String("object".to_owned()));
            insert_properties(&mut object, values, properties);
        }
    }
    Value::Object(object)
}

fn insert_properties(
    object: &mut Map<String, Value>,
    values: &mut [Option<Value>],
    properties: Vec<(String, usize, bool)>,
) {
    let mut compiled = Map::new();
    let mut required = Vec::new();
    for (key, id, is_required) in properties {
        compiled.insert(key.clone(), take_value(values, id));
        if is_required {
            required.push(Value::String(key));
        }
    }
    object.insert("properties".to_owned(), Value::Object(compiled));
    if !required.is_empty() {
        object.insert("required".to_owned(), Value::Array(required));
    }
}

fn take_value(values: &mut [Option<Value>], id: usize) -> Value {
    values[id].take().expect("child assembled before parent")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compiles_every_value_root_and_parameter_requiredness() {
        assert_eq!(
            value_schema_spec_to_json_schema(json!({
                "type": "array",
                "items": {"type": "json"}
            }))
            .expect("schema")
            .as_value(),
            &json!({"type": "array", "items": {}})
        );
        assert_eq!(
            parameter_schema_spec_to_json_schema(json!({
                "closed": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": true,
                    "properties": {"id": {"type": "integer", "required": true}}
                },
                "open": {"type": "object", "additionalProperties": true}
            }))
            .expect("schema")
            .as_value(),
            &json!({
                "type": "object",
                "properties": {
                    "closed": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"id": {"type": "integer"}},
                        "required": ["id"]
                    },
                    "open": {"type": "object", "additionalProperties": true}
                },
                "required": ["closed"]
            })
        );
    }

    #[test]
    fn rejects_forged_author_forms() {
        for invalid in [
            json!({"type": "object"}),
            json!({"oneOf": [{"type": "string"}]}),
            json!({"type": "number", "enum": ["1"]}),
            json!({"type": "string", "enum": ["a"], "const": "b"}),
            json!({"type": "integer", "const": 1.5}),
            json!({"type": "array", "items": {"type": "string", "required": true}}),
            json!({"type": "string", "extra": true}),
            json!({}),
            Value::Null,
        ] {
            assert!(value_schema_spec_to_json_schema(invalid).is_err());
        }
    }

    #[test]
    fn validates_arguments_at_the_implicit_root() {
        let violations = validate_args(
            json!({
                "path": {"type": "string", "required": true},
                "offset": {"type": "integer"}
            }),
            &json!({"offset": 1.5}),
        )
        .expect("valid author schema");
        assert_eq!(
            violations,
            [
                "missing required property \"path\"",
                "\"offset\" must be an integer"
            ]
        );
    }

    #[test]
    fn compiles_deep_unions_without_recursive_descent_or_drop() {
        let mut spec = json!({"type": "string"});
        for _ in 0..5_000 {
            let mut object = Map::new();
            object.insert(
                "oneOf".to_owned(),
                Value::Array(vec![spec, json!({"type": "null"})]),
            );
            spec = Value::Object(object);
        }
        let compiled = value_schema_spec_to_json_schema(spec).expect("deep schema");
        let mut cursor = compiled.as_value();
        let mut layers = 0;
        while let Some(branches) = cursor.get("oneOf").and_then(Value::as_array) {
            cursor = &branches[0];
            layers += 1;
        }
        assert_eq!(layers, 5_000);
    }
}
