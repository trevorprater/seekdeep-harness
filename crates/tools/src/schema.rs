//! Author-facing value-schema DSL and its enforced JSON Schema projection.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use seekdeep_llm::ContentBlock;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::json_schema::{
    JsonSchemaError, JsonSchemaNode, UNSUPPORTED_SCHEMA, assert_supported_json_schema,
    validate_json_schema_value_at,
};
use crate::runtime::ToolContentFinalizer;
use crate::{
    ToolCallView, ToolDefinition, ToolOutputDefinition, ToolResult, ToolResultView, ToolRunContext,
};

const ANNOTATION_KEYS: [&str; 4] = ["description", "title", "default", "examples"];

/// Stable code carried by argument-schema failures.
pub const INVALID_ARGS: &str = "INVALID_ARGS";

/// Invalid model-generated arguments for a typed tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolArgsError {
    /// Stable machine-readable failure code.
    pub code: &'static str,
    /// Individual violations in schema-walk order.
    pub violations: Vec<String>,
}

impl ToolArgsError {
    /// Creates a structured invalid-arguments failure.
    #[must_use]
    pub fn new(violations: Vec<String>) -> Self {
        Self {
            code: INVALID_ARGS,
            violations,
        }
    }
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

/// Boxed future returned by one typed tool body.
pub type DefineToolFuture<O> = Pin<Box<dyn Future<Output = anyhow::Result<O>> + Send + 'static>>;
/// Typed accepted-call body.
pub type DefineToolExecute<A, O> =
    Arc<dyn Fn(A, ToolRunContext) -> DefineToolFuture<O> + Send + Sync + 'static>;
/// Typed successful-value renderer.
pub type DefineToolRender<A, O> =
    Arc<dyn Fn(&A, &O) -> anyhow::Result<Vec<ContentBlock>> + Send + Sync + 'static>;
/// Typed replayable presentation-metadata projector.
pub type DefineToolPresentationMeta<A, O> =
    Arc<dyn Fn(&A, &O) -> anyhow::Result<Value> + Send + Sync + 'static>;
/// Typed fail-closed overlap classifier.
pub type DefineToolConcurrencyClassifier<A> = Arc<dyn Fn(&A) -> bool + Send + Sync + 'static>;
/// Typed pending-call presenter.
pub type DefineToolCallPresenter<A> =
    Arc<dyn Fn(&A) -> Option<ToolCallView> + Send + Sync + 'static>;
/// Typed completed-call presenter.
pub type DefineToolResultPresenter<A> =
    Arc<dyn Fn(&A, &ToolResult) -> Option<ToolResultView> + Send + Sync + 'static>;

/// Typed canonical output declaration for [`define_tool`].
pub struct DefineToolOutput<A, O> {
    /// Author-facing output schema.
    pub schema: Value,
    /// Pure Native/model renderer.
    pub render: DefineToolRender<A, O>,
    /// Optional replayable presentation metadata.
    pub presentation_meta: Option<DefineToolPresentationMeta<A, O>>,
}

impl<A, O> DefineToolOutput<A, O> {
    /// Builds the mandatory output declaration.
    #[must_use]
    pub fn new(schema: Value, render: DefineToolRender<A, O>) -> Self {
        Self {
            schema,
            render,
            presentation_meta: None,
        }
    }

