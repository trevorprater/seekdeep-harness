//! Python Code Mode SDK projection from the enforced tool schema store.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

use crate::{ToolSdkSchema, check_supported_json_schema};

const TYPING_ORDER: [&str; 5] = ["Any", "Literal", "NotRequired", "Protocol", "TypedDict"];
const MAX_CLASS_NAME_BASE: usize = 120;
const MAX_LIST_NESTING: usize = 180;
const RESERVED: [&str; 36] = [
    "False",
    "None",
    "True",
    "and",
    "as",
    "assert",
    "async",
    "await",
    "break",
    "class",
    "continue",
    "def",
    "del",
    "elif",
    "else",
    "except",
    "finally",
    "for",
    "from",
    "global",
    "if",
    "import",
    "in",
    "is",
    "lambda",
    "nonlocal",
    "not",
    "or",
    "pass",
    "raise",
    "return",
    "try",
    "while",
    "with",
    "yield",
    "__debug__",
];

fn is_bare_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || unicode_ident::is_xid_start(first))
        && chars.all(unicode_ident::is_xid_continue)
        && name.nfkc().eq(name.chars())
}

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

fn pad(indent: usize) -> String {
    "    ".repeat(indent)
}

#[derive(Default)]
struct RenderState {
    classes: Vec<String>,
    used_class_names: HashSet<String>,
    next_class_counter: HashMap<String, usize>,
    typing: HashSet<&'static str>,
}

fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn describe(schema: &Map<String, Value>) -> Option<String> {
    let description = schema.get("description")?.as_str()?;
    let mut collapsed = String::new();
    let mut pending_space = false;
    for character in description.chars() {
        if is_js_whitespace(character) {
            pending_space = !collapsed.is_empty();
            continue;
        }
        if pending_space {
            collapsed.push(' ');
            pending_space = false;
        }
        let code = u32::from(character);
        if matches!(code, 0x00..=0x08 | 0x0e..=0x1f | 0x7f..=0x9f) {
            write!(collapsed, "\\x{code:02x}").expect("writing to a string cannot fail");
        } else {
            collapsed.push(character);
        }
    }
    (!collapsed.is_empty()).then_some(collapsed)
}

fn doc_lines(description: &str, indent: usize) -> Vec<String> {
    let schema = Map::from_iter([(
        "description".to_owned(),
        Value::String(description.to_owned()),
    )]);
    describe(&schema).map_or_else(Vec::new, |description| {
        let escaped = description.replace('\\', "\\\\").replace('"', "\\\"");
        vec![format!("{}\"\"\"{escaped}\"\"\"", pad(indent))]
    })
}

fn camel_case(raw: &str) -> String {
    let mut joined = String::new();
    let mut part = String::new();
    let flush = |part: &mut String, joined: &mut String| {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            joined.extend(first.to_uppercase());
            joined.extend(chars);
        }
        part.clear();
    };
    for character in raw.chars() {
        if character == '_' || !unicode_ident::is_xid_continue(character) {
            flush(&mut part, &mut joined);
        } else {
            part.push(character);
        }
    }
    flush(&mut part, &mut joined);
    let normalized = joined.nfkc().collect::<String>();
    let prefixed = if normalized
        .chars()
        .next()
        .is_some_and(unicode_ident::is_xid_start)
    {
        normalized
    } else {
        format!("Tool{normalized}")
    };
    prefixed.nfkc().collect()
}

fn cap_class_name_base(base: &str) -> String {
    let mut units = 0;
    let mut capped = String::new();
    for character in base.chars() {
        let width = character.len_utf16();
        if units + width > MAX_CLASS_NAME_BASE {
            break;
        }
        units += width;
        capped.push(character);
    }
    capped
}

fn allocate_class_name(base: &str, state: &mut RenderState) -> String {
    let capped = cap_class_name_base(base);
    let mut name = capped.clone();
    if state.used_class_names.contains(&name) {
        let counter = state.next_class_counter.entry(capped.clone()).or_insert(2);
        while state
            .used_class_names
            .contains(&format!("{capped}{counter}"))
        {
            *counter += 1;
        }
        name = format!("{capped}{counter}");
        *counter += 1;
    }
    state.used_class_names.insert(name.clone());
    name
}

fn child_class_name(base: &str, segment: &str) -> String {
    cap_class_name_base(&format!("{base}{segment}").nfkc().collect::<String>())
}

