//! Deterministic MCP discovery, generation swap, execution, and rendering.

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_llm::ContentBlock;
use seekdeep_tools::{
    JsonSchemaNode, ToolDefinition, ToolOutputDefinition, assert_supported_json_schema,
};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::protocol::{McpClient, McpTool};

const MAX_PUBLIC_NAME_LENGTH: usize = 64;
const HASH_LENGTH: usize = 12;

/// Whether a registration conflict is contained or rejects synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationFailure {
    /// Log, roll back, and return an empty generation.
    Contain,
    /// Log, roll back, and propagate the conflict.
    Throw,
}

/// Resolved discovery and execution options.
#[derive(Clone, Debug)]
pub struct ToolBridgeOptions {
    /// Registration conflict behavior.
    pub registration_failure: RegistrationFailure,
    /// Stable server namespace.
    pub server_name: String,
    /// Per-call timeout in milliseconds.
    pub tool_call_timeout_ms: f64,
}

/// Current tool-generation effects keyed by public name.
pub type ToolDisposers = BTreeMap<String, EffectHandle>;

/// Derives the exact model-facing tool name from one stable MCP identity.
#[must_use]
pub fn public_tool_name(server_name: &str, raw_name: &str) -> String {
    let joined = format!("mcp__{server_name}__{raw_name}");
    let units = joined.encode_utf16().collect::<Vec<_>>();
    let mut changed = false;
    let mut normalized = String::with_capacity(units.len());
    for unit in units {
        if let Ok(byte) = u8::try_from(unit)
            && (byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            normalized.push(char::from(byte));
            continue;
        }
        changed = true;
        normalized.push('_');
    }
    if !changed && normalized.len() <= MAX_PUBLIC_NAME_LENGTH {
        return normalized;
    }
    let identity = format!("{server_name}\0{raw_name}");
    let digest = Sha256::digest(identity.as_bytes());
    let hash = digest
        .iter()
        .flat_map(|byte| format!("{byte:02x}").chars().collect::<Vec<_>>())
        .take(HASH_LENGTH)
        .collect::<String>();
    let prefix = MAX_PUBLIC_NAME_LENGTH - HASH_LENGTH - 1;
    format!("{}_{}", &normalized[..normalized.len().min(prefix)], hash)
}

/// Fetches and atomically replaces one server's complete tool generation.
///
/// Fetch failures preserve `previous`. Registration conflicts dispose the old
/// generation, roll back every partial new registration, and return according
/// to [`RegistrationFailure`].
///
/// # Errors
///
/// Returns protocol, invalid-list, schema, strict-registration, or disposal failures.
pub async fn sync_tools(
    client: Arc<dyn McpClient>,
    context: &Context,
    options: &ToolBridgeOptions,
    current: &mut ToolDisposers,
) -> anyhow::Result<()> {
    let mut definitions = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = client.list_tools(cursor.as_deref()).await?;
        for tool in page.tools {
            let public_name = public_tool_name(&options.server_name, &tool.name);
            anyhow::ensure!(
                !definitions.contains_key(&public_name),
                "mcp-client({}): server listed tool {:?} more than once — invalid tool list",
                options.server_name,
                tool.name
            );
            definitions.insert(
                public_name.clone(),
                create_definition(Arc::clone(&client), public_name, tool, options)?,
            );
        }
        cursor = page.next_cursor.filter(|value| !value.is_empty());
        if cursor.is_none() {
            break;
        }
    }
    anyhow::ensure!(
        !client.closed_signal().is_aborted(),
        "MCP generation closed during tool discovery"
    );

    dispose_generation(std::mem::take(current)).await?;
    let mut next = BTreeMap::new();
    for (public_name, definition) in definitions {
        match context
            .get(seekdeep_tools::TOOLS)
            .ok_or_else(|| anyhow::anyhow!("mcp-client requires tools"))?
            .register(context, definition)
        {
            Ok(effect) => {
                next.insert(public_name, effect);
            }
            Err(error) => {
                let rollback = dispose_generation(next).await;
                context
                    .logger(Some("mcp-client"))
                    .error([Value::String(format!(
                        "mcp-client({}): tool registration failed, no tools registered: {error}",
                        options.server_name
                    ))]);
                if let Err(rollback) = rollback {
                    return Err(anyhow::anyhow!(
                        "{error:#}: partial MCP generation rollback failed: {rollback:#}"
                    ));
                }
                return match options.registration_failure {
                    RegistrationFailure::Contain => Ok(()),
                    RegistrationFailure::Throw => Err(error),
                };
            }
        }
    }
    *current = next;
    Ok(())
}