    /// Adds presentation metadata.
    #[must_use]
    pub fn presentation_meta(mut self, projector: DefineToolPresentationMeta<A, O>) -> Self {
        self.presentation_meta = Some(projector);
        self
    }
}

/// Rust-native typed counterpart of the source `DefineToolOptions` object.
pub struct DefineToolOptions<A, O> {
    /// Unique tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// Author-facing implicit parameter property map.
    pub parameters: Value,
    /// Typed canonical output declaration.
    pub output: DefineToolOutput<A, O>,
    /// Typed accepted-call body.
    pub execute: DefineToolExecute<A, O>,
    /// Optional last-mile content finalizer.
    pub finalize_content: Option<ToolContentFinalizer>,
    /// Optional positive cooperative timeout.
    pub timeout_ms: Option<f64>,
    /// Optional typed overlap classifier.
    pub is_concurrency_safe: Option<DefineToolConcurrencyClassifier<A>>,
    /// Optional typed pending-call presenter.
    pub present_call: Option<DefineToolCallPresenter<A>>,
    /// Optional typed completed-call presenter.
    pub present_result: Option<DefineToolResultPresenter<A>>,
}

impl<A, O> DefineToolOptions<A, O> {
    /// Builds the mandatory definition fields.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        output: DefineToolOutput<A, O>,
        execute: DefineToolExecute<A, O>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            output,
            execute,
            finalize_content: None,
            timeout_ms: None,
            is_concurrency_safe: None,
            present_call: None,
            present_result: None,
        }
    }

    /// Adds a final content transform.
    #[must_use]
    pub fn finalize_content(mut self, finalizer: ToolContentFinalizer) -> Self {
        self.finalize_content = Some(finalizer);
        self
    }

    /// Declares a cooperative timeout.
    #[must_use]
    pub fn timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Adds a typed fail-closed overlap classifier.
    #[must_use]
    pub fn concurrency_safe(mut self, classifier: DefineToolConcurrencyClassifier<A>) -> Self {
        self.is_concurrency_safe = Some(classifier);
        self
    }

    /// Adds a typed replay-safe pending-call presenter.
    #[must_use]
    pub fn present_call(mut self, presenter: DefineToolCallPresenter<A>) -> Self {
        self.present_call = Some(presenter);
        self
    }

    /// Adds a typed replay-safe completed-call presenter.
    #[must_use]
    pub fn present_result(mut self, presenter: DefineToolResultPresenter<A>) -> Self {
        self.present_result = Some(presenter);
        self
    }
}

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