fn py_scalar(value: &Value) -> String {
    match value {
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(value) => serde_json::to_string(value).expect("string serialization"),
        Value::Number(number) => {
            let value = number.as_f64().unwrap_or_default();
            if value == 0.0 {
                return "0".to_owned();
            }
            if value.fract() == 0.0 && value.abs() > 9_007_199_254_740_991.0 {
                return format!("{value:.0}");
            }
            ryu_js::Buffer::new().format_finite(value).to_owned()
        }
        Value::Null | Value::Array(_) | Value::Object(_) => "None".to_owned(),
    }
}

fn constrained_scalar(
    node: &Map<String, Value>,
    broad: &'static str,
    state: &mut RenderState,
) -> String {
    if let Some(value) = node.get("const") {
        state.typing.insert("Literal");
        return format!("Literal[{}]", py_scalar(value));
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        state.typing.insert("Literal");
        return format!(
            "Literal[{}]",
            values.iter().map(py_scalar).collect::<Vec<_>>().join(", ")
        );
    }
    broad.to_owned()
}

enum TypeDocument {
    Text(String),
    Join(Vec<usize>),
    List(usize),
}

enum FrameKind<'a> {
    OneOf,
    Array,
    TypedDict {
        node: &'a Map<String, Value>,
        allocated: String,
        entries: Vec<(&'a str, &'a Value)>,
    },
}

struct RenderFrame<'a> {
    schema: &'a Value,
    class_name: String,
    list_depth: usize,
    phase_children: bool,
    kind: Option<FrameKind<'a>>,
    children: Vec<(&'a Value, String, usize)>,
    child_index: usize,
    child_documents: Vec<usize>,
}

impl<'a> RenderFrame<'a> {
    fn new(schema: &'a Value, class_name: String, list_depth: usize) -> Self {
        Self {
            schema,
            class_name,
            list_depth,
            phase_children: false,
            kind: None,
            children: Vec::new(),
            child_index: 0,
            child_documents: Vec::new(),
        }
    }
}

fn push_document(
    frames: &mut [RenderFrame<'_>],
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
    document: TypeDocument,
) {
    let index = documents.len();
    documents.push(document);
    if let Some(parent) = frames.last_mut() {
        parent.child_documents.push(index);
    } else {
        *root = Some(index);
    }
}

fn flatten_document(documents: &[TypeDocument], root: usize) -> String {
    enum Task<'a> {
        Document(usize),
        Text(&'a str),
    }
    let mut output = String::new();
    let mut tasks = vec![Task::Document(root)];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Text(text) => output.push_str(text),
            Task::Document(index) => match &documents[index] {
                TypeDocument::Text(text) => output.push_str(text),
                TypeDocument::List(child) => {
                    tasks.push(Task::Text("]"));
                    tasks.push(Task::Document(*child));
                    tasks.push(Task::Text("list["));
                }
                TypeDocument::Join(children) => {
                    for (position, child) in children.iter().enumerate().rev() {
                        tasks.push(Task::Document(*child));
                        if position > 0 {
                            tasks.push(Task::Text(" | "));
                        }
                    }
                }
            },
        }
    }
    output
}

fn render_type(schema: &Value, class_name: &str, state: &mut RenderState) -> String {
    if check_supported_json_schema(schema).is_err() {
        state.typing.insert("Any");
        return "Any".to_owned();
    }
    let root_class_name = class_name.to_owned();
    let mut frames = vec![RenderFrame::new(schema, class_name.to_owned(), 0)];
    let mut documents = Vec::new();
    let mut root = None;
    while !frames.is_empty() {
        let index = frames.len() - 1;
        if frames[index].phase_children {
            if frames[index].child_index < frames[index].children.len() {
                let child = frames[index].child_index;
                frames[index].child_index += 1;
                let (schema, name, depth) = frames[index].children[child].clone();
                frames.push(RenderFrame::new(schema, name, depth));
                continue;
            }
            finish_frame(&mut frames, &mut documents, &mut root, state);
            continue;
        }
        start_frame(
            &mut frames[index],
            &root_class_name,
            state,
            &mut documents,
            &mut root,
        );
    }
    root.map_or_else(
        || "Any".to_owned(),
        |root| flatten_document(&documents, root),
    )
}

