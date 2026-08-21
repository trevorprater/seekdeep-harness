//! Model-facing language-server navigation tool.

pub mod render;

pub use render::{
    DEFAULT_MAX_LOCATIONS, DEFAULT_MAX_RESULT_CHARS, LSP_OPERATIONS, LspToolArgs, LspToolInput,
    format_hover, format_locations, parse_lsp_args, present_lsp_call, render_uri,
};

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_llm::ContentBlock;
use seekdeep_lsp::{LSP, LspError, LspQueryRequest, LspQueryResult};
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{DefineToolOptions, DefineToolOutput, TOOLS, define_tool};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "tool-lsp";
/// Runtime services required by this tool plugin.
pub const INJECT: &[&str] = &["tools", "lsp", "systemPrompt"];
/// Default cooperative tool-call timeout in milliseconds.
pub const DEFAULT_LSP_TOOL_TIMEOUT_MS: f64 = 60_000.0;
const DEFAULT_MAX_LOCATIONS_NUMBER: f64 = 100.0;
const DEFAULT_MAX_RESULT_CHARS_NUMBER: f64 = 16_000.0;
/// Stable prompt guidance positioning LSP as a precision aid.
pub const LSP_PROMPT_TEXT: &str = "Use search/read for ordinary navigation. Use lsp when textual matches are ambiguous or before a change requires precise definitions, implementations, or references. Positions are one-based line and character (UTF-16) at the cursor; an off-symbol position may return no results. findReferences always includes the declaration.";
/// Stable code when a tool call has no live session workspace.
pub const LSP_WORKSPACE_REQUIRED: &str = "LSP_WORKSPACE_REQUIRED";

/// Result caps and timeout before defaults are resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Largest rendered-location count.
    pub max_locations: Option<f64>,
    /// Largest complete rendered result in UTF-16 characters.
    pub max_result_chars: Option<f64>,
    /// Cooperative tool-call timeout in milliseconds.
    pub timeout_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedConfig {
    max_locations: usize,
    max_result_chars: usize,
    timeout_ms: f64,
}

/// Source-compatible plugin configuration schema.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "maxLocations",
            Schema::number().with_default(DEFAULT_MAX_LOCATIONS_NUMBER),
        ),
        (
            "maxResultChars",
            Schema::number().with_default(DEFAULT_MAX_RESULT_CHARS_NUMBER),
        ),
        (
            "timeoutMs",
            Schema::number()
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_LSP_TOOL_TIMEOUT_MS),
        ),
    ])
}

/// Loader-facing namespace-style Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        let config: Config = serde_json::from_value(value.clone())?;
        let _ = resolve_config(&config)?;
        Ok(value.clone())
    })
}

/// Registers the prompt section and typed `lsp` tool.
///
/// # Errors
///
/// Returns invalid configuration, missing services, schema, duplicate
/// registration, prompt, or inactive-owner failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let resolved = resolve_config(config)?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-lsp requires tools"))?;
    let lsp = context
        .get(LSP)
        .ok_or_else(|| anyhow::anyhow!("tool-lsp requires lsp"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-lsp requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new("tool:lsp", 112.0, LSP_PROMPT_TEXT),
    )?;

    let output = DefineToolOutput::new(
        output_schema(),
        Arc::new(move |_args: &LspToolArgs, result: &LspQueryResult| {
            let text = match result {
                LspQueryResult::Locations {
                    locations,
                    resolved_workspace_uri,
                } => format_locations(
                    locations,
                    resolved_workspace_uri,
                    resolved.max_locations,
                    resolved.max_result_chars,
                ),
                LspQueryResult::Hover { hover } => {
                    format_hover(hover.as_ref(), resolved.max_result_chars)
                }
            };
            Ok(vec![ContentBlock::Text { text }])
        }),
    );
    let execute_lsp = lsp;
    let execute = Arc::new(
        move |args: LspToolArgs, execution: seekdeep_tools::ToolRunContext| {
            let lsp = execute_lsp.clone();
            Box::pin(async move {
                let input = parse_lsp_args(&args)?;
                let workspace_root = execution.session_cwd().ok_or_else(|| {
                    LspError::new(
                        "the lsp tool requires a session workspace cwd",
                        LSP_WORKSPACE_REQUIRED,
                    )
                })?;
                lsp.query(
                    LspQueryRequest {
                        operation: input.operation,
                        file_path: input.file_path,
                        position: input.position,
                        workspace_root: workspace_root.to_owned(),
                    },
                    Some(execution.signal()),
                )
                .await
            }) as seekdeep_tools::DefineToolFuture<LspQueryResult>
        },
    );
    let definition = define_tool(
        DefineToolOptions::new(
            "lsp",
            "Query a language server for precise code navigation. operation is one of goToDefinition, findReferences, goToImplementation, hover. line and character are one-based UTF-16 cursor coordinates. findReferences includes the declaration.",
            parameter_schema(),
            output,
            execute,
        )
        .timeout_ms(resolved.timeout_ms)
        .present_call(Arc::new(|args| Some(present_lsp_call(args)))),
    )?;
    tools.register(context, definition)?;
    Ok(())
}

fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let max_locations = config.max_locations.unwrap_or(DEFAULT_MAX_LOCATIONS_NUMBER);
    let max_result_chars = config
        .max_result_chars
        .unwrap_or(DEFAULT_MAX_RESULT_CHARS_NUMBER);
    let timeout_ms = config.timeout_ms.unwrap_or(DEFAULT_LSP_TOOL_TIMEOUT_MS);
    assert_positive_integer("maxLocations", max_locations)?;
    assert_positive_integer("maxResultChars", max_result_chars)?;
    anyhow::ensure!(
        timeout_ms.is_finite()
            && timeout_ms.fract() == 0.0
            && (1.0..=MAX_TIMER_DELAY_MS).contains(&timeout_ms),
        "tool-lsp: timeoutMs must be a positive integer no greater than {MAX_TIMER_DELAY_MS}"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(ResolvedConfig {
        max_locations: max_locations as usize,
        max_result_chars: max_result_chars as usize,
        timeout_ms,
    })
}

fn assert_positive_integer(name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && value >= 1.0,
        "tool-lsp: {name} must be a positive integer"
    );
    Ok(())
}

fn parameter_schema() -> Value {
    json!({
        "operation": {
            "type": "string",
            "required": true,
            "enum": LSP_OPERATIONS,
            "description": "goToDefinition, findReferences, goToImplementation, or hover."
        },
        "file_path": {
            "type": "string",
            "required": true,
            "description": "The source file to query, relative to the workspace or absolute."
        },
        "line": {
            "type": "number",
            "required": true,
            "description": "One-based line of the cursor."
        },
        "character": {
            "type": "number",
            "required": true,
            "description": "One-based UTF-16 column of the cursor."
        }
    })
}

fn output_schema() -> Value {
    let position = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "line": {"type": "integer", "required": true},
            "character": {"type": "integer", "required": true}
        }
    });
    let range = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "start": merge_required(position.clone()),
            "end": merge_required(position)
        }
    });
    json!({"oneOf": [
        {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "kind": {"type": "string", "required": true, "const": "locations"},
                "locations": {
                    "type": "array",
                    "required": true,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "uri": {"type": "string", "required": true},
                            "range": merge_required(range.clone())
                        }
                    }
                },
                "resolvedWorkspaceUri": {"type": "string", "required": true}
            }
        },
        {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "kind": {"type": "string", "required": true, "const": "hover"},
                "hover": {
                    "required": true,
                    "oneOf": [
                        {"type": "null"},
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "contents": {"type": "string", "required": true},
                                "range": range
                            }
                        }
                    ]
                }
            }
        }
    ]})
}

fn merge_required(mut value: Value) -> Value {
    value
        .as_object_mut()
        .expect("schema fragment is object")
        .insert("required".to_owned(), Value::Bool(true));
    value
}
