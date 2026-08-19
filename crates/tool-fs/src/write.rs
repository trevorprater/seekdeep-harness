//! Model-facing full-file write: validation, output formatting, and registration.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventReply};
use seekdeep_fs::{FS, FsObservation, FsWriteIntent, FsWriteOperation};
use seekdeep_llm::ContentBlock;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, DiffCallView, DiffResultView, FileDiff, FileLocation,
    TOOLS, ToolCallView, ToolResult, ToolResultView, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::diff::{compute_hunk_diffs, diffs_from_meta};
use crate::error::remediate_fs_error;
use crate::read_target::emit_fs_observed;
use crate::sandbox::{FsEscalationArgs, FsSandboxController};
use crate::session_cwd::session_resolve_options;

/// Canonical successful write outcome matching the tool's output schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOutcome {
    /// Resolved model-facing path.
    pub path: String,
    /// Whether the write created or replaced.
    pub operation: FsWriteOperation,
    /// Content before the write, or null when unavailable.
    pub before: Option<String>,
    /// Content after the write.
    pub after: String,
}

/// Registers the `write` tool and its system-prompt guidance.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures.
#[allow(clippy::too_many_lines)]
pub fn apply_write_tool(ctx: &Context, sandbox: &Arc<FsSandboxController>) -> anyhow::Result<()> {
    let prompt = ctx
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires systemPrompt"))?;
    prompt.section(
        ctx,
        PromptSection::new(
            "tool:write",
            101.0,
            PromptText::Static(
                "Use the write tool to create files or completely replace file contents. Existing files are overwritten, so read an existing file first (the default fs-observation-policy requires it) and prefer edit for targeted changes."
                    .to_owned(),
            ),
        ),
    )?;

    let tools = ctx
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires tools"))?;

    let mut parameters = json!({
        "file_path": {"type": "string", "required": true, "description": "Path to write, resolved by the filesystem backend."},
        "content": {"type": "string", "required": true, "description": "Full UTF-8 text content to write."},
    });
    if !sandbox.escalation_modes.is_empty() {
        let fields = sandbox.schema_fields();
        parameters["sandbox_permissions"] = fields.sandbox_permissions;
        parameters["justification"] = fields.justification;
    }

    let execute_ctx = ctx.clone();
    let execute_sandbox = Arc::clone(sandbox);
    let output = DefineToolOutput::new(
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "required": true},
                "operation": {"type": "string", "required": true, "enum": ["create", "update"]},
                "before": {"required": true, "oneOf": [{"type": "string"}, {"type": "null"}]},
                "after": {"type": "string", "required": true},
            },
        }),
        Arc::new(|_args: &WriteArgsRaw, value: &WriteOutcome| {
            Ok(vec![ContentBlock::Text {
                text: format_write_output(&value.path, value.operation),
            }])
        }),
    )
    .presentation_meta(Arc::new(|args: &WriteArgsRaw, value: &WriteOutcome| {
        let diffs = match &value.before {
            None => Vec::new(),
            Some(before) => compute_hunk_diffs(&args.file_path, before, &value.after),
        };
        Ok(json!({"diffs": diffs}))
    }));

    let definition = define_tool(
        DefineToolOptions::new(
            "write",
            "Create or fully replace a UTF-8 text file.",
            parameters,
            output,
            Arc::new(move |args: WriteArgsRaw, execution| {
                let ctx = execute_ctx.clone();
                let sandbox = Arc::clone(&execute_sandbox);
                Box::pin(async move {
                    let input = parse_write_args(&args)?;
                    let escalation = FsEscalationArgs {
                        sandbox_permissions: args.sandbox_permissions.clone(),
                        justification: args.justification.clone(),
                    };
                    let policy = sandbox
                        .resolve_policy(
                            "write",
                            &escalation,
                            execution.agent.as_ref(),
                            &execution.call_id,
                            &execution.signal(),
                        )
                        .await?;
                    let filesystem = ctx
                        .get(FS)
                        .ok_or_else(|| anyhow::anyhow!("tool-fs requires fs"))?
                        .filesystem();
                    let workspace_root = policy
                        .as_ref()
                        .map(|policy| policy.workspace_root.to_string_lossy().into_owned());
                    let options = session_resolve_options(
                        &execution,
                        &input.file_path,
                        workspace_root.as_deref(),
                    );
                    let target = filesystem
                        .resolve(
                            &input.file_path,
                            options.cwd.as_deref(),
                            Some(&options.signal),
                        )
                        .await?;
                    let reply = ctx
                        .events()
                        .waterfall(
                            &ctx,
                            "fs/write-intent",
                            &EventArgs::from_values(vec![
                                Arc::new(target.clone()),
                                Arc::new(execution.execution().clone()),
                            ]),
                            || Box::pin(async move { Ok(EventReply::Undefined) }),
                        )
                        .await?;
                    let intent = reply
                        .downcast::<FsWriteIntent>()
                        .map(|intent| (*intent).clone());
                    let outcome = match filesystem
                        .write_text(
                            &target,
                            &input.content,
                            intent.as_ref(),
                            Some(&execution.signal()),
                            policy.as_ref(),
                        )
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            return Err(remediate_fs_error(
                                sandbox.map_error(error, policy.as_ref()),
                            ));
                        }
                    };
                    emit_fs_observed(
                        &ctx,
                        &target,
                        FsObservation::Present {
                            version: outcome.version,
                        },
                        &execution,
                    )?;
                    Ok(WriteOutcome {
                        path: target.display_path,
                        operation: outcome.operation,
                        before: outcome.before,
                        after: outcome.after,
                    })
                })
            }),
        )
        .present_call(Arc::new(|args: &WriteArgsRaw| {
            Some(ToolCallView::Diff(DiffCallView {
                title: format!("Write {}", args.file_path),
                diffs: vec![FileDiff {
                    path: args.file_path.clone(),
                    old_text: None,
                    new_text: args.content.clone(),
                }],
                locations: Some(vec![FileLocation {
                    path: args.file_path.clone(),
                    line: None,
                }]),
            }))
        }))
        .present_result(Arc::new(|args: &WriteArgsRaw, result: &ToolResult| {
            if result.is_error {
                return None;
            }
            let diffs = result
                .meta
                .as_ref()
                .and_then(diffs_from_meta)
                .unwrap_or_else(|| {
                    vec![FileDiff {
                        path: args.file_path.clone(),
                        old_text: None,
                        new_text: args.content.clone(),
                    }]
                });
            Some(ToolResultView::Diff(DiffResultView {
                title: Some(format!("Write {}", args.file_path)),
                diffs,
            }))
        })),
    )?;
    tools.register(ctx, definition)?;
    Ok(())
}