fn finish_frame(
    frames: &mut Vec<RenderFrame<'_>>,
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
    state: &mut RenderState,
) {
    let frame = frames.pop().expect("render frame exists");
    let document = match frame.kind {
        Some(FrameKind::OneOf) => TypeDocument::Join(frame.child_documents),
        Some(FrameKind::Array) => TypeDocument::List(frame.child_documents[0]),
        Some(FrameKind::TypedDict {
            node,
            allocated,
            entries,
        }) => {
            let required = node
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>();
            let mut lines = vec![format!("class {allocated}(TypedDict):")];
            for (index, (field, schema)) in entries.iter().enumerate() {
                if let Some(description) = schema.as_object().and_then(describe) {
                    lines.push(format!("{}# {description}", pad(1)));
                }
                let field_type = flatten_document(documents, frame.child_documents[index]);
                if required.contains(field) {
                    lines.push(format!("{}{field}: {field_type}", pad(1)));
                } else {
                    state.typing.insert("NotRequired");
                    lines.push(format!("{}{field}: NotRequired[{field_type}]", pad(1)));
                }
            }
            if node.get("additionalProperties") != Some(&Value::Bool(false)) {
                lines.push(format!(
                    "{}# Additional keys beyond those declared are allowed.",
                    pad(1)
                ));
            }
            if lines.len() == 1 {
                lines.push(format!("{}pass", pad(1)));
            }
            state.classes.push(lines.join("\n"));
            TypeDocument::Text(allocated)
        }
        None => TypeDocument::Text("Any".to_owned()),
    };
    push_document(frames, documents, root, document);
}

fn start_frame(
    frame: &mut RenderFrame<'_>,
    root_class_name: &str,
    state: &mut RenderState,
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
) {
    frame.phase_children = true;
    let node = frame.schema.as_object().expect("validated schema node");
    if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
        frame.kind = Some(FrameKind::OneOf);
        frame.children = branches
            .iter()
            .enumerate()
            .map(|(index, branch)| {
                (
                    branch,
                    child_class_name(&frame.class_name, &(index + 1).to_string()),
                    frame.list_depth,
                )
            })
            .collect();
        return;
    }
    let Some(schema_type) = node.get("type").and_then(Value::as_str) else {
        state.typing.insert("Any");
        finish_immediate(frame, documents, root, "Any".to_owned());
        return;
    };
    match schema_type {
        "string" => finish_immediate(
            frame,
            documents,
            root,
            constrained_scalar(node, "str", state),
        ),
        "number" => finish_immediate(
            frame,
            documents,
            root,
            constrained_scalar(node, "float", state),
        ),
        "integer" => finish_immediate(
            frame,
            documents,
            root,
            constrained_scalar(node, "int", state),
        ),
        "boolean" => finish_immediate(
            frame,
            documents,
            root,
            constrained_scalar(node, "bool", state),
        ),
        "null" => finish_immediate(frame, documents, root, "None".to_owned()),
        "array" => start_array(frame, node, state, documents, root),
        "object" => start_object(frame, node, root_class_name, state, documents, root),
        _ => {
            state.typing.insert("Any");
            finish_immediate(frame, documents, root, "Any".to_owned());
        }
    }
}

fn finish_immediate(
    frame: &mut RenderFrame<'_>,
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
    text: String,
) {
    frame.kind = None;
    frame.children.clear();
    frame.child_documents.push(documents.len());
    documents.push(TypeDocument::Text(text));
    // A marker tells `finish_frame` to forward this already-built document.
    frame.kind = Some(FrameKind::OneOf);
    if root.is_none() {
        let _ = root;
    }
}

fn start_array<'a>(
    frame: &mut RenderFrame<'a>,
    node: &'a Map<String, Value>,
    state: &mut RenderState,
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
) {
    let Some(items) = node.get("items") else {
        state.typing.insert("Any");
        finish_immediate(frame, documents, root, "list[Any]".to_owned());
        return;
    };
    if frame.list_depth >= MAX_LIST_NESTING {
        state.typing.insert("Any");
        finish_immediate(frame, documents, root, "Any".to_owned());
        return;
    }
    frame.kind = Some(FrameKind::Array);
    frame
        .children
        .push((items, frame.class_name.clone(), frame.list_depth + 1));
}