/// Disposes every registration in one generation and aggregates failures.
///
/// # Errors
///
/// Returns all effect-disposal diagnostics after every entry settles.
pub async fn dispose_generation(generation: ToolDisposers) -> anyhow::Result<()> {
    let failures = futures::future::join_all(
        generation
            .into_values()
            .map(|effect| async move { effect.dispose().await }),
    )
    .await
    .into_iter()
    .filter_map(Result::err)
    .map(|error| format!("{error:#}"))
    .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("; "))
    }
}

fn create_definition(
    client: Arc<dyn McpClient>,
    public_name: String,
    tool: McpTool,
    options: &ToolBridgeOptions,
) -> anyhow::Result<ToolDefinition> {
    let raw_name = tool.name;
    let parameters =
        tool.input_schema.as_object().cloned().ok_or_else(|| {
            anyhow::anyhow!("MCP tool {raw_name:?} inputSchema must be an object")
        })?;
    let structured_schema = tool
        .output_schema
        .and_then(|schema| assert_supported_json_schema(schema).ok());
    let output = create_output(&raw_name, structured_schema.as_ref())?;
    let task_required = tool
        .execution
        .and_then(|execution| execution.task_support)
        .as_deref()
        == Some("required");
    let execute_name = raw_name.clone();
    let execute = Arc::new(
        move |arguments: Value, run: seekdeep_tools::ToolRunContext| {
            let client = Arc::clone(&client);
            let raw_name = execute_name.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    !task_required,
                    "Tool {raw_name:?} requires task-based execution, which this bridge does not support"
                );
                let arguments = arguments.as_object().cloned().unwrap_or_default();
                let result = client.call_tool(&raw_name, arguments, run.signal()).await?;
                normalize_call_result(&result, &raw_name)
            }) as seekdeep_tools::ToolExecuteFuture
        },
    );
    Ok(ToolDefinition::new(
        public_name,
        tool.description.unwrap_or_default(),
        parameters,
        output,
        execute,
    )
    .timeout_ms(options.tool_call_timeout_ms))
}

fn create_output(
    raw_name: &str,
    structured_schema: Option<&JsonSchemaNode>,
) -> anyhow::Result<ToolOutputDefinition> {
    let structured = structured_schema
        .as_ref()
        .map_or_else(|| json!({}), |schema| schema.as_value().clone());
    let required = if structured_schema.is_some() {
        json!(["content", "structuredContent"])
    } else {
        json!(["content"])
    };
    let schema = assert_supported_json_schema(json!({
        "type":"object",
        "properties":{
            "content":{"type":"array","items":{}},
            "structuredContent":structured
        },
        "required":required,
        "additionalProperties":false
    }))?;
    let name = raw_name.to_owned();
    Ok(ToolOutputDefinition::new(
        Arc::new(schema),
        Arc::new(move |_, value| {
            let content = value
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(vec![ContentBlock::Text {
                text: extract_text(&content, &name),
            }])
        }),
    ))
}

fn normalize_call_result(result: &Value, raw_name: &str) -> anyhow::Result<Value> {
    let object = result
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("MCP tools/call result must be an object"))?;
    let is_error = object
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(content) = object.get("content").and_then(Value::as_array) {
        let text = extract_text(content, raw_name);
        anyhow::ensure!(!is_error, text);
        let mut output = Map::from_iter([("content".to_owned(), Value::Array(content.clone()))]);
        if let Some(structured) = object.get("structuredContent") {
            output.insert("structuredContent".to_owned(), structured.clone());
        }
        return Ok(Value::Object(output));
    }
    let text = object.get("toolResult").map_or_else(
        || "(no output)".to_owned(),
        |value| serde_json::to_string(value).unwrap_or_else(|_| "(no output)".to_owned()),
    );
    anyhow::ensure!(!is_error, text.clone());
    let mut output = Map::from_iter([("content".to_owned(), json!([{"type":"text","text":text}]))]);
    if let Some(structured) = object.get("structuredContent") {
        output.insert("structuredContent".to_owned(), structured.clone());
    }
    Ok(Value::Object(output))
}

/// Projects complete MCP blocks into the existing Native text boundary.
#[must_use]
pub fn extract_text(content: &[Value], tool_name: &str) -> String {
    let mut parts = Vec::new();
    for value in content {
        let Some(block) = value.as_object() else {
            parts.push("[unsupported content type: unknown]".to_owned());
            continue;
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("image") => parts.push(format!(
                "[image: {}, content discarded]",
                block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )),
            Some("audio") => parts.push(format!(
                "[audio: {}, content discarded]",
                block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )),
            Some("resource" | "resource_link") => {
                parts.push("[resource: content discarded]".to_owned());
            }
            Some(other) => parts.push(format!("[unsupported content type: {other}]")),
            None => parts.push("[unsupported content type: undefined]".to_owned()),
        }
    }
    if parts.is_empty() {
        format!("({tool_name} returned no text content)")
    } else {
        parts.join("\n")
    }
}
