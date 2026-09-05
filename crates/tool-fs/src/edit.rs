//! Model-facing literal edit: validation, output formatting, and registration.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventReply};
use seekdeep_fs::{FS, FsEditRequest, FsObservation, FsVersion};
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

/// Canonical successful edit outcome matching the tool's output schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    /// Resolved model-facing path.
    pub path: String,
    /// Content before the edit.
    pub before: String,
    /// Content after the edit.
    pub after: String,
}

/// Registers the `edit` tool and its system-prompt guidance.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures.
#[allow(clippy::too_many_lines)]
pub fn apply_edit_tool(ctx: &Context, sandbox: &Arc<FsSandboxController>) -> anyhow::Result<()> {
    let prompt = ctx
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires systemPrompt"))?;
    prompt.section(
        ctx,
        PromptSection::new(
            "tool:edit",
            102.0,
            PromptText::Static(
                "Use the edit tool for targeted changes to existing UTF-8 text files. It replaces literal old_string with new_string; by default old_string must appear exactly once. If old_string appears multiple times, provide a more specific old_string or set replace_all to true. Read the file first (the default fs-observation-policy requires it), unless you just created or edited it in this session."
                    .to_owned(),
            ),
        ),
    )?;

    let tools = ctx
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-fs requires tools"))?;

    let mut parameters = json!({
        "file_path": {"type": "string", "required": true, "description": "Path to edit, resolved by the filesystem backend."},
        "old_string": {"type": "string", "required": true, "description": "Literal text to replace. Must match exactly."},
        "new_string": {"type": "string", "required": true, "description": "Literal replacement text. Use an empty string to delete the match."},
        "replace_all": {"type": "boolean", "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once."},
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
                "before": {"type": "string", "required": true},
                "after": {"type": "string", "required": true},
            },
        }),
        Arc::new(|args: &EditArgsRaw, value: &EditOutcome| {
            Ok(vec![ContentBlock::Text {
                text: format_edit_output(&value.path, args.replace_all.unwrap_or(false)),
            }])
        }),
    )
    .presentation_meta(Arc::new(|args: &EditArgsRaw, value: &EditOutcome| {
        Ok(json!({"diffs": compute_hunk_diffs(&args.file_path, &value.before, &value.after)}))
    }));

    let definition = define_tool(
        DefineToolOptions::new(
            "edit",
            "Edit an existing UTF-8 text file by replacing literal text.",
            parameters,
            output,
            Arc::new(move |args: EditArgsRaw, execution| {
                let ctx = execute_ctx.clone();
                let sandbox = Arc::clone(&execute_sandbox);
                Box::pin(async move {
                    let input = parse_edit_args(&args)?;
                    let escalation = FsEscalationArgs {
                        sandbox_permissions: args.sandbox_permissions.clone(),
                        justification: args.justification.clone(),
                    };
                    let policy = sandbox
                        .resolve_policy(
                            "edit",
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
                    let edit_request = FsEditRequest {
                        old_string: input.old_string,
                        new_string: input.new_string,
                        replace_all: input.replace_all,
                    };
                    let outcome = match (async {
                        let reply = ctx
                            .events()
                            .waterfall(
                                &ctx,
                                "fs/edit-intent",
                                &EventArgs::from_values(vec![
                                    Arc::new(target.clone()),
                                    Arc::new(execution.execution().clone()),
                                ]),
                                || Box::pin(async move { Ok(EventReply::Undefined) }),
                            )
                            .await?;
                        let version = reply
                            .downcast::<FsVersion>()
                            .map(|version| (*version).clone());
                        filesystem
                            .edit_text(
                                &target,
                                &edit_request,
                                version.as_ref(),
                                Some(&execution.signal()),
                                policy.as_ref(),
                            )
                            .await
                    })
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
                    Ok(EditOutcome {
                        path: target.display_path,
                        before: outcome.before,
                        after: outcome.after,
                    })
                })
            }),
        )
        .present_call(Arc::new(|args: &EditArgsRaw| {
            Some(ToolCallView::Diff(DiffCallView {
                title: format!("Edit {}", args.file_path),
                diffs: vec![FileDiff {
                    path: args.file_path.clone(),
                    old_text: if args.old_string.is_empty() {
                        None
                    } else {
                        Some(args.old_string.clone())
                    },
                    new_text: args.new_string.clone(),
                }],
                locations: Some(vec![FileLocation {
                    path: args.file_path.clone(),
                    line: None,
                }]),
            }))
        }))
        .present_result(Arc::new(|args: &EditArgsRaw, result: &ToolResult| {
            if result.is_error {
                return None;
            }
            let diffs = result.meta.as_ref().and_then(diffs_from_meta)?;
            Some(ToolResultView::Diff(DiffResultView {
                title: Some(format!("Edit {}", args.file_path)),
                diffs,
            }))
        })),
    )?;
    tools.register(ctx, definition)?;
    Ok(())
}