fn start_object<'a>(
    frame: &mut RenderFrame<'a>,
    node: &'a Map<String, Value>,
    root_class_name: &str,
    state: &mut RenderState,
    documents: &mut Vec<TypeDocument>,
    root: &mut Option<usize>,
) {
    let entries = node
        .get("properties")
        .and_then(Value::as_object)
        .map_or_else(Vec::new, |properties| {
            properties
                .iter()
                .map(|(name, value)| (name.as_str(), value))
                .collect()
        });
    let legal = entries.iter().all(|(name, _)| {
        is_bare_identifier(name)
            && !is_reserved(name)
            && (!name.starts_with("__") || name.ends_with("__"))
    });
    if root_class_name.is_empty()
        || !legal
        || (entries.is_empty() && node.get("additionalProperties") != Some(&Value::Bool(false)))
    {
        state.typing.insert("Any");
        finish_immediate(frame, documents, root, "dict[str, Any]".to_owned());
        return;
    }
    let allocated = allocate_class_name(&frame.class_name, state);
    state.typing.insert("TypedDict");
    frame.children = entries
        .iter()
        .map(|(field, child)| (*child, child_class_name(&allocated, &camel_case(field)), 1))
        .collect();
    frame.kind = Some(FrameKind::TypedDict {
        node,
        allocated,
        entries,
    });
}

/// Maps one supported JSON Schema node to a context-free Python annotation.
#[must_use]
pub fn json_schema_to_py(schema: &Value) -> String {
    render_type(schema, "", &mut RenderState::default())
}

const SDK_INSTRUCTIONS: &str = r#"## Writing code for run_code

`run_code` takes two required arguments: `code` — the body of an async Python function (top-level `await` and `return` both work) — and `description`, a short summary of what the program does. At run time exactly two of the names declared below are bound: `tools` and `ToolCallError`. Everything else is a STATIC STUB describing argument and return types — in particular the `TypedDict` classes do NOT exist at run time, so build arguments as plain `dict`/`list` JSON values: `await tools.name({"field": 1})`, never `FooArgs(field=1)`, which raises `NameError`. Inside the program:

- Call tools as `await tools.name(args)` — subscript access for exotic, reserved, or underscore-leading names: `await tools["my-tool"](args)`. Every call resolves to the tool's typed canonical JSON value (each method's return type below). Tool arguments must be lossless JSON.
- A FAILED tool call raises `ToolCallError`, whose `toolName` identifies the failed tool and whose message is human-readable — wrap in `try/except` to handle and continue.
- Independent read-only calls MAY overlap under `asyncio.gather` (safe calls run concurrently; mutating calls run alone, in submission order). Sequence dependent work with `await`.
- Emit the run's answer with `print(...)` and/or a top-level `return <value>`; the returned value must be lossless JSON. ONLY what you print and the returned value come back — intermediate tool results never enter the conversation, so extract just what you need.

The available tools:"#;

/// Renders the deterministic Python `tools:sdk` prompt section.
#[must_use]
pub fn render_tools_sdk_py(schemas: &[ToolSdkSchema]) -> String {
    let mut sorted = schemas.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| js_string_cmp(&left.name, &right.name));
    let mut state = RenderState::default();
    state.typing.insert("Protocol");
    let mut members = Vec::new();
    let mut statements = 0;
    for schema in sorted {
        let args = render_type(
            &schema.parameters,
            &format!("{}Args", camel_case(&schema.name)),
            &mut state,
        );
        let output = render_type(
            &schema.output,
            &format!("{}Output", camel_case(&schema.name)),
            &mut state,
        );
        if is_bare_identifier(&schema.name)
            && !is_reserved(&schema.name)
            && !schema.name.starts_with('_')
        {
            let docs = doc_lines(&schema.description, 2);
            if docs.is_empty() {
                members.push(format!(
                    "{}async def {}(self, args: {args}) -> {output}: ...",
                    pad(1),
                    schema.name
                ));
            } else {
                members.push(format!(
                    "{}async def {}(self, args: {args}) -> {output}:",
                    pad(1),
                    schema.name
                ));
                members.extend(docs);
            }
            statements += 1;
        } else {
            let quoted = Value::String(schema.name.clone()).to_string();
            members.push(format!(
                "{}# tools[{quoted}](args: {args}) -> {output}",
                pad(1)
            ));
            let schema_map = Map::from_iter([(
                "description".to_owned(),
                Value::String(schema.description.clone()),
            )]);
            if let Some(description) = describe(&schema_map) {
                members.push(format!("{}#   {description}", pad(1)));
            }
        }
    }
    if statements == 0 {
        members.insert(0, format!("{}pass", pad(1)));
    }
    let imports = TYPING_ORDER
        .into_iter()
        .filter(|symbol| state.typing.contains(symbol))
        .collect::<Vec<_>>()
        .join(", ");
    let classes = if state.classes.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", state.classes.join("\n\n"))
    };
    format!(
        "{SDK_INSTRUCTIONS}\n\n```python\nfrom typing import {imports}\n\nclass ToolCallError(Exception):\n    toolName: str\n\n{classes}class Tools(Protocol):\n{}\n\ntools: Tools\n```",
        members.join("\n")
    )
}