/// Defines one typed first-party tool with hard execution validation and soft
/// replay-presentation validation.
///
/// The generic Rust argument/output types are the compile-time counterpart of
/// the source DSL's `InferArgs`/`InferValue`: callers choose serde types whose
/// shape matches the declared schemas, and every runtime value still crosses
/// the same schema boundary before a typed callback runs.
///
/// # Errors
///
/// Returns schema-compilation errors or an invalid timeout declaration.
pub fn define_tool<A, O>(options: DefineToolOptions<A, O>) -> anyhow::Result<ToolDefinition>
where
    A: DeserializeOwned + Send + Sync + 'static,
    O: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let DefineToolOptions {
        name,
        description,
        parameters,
        output,
        execute,
        finalize_content,
        timeout_ms,
        is_concurrency_safe,
        present_call,
        present_result,
    } = options;
    if let Some(timeout_ms) = timeout_ms {
        anyhow::ensure!(
            timeout_ms.is_finite() && timeout_ms > 0.0,
            "defineTool({name}): timeoutMs must be a positive finite number"
        );
    }

    let parameters = Arc::new(parameter_schema_spec_to_json_schema(parameters)?);
    let output_schema = Arc::new(value_schema_spec_to_json_schema(output.schema)?);

    let render = output.render;
    let render_projection = Arc::new(move |arguments: &Value, value: &Value| {
        let arguments = decode_typed::<A>(arguments, "arguments for output.render")?;
        let value = decode_typed::<O>(value, "value for output.render")?;
        render(&arguments, &value)
    });
    let mut output_definition = ToolOutputDefinition::new(output_schema, render_projection);
    if let Some(projector) = output.presentation_meta {
        output_definition = output_definition.presentation_meta(Arc::new(
            move |arguments: &Value, value: &Value| {
                let arguments =
                    decode_typed::<A>(arguments, "arguments for output.presentationMeta")?;
                let value = decode_typed::<O>(value, "value for output.presentationMeta")?;
                projector(&arguments, &value)
            },
        ));
    }

    let execute_schema = parameters.clone();
    let body = Arc::new(move |arguments: Value, execution: ToolRunContext| {
        let violations = validate_json_schema_value_at(&execute_schema, &arguments, "");
        if !violations.is_empty() {
            return Box::pin(async move { Err(anyhow::Error::new(ToolArgsError::new(violations))) })
                as crate::runtime::ToolExecuteFuture;
        }
        let parsed = decode_typed::<A>(&arguments, "arguments for execute");
        let execute = execute.clone();
        Box::pin(async move {
            let value = execute(parsed?, execution).await?;
            serde_json::to_value(value)
                .map_err(|error| anyhow::anyhow!("tool output is not lossless JSON: {error}"))
        }) as crate::runtime::ToolExecuteFuture
    });

    let Value::Object(parameter_map) = parameters.as_value().clone() else {
        unreachable!("parameter compiler always returns an object root")
    };
    let mut definition =
        ToolDefinition::new(name, description, parameter_map, output_definition, body);
    definition.finalize_content = finalize_content;
    definition.timeout_ms = timeout_ms;
    if let Some(classifier) = is_concurrency_safe {
        let schema = parameters.clone();
        definition.is_concurrency_safe = Some(Arc::new(move |arguments| {
            if !validate_json_schema_value_at(&schema, arguments, "").is_empty() {
                return false;
            }
            decode_typed::<A>(arguments, "arguments for isConcurrencySafe")
                .is_ok_and(|arguments| classifier(&arguments))
        }));
    }
    if let Some(presenter) = present_call {
        let schema = parameters.clone();
        definition.present_call = Some(Arc::new(move |arguments| {
            if !validate_json_schema_value_at(&schema, arguments, "").is_empty() {
                return None;
            }
            let arguments = decode_typed::<A>(arguments, "arguments for presentCall").ok()?;
            presenter(&arguments)
        }));
    }
    if let Some(presenter) = present_result {
        let schema = parameters;
        definition.present_result = Some(Arc::new(move |arguments, result| {
            if !validate_json_schema_value_at(&schema, arguments, "").is_empty() {
                return None;
            }
            let arguments = decode_typed::<A>(arguments, "arguments for presentResult").ok()?;
            presenter(&arguments, result)
        }));
    }
    Ok(definition)
}