/// Raw schema-validated edit arguments, including the escalation fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditArgsRaw {
    /// Path to edit.
    pub file_path: String,
    /// Literal text to replace.
    pub old_string: String,
    /// Literal replacement text.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_all: Option<bool>,
    /// The wider sandbox mode this edit needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_permissions: Option<String>,
    /// One-sentence justification for the wider access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// Validated edit arguments after defaulting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditInput {
    /// Path to edit.
    pub file_path: String,
    /// Literal text to replace.
    pub old_string: String,
    /// Literal replacement text.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    pub replace_all: bool,
}

/// Validates the constraints the schema cannot express.
///
/// # Errors
///
/// Returns a blank-path, empty-old-string, or equal-pair failure.
pub fn parse_edit_args(args: &EditArgsRaw) -> anyhow::Result<EditInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    if args.old_string.is_empty() {
        anyhow::bail!("old_string must be a non-empty string");
    }
    if args.old_string == args.new_string {
        anyhow::bail!("old_string and new_string must differ");
    }
    Ok(EditInput {
        file_path: args.file_path.clone(),
        old_string: args.old_string.clone(),
        new_string: args.new_string.clone(),
        replace_all: args.replace_all.unwrap_or(false),
    })
}

/// Formats an edit success as a Claude-style model-facing message.
#[must_use]
pub fn format_edit_output(display_path: &str, replace_all: bool) -> String {
    if replace_all {
        format!(
            "The file {display_path} has been updated. All occurrences were successfully replaced."
        )
    } else {
        format!("The file {display_path} has been updated successfully.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(old_string: &str, new_string: &str) -> EditArgsRaw {
        EditArgsRaw {
            file_path: "a.txt".to_owned(),
            old_string: old_string.to_owned(),
            new_string: new_string.to_owned(),
            replace_all: None,
            sandbox_permissions: None,
            justification: None,
        }
    }

    #[test]
    fn parse_rejects_blank_empty_and_equal() {
        assert!(
            parse_edit_args(&EditArgsRaw {
                file_path: "  ".to_owned(),
                old_string: "a".to_owned(),
                new_string: "b".to_owned(),
                replace_all: None,
                sandbox_permissions: None,
                justification: None,
            })
            .is_err()
        );
        assert!(parse_edit_args(&raw("", "b")).is_err());
        assert!(parse_edit_args(&raw("a", "a")).is_err());
    }

    #[test]
    fn parse_defaults_replace_all_to_false() {
        let input = parse_edit_args(&raw("a", "b")).expect("valid");
        assert_eq!(input.old_string, "a");
        assert_eq!(input.new_string, "b");
        assert!(!input.replace_all);
    }

    #[test]
    fn format_edit_output_selects_wording() {
        assert_eq!(
            format_edit_output("/a", false),
            "The file /a has been updated successfully."
        );
        assert_eq!(
            format_edit_output("/a", true),
            "The file /a has been updated. All occurrences were successfully replaced."
        );
    }
}