/// Raw schema-validated write arguments, including the escalation fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteArgsRaw {
    /// Path to write.
    pub file_path: String,
    /// Full UTF-8 text content to write.
    pub content: String,
    /// The wider sandbox mode this write needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_permissions: Option<String>,
    /// One-sentence justification for the wider access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// Validated write arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteInput {
    /// Path to write.
    pub file_path: String,
    /// Full UTF-8 text content to write.
    pub content: String,
}

/// Validates the constraints the schema cannot express: only a non-blank path.
///
/// # Errors
///
/// Returns a blank-path failure.
pub fn parse_write_args(args: &WriteArgsRaw) -> anyhow::Result<WriteInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    Ok(WriteInput {
        file_path: args.file_path.clone(),
        content: args.content.clone(),
    })
}

/// Formats a write outcome as one model-facing confirmation envelope.
#[must_use]
pub fn format_write_output(display_path: &str, operation: FsWriteOperation) -> String {
    let verb = match operation {
        FsWriteOperation::Create => "Created",
        FsWriteOperation::Update => "Updated",
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(file_path: &str, content: &str) -> WriteArgsRaw {
        WriteArgsRaw {
            file_path: file_path.to_owned(),
            content: content.to_owned(),
            sandbox_permissions: None,
            justification: None,
        }
    }

    #[test]
    fn parse_rejects_blank_path_and_preserves_content() {
        assert!(parse_write_args(&raw("  ", "x")).is_err());
        let input = parse_write_args(&raw("a.txt", "hello")).expect("valid");
        assert_eq!(input.file_path, "a.txt");
        assert_eq!(input.content, "hello");
    }

    #[test]
    fn format_write_output_selects_verb() {
        let created = format_write_output("/a", FsWriteOperation::Create);
        assert!(created.contains("<path>/a</path>"));
        assert!(created.contains("Created file"));
        let updated = format_write_output("/a", FsWriteOperation::Update);
        assert!(updated.contains("Updated file"));
    }
}