fn decode_typed<T: DeserializeOwned>(value: &Value, boundary: &str) -> anyhow::Result<T> {
    serde_json::from_value(value.clone())
        .map_err(|error| anyhow::anyhow!("{boundary} does not match its Rust type: {error}"))
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
        code: UNSUPPORTED_SCHEMA,
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
    use seekdeep_llm::{AbortSignal, CallId};
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::{
        GenericCallView, GenericResultView, ToolCallKind, ToolExecutionInput, ToolRuntime,
        ToolRuntimeConfig,
    };

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TypedEchoArgs {
        text: String,
        uppercase: Option<bool>,
    }

    #[derive(Debug, Deserialize)]
    struct RequiredOptionalArgs {
        path: String,
        offset: Option<i64>,
        data: Option<Value>,
    }

    fn text_output<A>() -> DefineToolOutput<A, String> {
        DefineToolOutput::new(
            json!({"type": "string"}),
            Arc::new(|_, value| {
                Ok(vec![ContentBlock::Text {
                    text: value.clone(),
                }])
            }),
        )
    }

    #[test]
    fn compiles_every_value_root_and_author_only_json_node() {
        for (spec, expected) in [
            (
                json!({"type": "string", "enum": ["a", "b"], "const": "a"}),
                json!({"type": "string", "enum": ["a", "b"], "const": "a"}),
            ),
            (json!({"type": "number"}), json!({"type": "number"})),
            (json!({"type": "integer"}), json!({"type": "integer"})),
            (json!({"type": "boolean"}), json!({"type": "boolean"})),
            (json!({"type": "null"}), json!({"type": "null"})),
            (
                json!({"type": "array", "items": {"type": "json"}}),
                json!({"type": "array", "items": {}}),
            ),
            (
                json!({"type": "object", "additionalProperties": false, "properties": {}}),
                json!({"type": "object", "additionalProperties": false, "properties": {}}),
            ),
            (
                json!({
                    "type": "json",
                    "description": "anything",
                    "title": "Any JSON",
                    "default": null,
                    "examples": [{"nested": true}],
                }),
                json!({
                    "description": "anything",
                    "title": "Any JSON",
                    "default": null,
                    "examples": [{"nested": true}],
                }),
            ),
            (
                json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
                json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
            ),
        ] {
            assert_eq!(
                value_schema_spec_to_json_schema(spec)
                    .expect("compile")
                    .as_value(),
                &expected
            );
        }
    }

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
    fn preserves_property_literally_named_proto_as_schema_data() {
        let schema = parameter_schema_spec_to_json_schema(json!({
            "__proto__": {"type": "string", "required": true},
        }))
        .expect("schema");
        assert_eq!(
            schema.as_value(),
            &json!({
                "type": "object",
                "properties": {"__proto__": {"type": "string"}},
                "required": ["__proto__"],
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
            json!({"type": "array", "items": 42}),
            json!({"type": "array", "items": {"type": "string", "required": true}}),
            json!({"type": "string", "extra": true}),
            json!({"type": "string", "oneOf": [{"type": "string"}, {"type": "null"}]}),
            json!({"oneOf": "not-an-array"}),
            json!({"type": "string", "enum": "a"}),
            json!({}),
            Value::Null,
        ] {
            assert!(value_schema_spec_to_json_schema(invalid).is_err());
        }
        for invalid in [
            Value::Null,
            json!({"bad": 42}),
            json!({"value": {"type": "string", "required": false}}),
        ] {
            assert!(parameter_schema_spec_to_json_schema(invalid).is_err());
        }
        // Symbol/non-enumerable keys, decorated arrays, sparse arrays, and
        // cyclic author graphs cannot inhabit owned `Value`.
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

    #[test]
    fn rust_author_schema_values_are_acyclic_by_construction() {
        let reused = json!({"type": "string"});
        let schema = value_schema_spec_to_json_schema(json!({
            "oneOf": [reused.clone(), reused],
        }))
        .expect("value reuse is acyclic and valid");
        assert_eq!(schema.as_value()["oneOf"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn typed_builder_infers_native_argument_and_output_types() {
        let definition = define_tool(DefineToolOptions::new(
            "typed-echo",
            "A typed echo tool",
            json!({
                "text": {"type": "string", "required": true},
                "uppercase": {"type": "boolean"},
            }),
            text_output::<TypedEchoArgs>(),
            Arc::new(|args: TypedEchoArgs, _| {
                Box::pin(async move {
                    Ok(if args.uppercase.unwrap_or(false) {
                        args.text.to_uppercase()
                    } else {
                        args.text
                    })
                })
            }),
        ))
        .expect("definition");
        assert_eq!(definition.name, "typed-echo");
        assert_eq!(
            definition.parameters,
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "uppercase": {"type": "boolean"},
                },
                "required": ["text"],
            })
            .as_object()
            .expect("object")
            .clone()
        );
    }

    #[test]
    fn typed_builder_preserves_required_and_optional_native_fields() {
        let definition = define_tool(DefineToolOptions::new(
            "typed-fields",
            "",
            json!({
                "path": {"type": "string", "required": true},
                "offset": {"type": "integer"},
                "data": {"type": "json"},
            }),
            text_output::<RequiredOptionalArgs>(),
            Arc::new(|args: RequiredOptionalArgs, _| {
                Box::pin(async move {
                    let _typed: (&str, Option<i64>, Option<&Value>) =
                        (&args.path, args.offset, args.data.as_ref());
                    Ok(args.path)
                })
            }),
        ))
        .expect("definition");
        assert_eq!(definition.parameters["required"], json!(["path"]));
    }

    #[tokio::test]
    async fn define_tool_executes_typed_values_and_soft_validates_presenters() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let options = DefineToolOptions::new(
            "typed-echo",
            "A typed echo tool",
            json!({
                "text": {"type": "string", "required": true},
                "uppercase": {"type": "boolean"},
            }),
            text_output::<TypedEchoArgs>(),
            Arc::new(|args: TypedEchoArgs, _| {
                Box::pin(async move {
                    Ok(if args.uppercase.unwrap_or(false) {
                        args.text.to_uppercase()
                    } else {
                        args.text
                    })
                })
            }),
        )
        .present_call(Arc::new(|args| {
            Some(ToolCallView::Generic(GenericCallView {
                title: format!("Open {}", args.text),
                kind: Some(ToolCallKind::Read),
                raw_input: Some(json!(args.text)),
                content: None,
                locations: None,
            }))
        }))
        .present_result(Arc::new(|args, result| {
            Some(ToolResultView::Generic(GenericResultView {
                title: Some(format!("Opened {}", args.text)),
                content: Some(result.content.clone()),
            }))
        }));
        let definition = define_tool(options).expect("definition");
        assert!(definition.present_call.as_ref().expect("present call")(&json!({})).is_none());
        assert!(
            definition.present_result.as_ref().expect("present result")(
                &json!({"wrong": 1}),
                &ToolResult {
                    content: Vec::new(),
                    is_error: false,
                    meta: None,
                }
            )
            .is_none()
        );
        runtime
            .register(&context, definition)
            .expect("register definition");
        let result = runtime
            .execute(ToolExecutionInput::new(
                CallId::new("c1"),
                "typed-echo",
                json!({"text": "hello", "uppercase": true}),
                AbortSignal::default(),
            ))
            .await;
        assert_eq!(result.value(), Some(&json!("HELLO")));
        assert_eq!(
            result.content(),
            [ContentBlock::Text {
                text: "HELLO".to_owned(),
            }]
        );

        let invalid = runtime
            .execute(ToolExecutionInput::new(
                CallId::new("c2"),
                "typed-echo",
                json!({}),
                AbortSignal::default(),
            ))
            .await;
        assert_eq!(
            invalid.error().map(|error| error.message.as_str()),
            Some("invalid arguments: missing required property \"text\"")
        );
        assert_eq!(
            invalid.error().and_then(|error| error.info.as_ref()),
            Some(&crate::ToolErrorInfo {
                name: "ToolArgsError".to_owned(),
                code: INVALID_ARGS.to_owned(),
            })
        );
    }

    #[test]
    fn define_tool_rejects_nonpositive_and_nonfinite_timeouts() {
        for timeout in [0.0, -5.0, f64::INFINITY, f64::NAN] {
            let error = define_tool(
                DefineToolOptions::new(
                    "x",
                    "d",
                    json!({}),
                    text_output::<Value>(),
                    Arc::new(|_: Value, _| Box::pin(async { Ok("ok".to_owned()) })),
                )
                .timeout_ms(timeout),
            )
            .expect_err("timeout must fail");
            assert!(
                error
                    .to_string()
                    .contains("timeoutMs must be a positive finite number")
            );
        }
    }

    #[test]
    fn tool_args_error_carries_stable_code_violations_and_message() {
        let error = ToolArgsError::new(vec![
            "missing required property \"a\"".to_owned(),
            "\"b\" must be a number".to_owned(),
        ]);
        assert_eq!(error.code, INVALID_ARGS);
        assert_eq!(
            error.violations.as_slice(),
            ["missing required property \"a\"", "\"b\" must be a number",]
        );
        assert_eq!(
            error.to_string(),
            "invalid arguments: missing required property \"a\"; \"b\" must be a number"
        );
    }
}