fn js_string_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_schema_roots_and_constraints() {
        for (schema, expected) in [
            (json!({"type": "string"}), "str"),
            (json!({"type": "number"}), "float"),
            (json!({"type": "integer"}), "int"),
            (json!({"type": "boolean"}), "bool"),
            (json!({"type": "null"}), "None"),
            (
                json!({"type": "string", "enum": ["a", "b"]}),
                "Literal[\"a\", \"b\"]",
            ),
            (
                json!({"type": "array", "items": {"type": "number"}}),
                "list[float]",
            ),
            (json!({"type": "array"}), "list[Any]"),
            (json!({"type": "object"}), "dict[str, Any]"),
            (
                json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
                "str | None",
            ),
        ] {
            assert_eq!(json_schema_to_py(&schema), expected, "{schema}");
        }
    }

    #[test]
    fn renders_named_typed_dicts_methods_and_exotic_comments() {
        let schemas = vec![
            ToolSdkSchema {
                name: "search".to_owned(),
                description: "Search for text.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "What to search for."},
                        "limit": {"type": "number", "description": "Max results."},
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                }),
                output: json!({"type": "string"}),
            },
            ToolSdkSchema {
                name: "my-mcp.tool".to_owned(),
                description: "Exotic name.".to_owned(),
                parameters: json!({"type": "object"}),
                output: json!({"type": "string"}),
            },
        ];
        let text = render_tools_sdk_py(&schemas);
        assert!(text.contains("class SearchArgs(TypedDict):"));
        assert!(text.contains("    query: str"));
        assert!(text.contains("    limit: NotRequired[float]"));
        assert!(text.contains("async def search(self, args: SearchArgs) -> str:"));
        assert!(text.contains("# tools[\"my-mcp.tool\"](args: dict[str, Any]) -> str"));
        assert!(text.contains("from typing import Any, NotRequired, Protocol, TypedDict"));
    }

    #[test]
    fn renders_nested_objects_and_object_unions_dependency_first() {
        let text = render_tools_sdk_py(&[ToolSdkSchema {
            name: "workflow".to_owned(),
            description: "Run a workflow.".to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["meta"],
                "properties": {
                    "meta": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "name": {"type": "string"},
                            "phases": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {"title": {"type": "string"}},
                                    "required": ["title"]
                                }
                            }
                        },
                        "required": ["name"]
                    }
                }
            }),
            output: json!({
                "oneOf": [
                    {"type": "object", "additionalProperties": false, "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]},
                    {"type": "object", "additionalProperties": false, "properties": {"error": {"type": "string"}}, "required": ["error"]}
                ]
            }),
        }]);
        for expected in [
            "class WorkflowArgsMetaPhases(TypedDict):",
            "class WorkflowArgsMeta(TypedDict):",
            "class WorkflowArgs(TypedDict):",
            "class WorkflowOutput1(TypedDict):",
            "class WorkflowOutput2(TypedDict):",
            "phases: NotRequired[list[WorkflowArgsMetaPhases]]",
            "-> WorkflowOutput1 | WorkflowOutput2",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        assert!(
            text.find("class WorkflowArgsMetaPhases")
                < text.find("class WorkflowArgsMeta(TypedDict)")
        );
        assert!(text.find("class WorkflowArgs(TypedDict)") < text.find("class Tools(Protocol)"));
    }

    #[test]
    fn handles_unicode_normalization_reserved_names_and_class_collisions() {
        let object = |field: &str| {
            let properties = Map::from_iter([(field.to_owned(), json!({"type": "string"}))]);
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": properties,
                "required": [field]
            })
        };
        let text = render_tools_sdk_py(&[
            ToolSdkSchema {
                name: "路径".to_owned(),
                description: "Unicode tool.".to_owned(),
                parameters: object("路径"),
                output: json!({"type": "string"}),
            },
            ToolSdkSchema {
                name: "ﬁnd".to_owned(),
                description: "Normalizing tool.".to_owned(),
                parameters: object("q"),
                output: json!({"type": "string"}),
            },
            ToolSdkSchema {
                name: "class".to_owned(),
                description: "Reserved tool.".to_owned(),
                parameters: object("q"),
                output: json!({"type": "string"}),
            },
            ToolSdkSchema {
                name: "my-tool".to_owned(),
                description: "Dash.".to_owned(),
                parameters: object("x"),
                output: json!({"type": "string"}),
            },
            ToolSdkSchema {
                name: "my.tool".to_owned(),
                description: "Dot.".to_owned(),
                parameters: object("y"),
                output: json!({"type": "string"}),
            },
        ]);
        for expected in [
            "async def 路径(self, args: 路径Args) -> str:",
            "    路径: str",
            "# tools[\"ﬁnd\"](args: FIndArgs) -> str",
            "# tools[\"class\"](args: ClassArgs) -> str",
            "class MyToolArgs(TypedDict):",
            "class MyToolArgs2(TypedDict):",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }

        let normalized_field = render_tools_sdk_py(&[ToolSdkSchema {
            name: "ligature".to_owned(),
            description: String::new(),
            parameters: object("ﬁeld"),
            output: json!({"type": "string"}),
        }]);
        assert!(normalized_field.contains("args: dict[str, Any]"));
        assert!(!normalized_field.contains("ﬁeld:"));
    }

    #[test]
    fn keeps_render_stack_and_python_bracket_depth_bounded() {
        let mut schema = json!({"type": "string", "enum": ["end"]});
        for _ in 0..5_000 {
            schema = Value::Object(Map::from_iter([
                ("type".to_owned(), Value::String("array".to_owned())),
                ("items".to_owned(), schema),
            ]));
        }
        let rendered = json_schema_to_py(&schema);
        assert_eq!(rendered.matches("list[").count(), MAX_LIST_NESTING);
        assert!(rendered.ends_with(&"]".repeat(MAX_LIST_NESTING)));
        assert!(rendered.contains("Any"));
        std::mem::forget(schema);
    }

    #[test]
    fn escapes_docs_and_emits_exact_unsafe_integer_digits() {
        assert_eq!(
            json_schema_to_py(&json!({"type": "integer", "const": 1_152_921_504_606_846_976_u64})),
            "Literal[1152921504606846976]"
        );
        assert_eq!(
            json_schema_to_py(&json!({"type": "number", "const": 1e-7})),
            "Literal[1e-7]"
        );
        let text = render_tools_sdk_py(&[ToolSdkSchema {
            name: "weird".to_owned(),
            description: "quote \" slash \\ nul \0 end".to_owned(),
            parameters: json!({"type": "object"}),
            output: json!({"type": "string"}),
        }]);
        assert!(
            text.contains("\"\"\"quote \\\" slash \\\\ nul \\\\x00 end\"\"\""),
            "{text}"
        );
    }

    #[test]
    fn matches_unicode_identifier_and_case_mapping_edges() {
        let schema = |name: &str| ToolSdkSchema {
            name: name.to_owned(),
            description: format!("Tool {name}."),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"a‌b": {"type": "string"}}
            }),
            output: json!({"type": "string"}),
        };
        let text = render_tools_sdk_py(&[schema("ping"), schema("‌b"), schema("ƛ")]);
        for expected in [
            "    a‌b: NotRequired[str]",
            "# tools[\"‌b\"](args: Tool‌bArgs) -> str",
            "class Tool‌bArgs(TypedDict):",
            "async def ƛ(self, args: ꟜArgs) -> str:",
            "class ꟜArgs(TypedDict):",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
    }

    #[test]
    fn unsupported_schema_roots_degrade_to_any_and_literal_escaping_stays_parseable() {
        for schema in [
            Value::Null,
            json!(42),
            json!("string-schema"),
            json!({}),
            json!({"oneOf": 7}),
            json!({"$ref": "#/defs/x"}),
            json!({"type": "object", "properties": 7}),
            json!({"type": "string", "enum": [1, 2]}),
            json!({"type": "string", "enum": []}),
        ] {
            assert_eq!(json_schema_to_py(&schema), "Any", "{schema}");
        }
        for (schema, expected) in [
            (
                json!({"type": "string", "const": "a\0b"}),
                r#"Literal["a\u0000b"]"#,
            ),
            (
                json!({"type": "string", "const": "say \"hi\"\n"}),
                r#"Literal["say \"hi\"\n"]"#,
            ),
            (
                json!({"type": "string", "const": "ends\\"}),
                r#"Literal["ends\\"]"#,
            ),
            (json!({"type": "boolean", "const": true}), "Literal[True]"),
            (
                json!({"type": "boolean", "enum": [false]}),
                "Literal[False]",
            ),
            (
                json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
                "str | None",
            ),
        ] {
            assert_eq!(json_schema_to_py(&schema), expected, "{schema}");
        }
    }

    #[test]
    fn open_closed_and_python_unsafe_object_fields_match_the_source_fallbacks() {
        let schema = |name: &str, parameters: Value| ToolSdkSchema {
            name: name.to_owned(),
            description: String::new(),
            parameters,
            output: json!({"type": "string"}),
        };
        let text = render_tools_sdk_py(&[
            schema(
                "openness",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "open": {"type": "object", "additionalProperties": true, "properties": {"x": {"type": "string"}}, "required": ["x"]},
                        "closedEmpty": {"type": "object", "additionalProperties": false, "properties": {}},
                    },
                    "required": ["open", "closedEmpty"],
                }),
            ),
            schema(
                "mangled",
                json!({"type": "object", "additionalProperties": false, "properties": {"__token": {"type": "string"}}, "required": ["__token"]}),
            ),
            schema(
                "debugger",
                json!({"type": "object", "additionalProperties": false, "properties": {"__debug__": {"type": "string"}}, "required": ["__debug__"]}),
            ),
            schema(
                "soft",
                json!({"type": "object", "additionalProperties": false, "properties": {"match": {"type": "string"}, "case": {"type": "string"}, "type": {"type": "string"}, "_": {"type": "string"}}}),
            ),
        ]);
        for expected in [
            "class OpennessArgsOpen(TypedDict):",
            "    # Additional keys beyond those declared are allowed.",
            "class OpennessArgsClosedEmpty(TypedDict):\n    pass",
            "async def mangled(self, args: dict[str, Any]) -> str: ...",
            "async def debugger(self, args: dict[str, Any]) -> str: ...",
            "    match: NotRequired[str]",
            "    case: NotRequired[str]",
            "    type: NotRequired[str]",
            "    _: NotRequired[str]",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        assert!(!text.contains("__token:"));
        assert!(!text.contains("__debug__:"));
    }

    #[test]
    fn tool_members_keep_utf16_order_and_route_reserved_exotic_and_underscore_names() {
        let schema = |name: &str| ToolSdkSchema {
            name: name.to_owned(),
            description: String::new(),
            parameters: json!({"type": "object"}),
            output: json!({"type": "string"}),
        };
        let text = render_tools_sdk_py(&[
            schema("z"),
            schema("a-tool"),
            schema("class"),
            schema("_foo"),
            schema("__meta__"),
            schema("\u{e000}"),
            schema("\u{10000}"),
        ]);
        for expected in [
            "# tools[\"a-tool\"]",
            "# tools[\"class\"]",
            "# tools[\"_foo\"]",
            "# tools[\"__meta__\"]",
            "async def z",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        assert!(text.find("# tools[\"a-tool\"]") < text.find("async def z"));
        assert!(text.find("# tools[\"𐀀\"]") < text.find("# tools[\"\u{e000}\"]"));
        assert!(!text.contains("async def _foo"));
    }

    #[test]
    fn empty_sdk_and_description_projection_are_exact_and_deterministic() {
        let empty = render_tools_sdk_py(&[]);
        assert!(empty.contains("from typing import Protocol"));
        assert!(empty.contains("class Tools(Protocol):\n    pass"));

        let described = || ToolSdkSchema {
            name: "weird".to_owned(),
            description: "tab\t newline\n quote \" slash \\ bell\u{7}".to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"field": {"type": "string", "description": "tab\t newline\n quote \" slash \\ bell\u{7}"}},
                "required": ["field"],
            }),
            output: json!({"type": "string"}),
        };
        let rendered = render_tools_sdk_py(&[described()]);
        assert!(rendered.contains("# tab newline quote \" slash \\ bell\\x07"));
        assert!(rendered.contains(r#""""tab newline quote \" slash \\ bell\\x07""""#));
        assert_eq!(rendered, render_tools_sdk_py(&[described()]));
    }
}
