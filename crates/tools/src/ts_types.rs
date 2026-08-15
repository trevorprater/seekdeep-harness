//! TypeScript Code Mode SDK projection from the enforced tool schema store.

use serde_json::Value;

use crate::json_schema::check_supported_json_schema;

/// Model-facing input schema plus the canonical output schema.
#[derive(Debug, PartialEq)]
pub struct ToolSdkSchema {
    /// Registered tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// Object-rooted argument schema.
    pub parameters: Value,
    /// Canonical successful value schema.
    pub output: Value,
}

#[derive(Debug)]
struct TypeDocument {
    parts: Vec<DocumentPart>,
    contains_union_or_intersection: bool,
}

#[derive(Debug)]
enum DocumentPart {
    Text(String),
    Document(usize),
}

impl TypeDocument {
    fn text(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            contains_union_or_intersection: value.contains('|') || value.contains('&'),
            parts: vec![DocumentPart::Text(value)],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    OneOf,
    Array,
    Object,
}

struct RenderFrame<'a> {
    node: &'a Value,
    indent: usize,
    phase_children: bool,
    kind: Option<FrameKind>,
    children: Vec<(&'a Value, usize)>,
    child_index: usize,
    child_documents: Vec<usize>,
    entries: Vec<(&'a str, &'a Value)>,
}

impl<'a> RenderFrame<'a> {
    fn new(node: &'a Value, indent: usize) -> Self {
        Self {
            node,
            indent,
            phase_children: false,
            kind: None,
            children: Vec::new(),
            child_index: 0,
            child_documents: Vec::new(),
            entries: Vec::new(),
        }
    }
}

/// Maps one enforced JSON Schema node to a TypeScript type literal.
///
/// Unsupported input degrades to `unknown` and never escapes an error.
#[must_use]
pub fn json_schema_to_ts(schema: &Value, indent: usize) -> String {
    if check_supported_json_schema(schema).is_err() {
        return "unknown".to_owned();
    }
    let (documents, root) = render_supported_schema(schema, indent);
    flatten_document(&documents, root)
}

fn render_supported_schema(schema: &Value, indent: usize) -> (Vec<TypeDocument>, usize) {
    let mut frames = vec![RenderFrame::new(schema, indent)];
    let mut documents = Vec::new();
    let mut root = None;
    while !frames.is_empty() {
        let index = frames.len() - 1;
        if frames[index].phase_children {
            if frames[index].child_index < frames[index].children.len() {
                let child_index = frames[index].child_index;
                frames[index].child_index += 1;
                let (node, indent) = frames[index].children[child_index];
                frames.push(RenderFrame::new(node, indent));
                continue;
            }
            let frame = frames.pop().expect("render frame exists");
            let document = finish_frame(&frame, &documents);
            finish_document(&mut frames, &mut documents, &mut root, document);
            continue;
        }
        let object = frames[index].node.as_object().expect("asserted schema");
        if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
            let frame = &mut frames[index];
            frame.kind = Some(FrameKind::OneOf);
            frame.children = one_of.iter().map(|child| (child, frame.indent)).collect();
            frame.phase_children = true;
            continue;
        }
        let Some(schema_type) = object.get("type").and_then(Value::as_str) else {
            let frame = frames.pop().expect("render frame exists");
            finish_document(
                &mut frames,
                &mut documents,
                &mut root,
                TypeDocument::text("JsonValue"),
            );
            drop(frame);
            continue;
        };
        match schema_type {
            "string" | "number" | "integer" | "boolean" | "null" => {
                let document = TypeDocument::text(render_constrained_scalar(object, schema_type));
                frames.pop();
                finish_document(&mut frames, &mut documents, &mut root, document);
            }
            "array" => {
                if let Some(items) = object.get("items") {
                    let frame = &mut frames[index];
                    frame.kind = Some(FrameKind::Array);
                    frame.children.push((items, frame.indent));
                    frame.phase_children = true;
                } else {
                    frames.pop();
                    finish_document(
                        &mut frames,
                        &mut documents,
                        &mut root,
                        TypeDocument::text("JsonValue[]"),
                    );
                }
            }
            "object" => {
                let entries = object
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| {
                        properties
                            .iter()
                            .map(|(name, child)| (name.as_str(), child))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if entries.is_empty() {
                    let open = object.get("additionalProperties") != Some(&Value::Bool(false));
                    frames.pop();
                    finish_document(
                        &mut frames,
                        &mut documents,
                        &mut root,
                        TypeDocument::text(if open {
                            "Record<string, JsonValue>"
                        } else {
                            "Record<string, never>"
                        }),
                    );
                } else {
                    let frame = &mut frames[index];
                    frame.kind = Some(FrameKind::Object);
                    frame.children = entries
                        .iter()
                        .map(|(_, child)| (*child, frame.indent + 1))
                        .collect();
                    frame.entries = entries;
                    frame.phase_children = true;
                }
            }
            _ => unreachable!("schema type was asserted"),
        }
    }
    (documents, root.expect("root document"))
}

fn finish_document(
    frames: &mut [RenderFrame<'_>],
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
    document: TypeDocument,
) {
    let id = documents.len();
    documents.push(document);
    if let Some(parent) = frames.last_mut() {
        parent.child_documents.push(id);
    } else {
        *root = Some(id);
    }
}

fn finish_frame(frame: &RenderFrame<'_>, documents: &[TypeDocument]) -> TypeDocument {
    match frame.kind.expect("child frame kind") {
        FrameKind::OneOf => {
            let mut parts = Vec::new();
            for (index, child) in frame.child_documents.iter().enumerate() {
                if index > 0 {
                    parts.push(DocumentPart::Text(" | ".to_owned()));
                }
                parts.push(DocumentPart::Document(*child));
            }
            document(parts, documents)
        }
        FrameKind::Array => {
            let child = frame.child_documents[0];
            let parts = if documents[child].contains_union_or_intersection {
                vec![
                    DocumentPart::Text("(".to_owned()),
                    DocumentPart::Document(child),
                    DocumentPart::Text(")[]".to_owned()),
                ]
            } else {
                vec![
                    DocumentPart::Document(child),
                    DocumentPart::Text("[]".to_owned()),
                ]
            };
            document(parts, documents)
        }
        FrameKind::Object => finish_object(frame, documents),
    }
}

fn finish_object(frame: &RenderFrame<'_>, documents: &[TypeDocument]) -> TypeDocument {
    let object = frame.node.as_object().expect("asserted object schema");
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut parts = vec![DocumentPart::Text("{".to_owned())];
    for (index, (name, property)) in frame.entries.iter().enumerate() {
        if let Some(description) = property
            .as_object()
            .and_then(|property| property.get("description"))
            .and_then(Value::as_str)
        {
            for line in doc_lines(description, frame.indent + 1) {
                parts.push(DocumentPart::Text(format!("\n{line}")));
            }
        }
        parts.push(DocumentPart::Text(format!(
            "\n{}{}{}: ",
            pad(frame.indent + 1),
            render_key(name),
            if required.contains(name) { "" } else { "?" }
        )));
        parts.push(DocumentPart::Document(frame.child_documents[index]));
        parts.push(DocumentPart::Text(";".to_owned()));
    }
    parts.push(DocumentPart::Text(format!("\n{}}}", pad(frame.indent))));
    let mut declared = document(parts, documents);
    if object.get("additionalProperties") != Some(&Value::Bool(false)) {
        // The caller cannot append the intermediate into its immutable slice,
        // so retain it inline as a nested sequence by flattening this bounded object document.
        let rendered = flatten_single(&declared, documents);
        declared = TypeDocument {
            parts: vec![DocumentPart::Text(format!(
                "{rendered} & Record<string, JsonValue>"
            ))],
            contains_union_or_intersection: true,
        };
    }
    declared
}

fn document(parts: Vec<DocumentPart>, documents: &[TypeDocument]) -> TypeDocument {
    let contains = parts.iter().any(|part| match part {
        DocumentPart::Text(text) => text.contains('|') || text.contains('&'),
        DocumentPart::Document(id) => documents[*id].contains_union_or_intersection,
    });
    TypeDocument {
        parts,
        contains_union_or_intersection: contains,
    }
}

fn flatten_single(document: &TypeDocument, documents: &[TypeDocument]) -> String {
    let mut chunks = Vec::new();
    let mut tasks = document.parts.iter().rev().collect::<Vec<_>>();
    while let Some(part) = tasks.pop() {
        match part {
            DocumentPart::Text(text) => chunks.push(text.as_str()),
            DocumentPart::Document(id) => {
                tasks.extend(documents[*id].parts.iter().rev());
            }
        }
    }
    chunks.concat()
}

fn flatten_document(documents: &[TypeDocument], root: usize) -> String {
    flatten_single(&documents[root], documents)
}

fn render_constrained_scalar(object: &serde_json::Map<String, Value>, schema_type: &str) -> String {
    if let Some(constant) = object.get("const") {
        return serde_json::to_string(constant).expect("scalar serializes");
    }
    if let Some(enumeration) = object.get("enum").and_then(Value::as_array) {
        return enumeration
            .iter()
            .map(|value| serde_json::to_string(value).expect("scalar serializes"))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if schema_type == "integer" {
        "number".to_owned()
    } else {
        schema_type.to_owned()
    }
}

fn render_key(name: &str) -> String {
    let mut characters = name.chars();
    let first = characters.next();
    let valid = first
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | '$'))
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'));
    if valid {
        name.to_owned()
    } else {
        serde_json::to_string(name).expect("string serializes")
    }
}

fn pad(indent: usize) -> String {
    "  ".repeat(indent)
}

fn doc_lines(description: &str, indent: usize) -> Vec<String> {
    if description.is_empty() {
        return Vec::new();
    }
    let collapsed = description.split_whitespace().collect::<Vec<_>>().join(" ");
    vec![format!(
        "{}/** {} */",
        pad(indent),
        collapsed.replace("*/", "*\\/")
    )]
}

const SDK_INSTRUCTIONS: &str = r#"## Writing code for run_code

`run_code` takes two required arguments: `code` — the body of an async TypeScript function (erasable syntax only — no `enum` or namespaces; type annotations are advisory, the code runs type-stripped) — and `description`, a short summary of what the program does. Inside the program:

- Call tools as `await tools.name(args)` — quoted access for exotic names: `tools["my-tool"](args)`. Every call resolves to the tool's typed canonical JSON value. Tool arguments must be lossless JSON.
- A FAILED tool call rejects with `ToolCallError`, whose `toolName` identifies the failed tool and whose `message` is human-readable — `try/catch` it to handle and continue.
- Independent read-only calls MAY overlap under `Promise.all` (safe calls run concurrently; mutating calls run alone, in submission order). Sequence dependent work with `await`.
- Emit results with `return` and/or `console.log(...)`. ONLY what you print or return comes back to you — intermediate tool results never enter the conversation, so extract just what you need.

The available tools:"#;

/// Renders the deterministic TypeScript `tools:sdk` model prompt section.
#[must_use]
pub fn render_tools_sdk(schemas: &[ToolSdkSchema]) -> String {
    let mut sorted = schemas.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let mut args_members = Vec::new();
    let mut output_members = Vec::new();
    for schema in sorted {
        args_members.extend(doc_lines(&schema.description, 1));
        args_members.push(format!(
            "{}{}: {};",
            pad(1),
            render_key(&schema.name),
            json_schema_to_ts(&schema.parameters, 1)
        ));
        output_members.push(format!(
            "{}{}: {};",
            pad(1),
            render_key(&schema.name),
            json_schema_to_ts(&schema.output, 1)
        ));
    }
    let args_map = interface("ToolArgsMap", &args_members);
    let output_map = interface("ToolOutputMap", &output_members);
    let declaration = format!(
        "{args_map}\n\n{output_map}\n\ntype ToolName = keyof ToolOutputMap\n\ndeclare class ToolCallError extends Error {{\n  readonly name: \"ToolCallError\";\n  readonly toolName: ToolName;\n}}\n\ndeclare const tools: {{\n  [K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]>;\n}}"
    );
    let json_value = "type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }";
    format!("{SDK_INSTRUCTIONS}\n\n```ts\n{json_value}\n\n{declaration}\n```")
}

fn interface(name: &str, members: &[String]) -> String {
    if members.is_empty() {
        format!("interface {name} {{}}")
    } else {
        format!("interface {name} {{\n{}\n}}", members.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_every_schema_construct() {
        for (schema, expected) in [
            (json!({"type": "string"}), "string"),
            (json!({"type": "integer"}), "number"),
            (json!({"type": "null"}), "null"),
            (
                json!({"type": "string", "enum": ["a", "b"]}),
                "\"a\" | \"b\"",
            ),
            (
                json!({"type": "array", "items": {"type": "number"}}),
                "number[]",
            ),
            (json!({"type": "object"}), "Record<string, JsonValue>"),
            (
                json!({"type": "object", "additionalProperties": false}),
                "Record<string, never>",
            ),
            (json!({}), "JsonValue"),
        ] {
            assert_eq!(json_schema_to_ts(&schema, 0), expected);
        }
        assert_eq!(
            json_schema_to_ts(
                &json!({"type": "array", "items": {"type": "string", "enum": ["x", "y"]}}),
                0
            ),
            "(\"x\" | \"y\")[]"
        );
    }

    #[test]
    fn renders_nested_objects_docs_and_exotic_names() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute file path"},
                "my-key": {"type": "boolean"}
            },
            "required": ["path"]
        });
        assert_eq!(
            json_schema_to_ts(&schema, 0),
            "{\n  /** Absolute file path */\n  path: string;\n  \"my-key\"?: boolean;\n} & Record<string, JsonValue>"
        );
    }

    #[test]
    fn degrades_invalid_input_and_is_stack_safe() {
        assert_eq!(json_schema_to_ts(&json!({"oneOf": []}), 0), "unknown");
        let mut schema = json!({"type": "string"});
        for _ in 0..5_000 {
            let mut object = serde_json::Map::new();
            object.insert(
                "oneOf".to_owned(),
                Value::Array(vec![schema, json!({"type": "null"})]),
            );
            schema = Value::Object(object);
        }
        let rendered = json_schema_to_ts(&schema, 0);
        assert!(rendered.starts_with("string | null"));
        assert_eq!(rendered.len(), "string".len() + 5_000 * " | null".len());
        std::mem::forget(schema);
    }

    #[test]
    fn renders_deterministic_sdk() {
        let schemas = [
            ToolSdkSchema {
                name: "my-mcp.tool".to_owned(),
                description: "Exotic name.".to_owned(),
                parameters: json!({"type": "object", "properties": {}}),
                output: json!({"type": "array", "items": {"type": "string"}}),
            },
            ToolSdkSchema {
                name: "bash".to_owned(),
                description: "Run a shell command.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }),
                output: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"exitCode": {"type": "integer"}},
                    "required": ["exitCode"]
                }),
            },
        ];
        let text = render_tools_sdk(&schemas);
        assert!(text.find("bash:").expect("bash") < text.find("\"my-mcp.tool\":").expect("mcp"));
        assert!(text.contains("two required arguments"));
        assert!(text.contains("readonly toolName: ToolName;"));
    }
}
