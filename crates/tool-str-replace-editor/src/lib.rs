//! Model-facing persistent view/create/literal-replace/line-insert editor.

use std::{cmp::Ordering, fmt::Write as _, path::Path, sync::Arc};

use seekdeep_cordis::{Context, EventArgs, EventReply, Plugin};
use seekdeep_fs::{
    FS, FileSystem, FsError, FsErrorCode, FsInfo, FsKind, FsObservation, FsTarget, FsVersion,
    FsWriteIntent,
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::ContentBlock;
use seekdeep_sandbox::{SandboxExecutionPolicy, sandbox_denial_marker};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest, SandboxPolicyService};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, DiffCallView, FileDiff, FileLocation, GenericCallView,
    TOOLS, ToolCallKind, ToolCallView, ToolRunContext, define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Cordis plugin name.
pub const NAME: &str = "tool-str-replace-editor";
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "tool-str-replace-editor-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-tool-str-replace-editor";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

const DEFAULT_DESCRIPTION: &str = r"Custom editing tool for viewing, creating and editing files
* State is persistent across command calls and discussions with the user
* If `path` is a file, `view` displays the result of applying `cat -n`. If `path` is a directory, `view` lists non-hidden files and directories up to 2 levels deep
* The `create` command cannot be used if the specified `path` already exists as a file
* If a `command` generates a long output, it will be truncated and marked with `<response clipped>`

Notes for using the `str_replace` command:
* The `old_str` parameter should match EXACTLY one or more consecutive lines from the original file. Be mindful of whitespaces!
* If the `old_str` parameter is not unique in the file, the replacement will not be performed. Make sure to include enough context in `old_str` to make it unique
* The `new_str` parameter should contain the edited lines that should replace the `old_str`";

/// Raw plugin configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Maximum returned view UTF-16 characters before clipping.
    pub max_output_chars: Option<f64>,
    /// Model-facing tool description.
    pub description: Option<String>,
    /// Schemastery object schemas preserve undeclared values in loose mode.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
struct ResolvedConfig {
    max_output_chars: usize,
    description: String,
}

/// Editor command discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorCommand {
    /// View a file or directory.
    View,
    /// Create one absent file.
    Create,
    /// Replace one unique literal span.
    StrReplace,
    /// Insert lines after a zero-based boundary count.
    Insert,
}

/// Tool arguments after JSON-schema decoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorArgs {
    /// Command to perform.
    pub command: EditorCommand,
    /// Absolute path.
    pub path: String,
    /// Create content.
    #[serde(default)]
    pub file_text: Option<String>,
    /// Zero-based insertion boundary.
    #[serde(default)]
    pub insert_line: Option<f64>,
    /// Replacement or insertion text.
    #[serde(default)]
    pub new_str: Option<String>,
    /// Literal search text.
    #[serde(default)]
    pub old_str: Option<String>,
    /// One-based inclusive file-view range.
    #[serde(default)]
    pub view_range: Option<Vec<f64>>,
}

#[derive(Clone)]
struct MutationPolicy {
    policy: Option<Arc<SandboxPolicyService>>,
}

impl MutationPolicy {
    fn new(context: &Context, filesystem: &Arc<dyn FileSystem>) -> anyhow::Result<Self> {
        let policy = if filesystem.sandbox_mode().is_some() {
            Some(context.get(SANDBOX_POLICY).ok_or_else(|| {
                anyhow::anyhow!(
                    "tool-str-replace-editor: the mounted filesystem confines but ctx.sandboxPolicy is missing"
                )
            })?)
        } else {
            None
        };
        Ok(Self { policy })
    }

    fn resolve(
        &self,
        execution: &ToolRunContext,
    ) -> anyhow::Result<Option<SandboxExecutionPolicy>> {
        self.policy
            .as_ref()
            .map(|policy| {
                policy.resolve(SandboxPolicyRequest {
                    session: execution
                        .agent
                        .as_ref()
                        .map(|agent| agent.session().as_ref()),
                    mode: None,
                })
            })
            .transpose()
    }

    fn map_error(error: anyhow::Error, policy: Option<&SandboxExecutionPolicy>) -> anyhow::Error {
        let Some(fs_error) = error.downcast_ref::<FsError>() else {
            return error;
        };
        if fs_error.code != FsErrorCode::FsSandboxDenied {
            return error;
        }
        let mode = policy
            .expect("confining filesystem always resolves a sandbox policy")
            .mode;
        anyhow::Error::new(FsError::new(
            sandbox_denial_marker(mode),
            FsErrorCode::FsSandboxDenied,
        ))
    }
}

/// Registers the editor tool over the mounted filesystem.
///
/// # Errors
///
/// Returns invalid configuration, missing service/policy, or tool registration failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let config = resolve_config(config)?;
    let filesystem = context
        .get(FS)
        .ok_or_else(|| anyhow::anyhow!("tool-str-replace-editor requires fs"))?
        .filesystem();
    let policy = MutationPolicy::new(context, &filesystem)?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-str-replace-editor requires tools"))?;
    let output = DefineToolOutput::new(
        json!({"type":"string"}),
        Arc::new(|_args: &EditorArgs, value: &String| {
            Ok(vec![ContentBlock::Text {
                text: value.clone(),
            }])
        }),
    );
    let execute_context = context.clone();
    let execute_filesystem = filesystem;
    let execute_policy = policy;
    let max_output_chars = config.max_output_chars;
    let definition = define_tool(
        DefineToolOptions::new(
            "str_replace_editor",
            config.description,
            parameter_schema(),
            output,
            Arc::new(move |args: EditorArgs, execution| {
                let context = execute_context.clone();
                let filesystem = execute_filesystem.clone();
                let policy = execute_policy.clone();
                Box::pin(async move {
                    execute_editor(
                        &context,
                        &filesystem,
                        &policy,
                        &args,
                        max_output_chars,
                        &execution,
                    )
                    .await
                })
            }),
        )
        .present_call(Arc::new(|args: &EditorArgs| {
            Some(present_editor_call(args))
        })),
    )?;
    tools.register(context, definition)?;
    Ok(())
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, ["tools", "fs"], |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(normalize_config_value(config))?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        let config: Config = serde_json::from_value(normalize_config_value(value.clone()))?;
        let resolved = resolve_config(&config)?;
        let mut normalized = config.extra;
        normalized.insert(
            "maxOutputChars".to_owned(),
            json!(resolved.max_output_chars),
        );
        normalized.insert("description".to_owned(), json!(resolved.description));
        Ok(Value::Object(normalized))
    })
}

/// Registers the package's explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn normalize_config_value(value: Value) -> Value {
    if value.is_null() {
        Value::Object(Map::new())
    } else {
        value
    }
}

fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let max_output_chars = config.max_output_chars.unwrap_or(16_000.0);
    anyhow::ensure!(
        max_output_chars.is_finite()
            && max_output_chars > 0.0
            && max_output_chars.fract() == 0.0
            && max_output_chars <= MAX_SAFE_INTEGER,
        "tool-str-replace-editor: maxOutputChars must be a positive safe integer"
    );
    let max_output_chars = ryu_js::Buffer::new()
        .format(max_output_chars)
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("maxOutputChars does not fit this platform"))?;
    let description = config
        .description
        .clone()
        .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_owned());
    anyhow::ensure!(
        !description.trim().is_empty(),
        "tool-str-replace-editor: description must be non-empty"
    );
    Ok(ResolvedConfig {
        max_output_chars,
        description,
    })
}

fn parameter_schema() -> Value {
    json!({
        "command": {"type":"string","required":true,"enum":["view","create","str_replace","insert"],"description":"The commands to run. Allowed options are: `view`, `create`, `str_replace`, `insert`."},
        "path": {"type":"string","required":true,"description":"Absolute path to file or directory, e.g. `/repo/file.py` or `/repo`."},
        "file_text": {"type":"string","description":"Required parameter of `create` command, with the content of the file to be created."},
        "insert_line": {"type":"integer","description":"Required parameter of `insert` command. The `new_str` will be inserted AFTER the line `insert_line` of `path`."},
        "new_str": {"type":"string","description":"Optional parameter of `str_replace` command containing the new string (if not given, no string will be added). Required parameter of `insert` command containing the string to insert."},
        "old_str": {"type":"string","description":"Required parameter of `str_replace` command containing the string in `path` to replace."},
        "view_range": {"type":"array","items":{"type":"integer"},"description":"Optional parameter of `view` command when `path` points to a file. If none is given, the full file is shown. If provided, the file will be shown in the indicated line number range, e.g. [11, 12] will show lines 11 and 12. Indexing at 1 to start. Setting `[start_line, -1]` shows all lines from `start_line` to the end of the file."}
    })
}

async fn execute_editor(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    policy: &MutationPolicy,
    args: &EditorArgs,
    max_output_chars: usize,
    execution: &ToolRunContext,
) -> anyhow::Result<String> {
    match args.command {
        EditorCommand::View => {
            view_path(
                context,
                filesystem,
                &args.path,
                args.view_range.as_deref(),
                max_output_chars,
                execution,
            )
            .await
        }
        EditorCommand::Create => {
            create_file(
                context,
                filesystem,
                policy,
                &args.path,
                args.file_text.as_deref(),
                execution,
            )
            .await
        }
        EditorCommand::StrReplace => {
            replace_in_file(
                context,
                filesystem,
                policy,
                &args.path,
                args.old_str.as_deref(),
                args.new_str.as_deref(),
                execution,
            )
            .await
        }
        EditorCommand::Insert => {
            insert_in_file(
                context,
                filesystem,
                policy,
                &args.path,
                args.insert_line,
                args.new_str.as_deref(),
                execution,
            )
            .await
        }
    }
}

async fn resolve_target(
    filesystem: &Arc<dyn FileSystem>,
    path: &str,
    signal: &seekdeep_llm::AbortSignal,
) -> anyhow::Result<FsTarget> {
    anyhow::ensure!(!path.trim().is_empty(), "path must be a non-empty string");
    anyhow::ensure!(
        Path::new(path).is_absolute(),
        "The path {path} is not an absolute path, it should start with `/`. Maybe you meant /{path}?"
    );
    filesystem.resolve(path, None, Some(signal)).await
}

async fn stat_existing(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    target: &FsTarget,
    command: EditorCommand,
    execution: &ToolRunContext,
) -> anyhow::Result<FsInfo> {
    let signal = execution.signal();
    let info = filesystem.stat(target, Some(&signal)).await?;
    let Some(info) = info else {
        emit_observed(context, target, FsObservation::Absent, execution)?;
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "The path {} does not exist. Please provide a valid path.",
                target.display_path
            ),
            FsErrorCode::FsNotFound,
        )));
    };
    if info.kind == FsKind::Directory && command != EditorCommand::View {
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "The path {} is a directory and only the `view` command can be used on directories",
                target.display_path
            ),
            FsErrorCode::FsNotRegularFile,
        )));
    }
    Ok(info)
}

fn required<'a>(
    value: Option<&'a str>,
    parameter: &str,
    command: &str,
    allow_empty: bool,
) -> anyhow::Result<&'a str> {
    let value = value.ok_or_else(|| {
        anyhow::anyhow!("Parameter `{parameter}` is required for command: {command}")
    })?;
    anyhow::ensure!(
        allow_empty || !value.is_empty(),
        "Parameter `{parameter}` is empty for command: {command}"
    );
    Ok(value)
}

async fn view_path(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    path: &str,
    view_range: Option<&[f64]>,
    max_output_chars: usize,
    execution: &ToolRunContext,
) -> anyhow::Result<String> {
    let signal = execution.signal();
    let target = resolve_target(filesystem, path, &signal).await?;
    let info = stat_existing(context, filesystem, &target, EditorCommand::View, execution).await?;
    match info.kind {
        FsKind::Directory => {
            anyhow::ensure!(
                view_range.is_none(),
                "The `view_range` parameter is not allowed when `path` points to a directory."
            );
            list_directory(filesystem, &target, max_output_chars, &signal).await
        }
        FsKind::File => {
            let file_content = filesystem.read_text(&target, Some(&signal)).await?;
            emit_observed(
                context,
                &target,
                FsObservation::Present {
                    version: info.version,
                },
                execution,
            )?;
            format_file_view(
                &target.display_path,
                &file_content,
                max_output_chars,
                view_range,
            )
        }
        FsKind::Other => Err(anyhow::Error::new(FsError::new(
            format!(
                "cannot view \"{}\": not a regular file or directory",
                target.display_path
            ),
            FsErrorCode::FsNotRegularFile,
        ))),
    }
}

async fn create_file(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    policy: &MutationPolicy,
    path: &str,
    file_text: Option<&str>,
    execution: &ToolRunContext,
) -> anyhow::Result<String> {
    let create_content = required(file_text, "file_text", "create", true)?;
    let sandbox_policy = policy.resolve(execution)?;
    let signal = execution.signal();
    let target = resolve_target(filesystem, path, &signal).await?;
    if filesystem.stat(&target, Some(&signal)).await?.is_some() {
        anyhow::bail!(
            "File already exists at: {}. Cannot overwrite files using command `create`.",
            target.display_path
        );
    }
    let intent = write_intent(context, &target, execution).await?;
    let outcome = filesystem
        .write_text(
            &target,
            create_content,
            Some(&intent),
            Some(&signal),
            sandbox_policy.as_ref(),
        )
        .await
        .map_err(|error| MutationPolicy::map_error(error, sandbox_policy.as_ref()))?;
    emit_observed(
        context,
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        execution,
    )?;
    Ok(format!(
        "New file created successfully at: {}",
        target.display_path
    ))
}

async fn replace_in_file(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    policy: &MutationPolicy,
    path: &str,
    old_str: Option<&str>,
    new_str: Option<&str>,
    execution: &ToolRunContext,
) -> anyhow::Result<String> {
    let sandbox_policy = policy.resolve(execution)?;
    let signal = execution.signal();
    let target = resolve_target(filesystem, path, &signal).await?;
    let observed_version = edit_intent(context, &target, execution).await?;
    let old_value = required(old_str, "old_str", "str_replace", false)?;
    let new_value = new_str.unwrap_or_default();
    let info = stat_existing(
        context,
        filesystem,
        &target,
        EditorCommand::StrReplace,
        execution,
    )
    .await?;
    if info.kind != FsKind::File {
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "cannot edit \"{}\": not a regular file",
                target.display_path
            ),
            FsErrorCode::FsNotRegularFile,
        )));
    }
    let before = filesystem.read_text(&target, Some(&signal)).await?;
    let offsets = match_offsets(&before, old_value);
    let Some(offset) = offsets.first().copied() else {
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "No replacement was performed, old_str `{old_value}` did not appear verbatim in {}.",
                target.display_path
            ),
            FsErrorCode::FsEditNotFound,
        )));
    };
    if offsets.len() > 1 {
        let lines = line_numbers_at(&before, &offsets)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "No replacement was performed. Multiple occurrences of old_str `{old_value}` in lines [{lines}]. Please ensure it is unique"
            ),
            FsErrorCode::FsAmbiguousEdit,
        )));
    }
    let mut after = String::with_capacity(before.len() - old_value.len() + new_value.len());
    after.push_str(&before[..offset]);
    after.push_str(new_value);
    after.push_str(&before[offset + old_value.len()..]);
    let expected = FsWriteIntent::ReplaceIfVersion {
        version: observed_version.unwrap_or(info.version),
    };
    let outcome = filesystem
        .write_text(
            &target,
            &after,
            Some(&expected),
            Some(&signal),
            sandbox_policy.as_ref(),
        )
        .await
        .map_err(|error| MutationPolicy::map_error(error, sandbox_policy.as_ref()))?;
    emit_observed(
        context,
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        execution,
    )?;
    Ok(format!(
        "The file {} has been edited successfully.",
        target.display_path
    ))
}

async fn insert_in_file(
    context: &Context,
    filesystem: &Arc<dyn FileSystem>,
    policy: &MutationPolicy,
    path: &str,
    insert_line: Option<f64>,
    new_str: Option<&str>,
    execution: &ToolRunContext,
) -> anyhow::Result<String> {
    let insert_line = insert_line.ok_or_else(|| {
        anyhow::anyhow!("Parameter `insert_line` is required for command: insert")
    })?;
    let value = required(new_str, "new_str", "insert", true)?;
    let sandbox_policy = policy.resolve(execution)?;
    let signal = execution.signal();
    let target = resolve_target(filesystem, path, &signal).await?;
    let observed_version = edit_intent(context, &target, execution).await?;
    let info = stat_existing(
        context,
        filesystem,
        &target,
        EditorCommand::Insert,
        execution,
    )
    .await?;
    if info.kind != FsKind::File {
        return Err(anyhow::Error::new(FsError::new(
            format!(
                "cannot insert into \"{}\": not a regular file",
                target.display_path
            ),
            FsErrorCode::FsNotRegularFile,
        )));
    }
    let before = filesystem.read_text(&target, Some(&signal)).await?;
    let lines = before.split('\n').collect::<Vec<_>>();
    let index = nonnegative_index(insert_line).filter(|index| *index <= lines.len());
    anyhow::ensure!(
        index.is_some(),
        "Invalid `insert_line` parameter: {}. It should be within the range of lines of the file: [0, {}]",
        js_number(insert_line),
        lines.len()
    );
    let index = index.expect("validated insertion index");
    let mut after_lines = Vec::with_capacity(lines.len() + value.matches('\n').count() + 1);
    after_lines.extend_from_slice(&lines[..index]);
    after_lines.extend(value.split('\n'));
    after_lines.extend_from_slice(&lines[index..]);
    let after = after_lines.join("\n");
    let expected = FsWriteIntent::ReplaceIfVersion {
        version: observed_version.unwrap_or(info.version),
    };
    let outcome = filesystem
        .write_text(
            &target,
            &after,
            Some(&expected),
            Some(&signal),
            sandbox_policy.as_ref(),
        )
        .await
        .map_err(|error| MutationPolicy::map_error(error, sandbox_policy.as_ref()))?;
    emit_observed(
        context,
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        execution,
    )?;
    Ok(format!(
        "The file {} has been edited successfully.",
        target.display_path
    ))
}

async fn write_intent(
    context: &Context,
    target: &FsTarget,
    execution: &ToolRunContext,
) -> anyhow::Result<FsWriteIntent> {
    let default = FsWriteIntent::CreateIfAbsent;
    let reply = context
        .events()
        .waterfall(
            context,
            "fs/write-intent",
            &EventArgs::from_values(vec![
                Arc::new(target.clone()),
                Arc::new(execution.execution().clone()),
            ]),
            || Box::pin(async { Ok(EventReply::Undefined) }),
        )
        .await?;
    Ok(reply
        .downcast::<FsWriteIntent>()
        .map_or(default, |intent| (*intent).clone()))
}

async fn edit_intent(
    context: &Context,
    target: &FsTarget,
    execution: &ToolRunContext,
) -> anyhow::Result<Option<FsVersion>> {
    let reply = context
        .events()
        .waterfall(
            context,
            "fs/edit-intent",
            &EventArgs::from_values(vec![
                Arc::new(target.clone()),
                Arc::new(execution.execution().clone()),
            ]),
            || Box::pin(async { Ok(EventReply::Undefined) }),
        )
        .await?;
    Ok(reply
        .downcast::<FsVersion>()
        .map(|version| (*version).clone()))
}

fn emit_observed(
    context: &Context,
    target: &FsTarget,
    observation: FsObservation,
    execution: &ToolRunContext,
) -> anyhow::Result<()> {
    context.events().emit(
        context,
        "fs/observed",
        &EventArgs::from_values(vec![
            Arc::new(target.clone()),
            Arc::new(observation),
            Arc::new(execution.execution().clone()),
        ]),
    )
}

fn format_file_view(
    path: &str,
    content: &str,
    max_output_chars: usize,
    view_range: Option<&[f64]>,
) -> anyhow::Result<String> {
    let all_lines = content.split('\n').collect::<Vec<_>>();
    let mut initial_line = 1_usize;
    let mut final_line = None;
    let mut prompt = format!(
        "Here's the content of {path} with line numbers (which has a total of {} lines)",
        all_lines.len()
    );
    if let Some(view_range) = view_range {
        anyhow::ensure!(
            view_range.len() == 2 && view_range.iter().all(|value| value.fract() == 0.0),
            "Invalid `view_range`. It should be a list of two integers."
        );
        let requested_initial = view_range[0];
        let requested_final = view_range[1];
        let initial_index = nonnegative_index(requested_initial)
            .filter(|line| *line >= 1 && *line <= all_lines.len());
        let final_index = if js_number(requested_final) == "-1" {
            Some(None)
        } else {
            nonnegative_index(requested_final).map(Some)
        };
        anyhow::ensure!(
            initial_index.is_some(),
            "Invalid `view_range`: [{}, {}]. Its first element `{}` should be within the range of lines of the file: [1, {}]",
            js_number(requested_initial),
            js_number(requested_final),
            js_number(requested_initial),
            all_lines.len()
        );
        anyhow::ensure!(
            final_index
                .as_ref()
                .is_some_and(|line| line.is_none_or(|line| line <= all_lines.len())),
            "Invalid `view_range`: [{}, {}]. Its second element `{}` should be smaller than the number of lines in the file: `{}`",
            js_number(requested_initial),
            js_number(requested_final),
            js_number(requested_final),
            all_lines.len()
        );
        initial_line = initial_index.expect("validated first line");
        final_line = final_index.expect("validated final line");
        anyhow::ensure!(
            final_line.is_none_or(|line| line >= initial_line),
            "Invalid `view_range`: [{}, {}]. Its second element `{}` should be larger or equal than its first `{}`",
            js_number(requested_initial),
            js_number(requested_final),
            js_number(requested_final),
            js_number(requested_initial)
        );
        write!(
            prompt,
            " with view_range=[{}, {}]",
            js_number(requested_initial),
            js_number(requested_final)
        )?;
    }
    let end = final_line.unwrap_or(all_lines.len());
    let numbered = all_lines[initial_line - 1..end]
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}  {line}", initial_line + index))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(maybe_truncate(
        &format!("{prompt}:\n{numbered}\n"),
        max_output_chars,
    ))
}

async fn list_directory(
    filesystem: &Arc<dyn FileSystem>,
    target: &FsTarget,
    max_output_chars: usize,
    signal: &seekdeep_llm::AbortSignal,
) -> anyhow::Result<String> {
    let mut rows = vec![format!("d\t{}", target.display_path)];
    rows.extend(visit_directory(filesystem, target, 1, signal).await?);
    rows.sort_by(|left, right| {
        let left = left
            .split_once('\t')
            .map_or(left.as_str(), |(_, path)| path);
        let right = right
            .split_once('\t')
            .map_or(right.as_str(), |(_, path)| path);
        codepoint_compare(left, right)
    });
    let listing = maybe_truncate(&(rows.join("\n") + "\n"), max_output_chars);
    Ok(format!(
        "Here're the files and directories up to 2 levels deep in {}, excluding hidden items, node_modules, and Python cache directories:\n{listing}\n",
        target.display_path
    ))
}

fn visit_directory<'a>(
    filesystem: &'a Arc<dyn FileSystem>,
    target: &'a FsTarget,
    depth: usize,
    signal: &'a seekdeep_llm::AbortSignal,
) -> futures::future::BoxFuture<'a, anyhow::Result<Vec<String>>> {
    Box::pin(async move {
        let entries = filesystem.list_dir(target, Some(signal)).await?;
        let mut rows = Vec::new();
        for entry in entries.into_iter().filter(|entry| {
            !entry.name.starts_with('.')
                && entry.name != "node_modules"
                && entry.name != "__pycache__"
        }) {
            let kind = match entry.kind {
                FsKind::Directory => 'd',
                FsKind::File => 'f',
                FsKind::Other => '?',
            };
            rows.push(format!("{kind}\t{}", entry.target.display_path));
            if entry.kind == FsKind::Directory && depth < 2 {
                rows.extend(visit_directory(filesystem, &entry.target, depth + 1, signal).await?);
            }
        }
        Ok(rows)
    })
}

fn maybe_truncate(content: &str, max_output_chars: usize) -> String {
    let units = content.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max_output_chars {
        content.to_owned()
    } else {
        format!(
            "{}{TRUNCATED_MESSAGE}",
            String::from_utf16_lossy(&units[..max_output_chars])
        )
    }
}

fn codepoint_compare(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn match_offsets(content: &str, search: &str) -> Vec<usize> {
    content
        .match_indices(search)
        .map(|(offset, _)| offset)
        .collect()
}

fn line_numbers_at(content: &str, offsets: &[usize]) -> Vec<usize> {
    let mut line = 1;
    let mut cursor = 0;
    offsets
        .iter()
        .map(|offset| {
            while cursor < *offset {
                if content.as_bytes()[cursor] == b'\n' {
                    line += 1;
                }
                cursor += 1;
            }
            line
        })
        .collect()
}

fn js_number(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}

fn nonnegative_index(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    js_number(value).parse().ok()
}

fn present_editor_call(args: &EditorArgs) -> ToolCallView {
    let location = |line| FileLocation {
        path: args.path.clone(),
        line,
    };
    match args.command {
        EditorCommand::View => ToolCallView::Generic(GenericCallView {
            title: format!("view {}", args.path),
            kind: Some(ToolCallKind::Read),
            raw_input: None,
            content: None,
            locations: Some(vec![location(None)]),
        }),
        EditorCommand::Create => ToolCallView::Diff(DiffCallView {
            title: format!("create {}", args.path),
            diffs: vec![FileDiff {
                path: args.path.clone(),
                old_text: None,
                new_text: args.file_text.clone().unwrap_or_default(),
            }],
            locations: Some(vec![location(None)]),
        }),
        EditorCommand::StrReplace => ToolCallView::Diff(DiffCallView {
            title: format!("str_replace {}", args.path),
            diffs: vec![FileDiff {
                path: args.path.clone(),
                old_text: args.old_str.clone(),
                new_text: args.new_str.clone().unwrap_or_default(),
            }],
            locations: Some(vec![location(None)]),
        }),
        EditorCommand::Insert => ToolCallView::Generic(GenericCallView {
            title: format!("insert {}", args.path),
            kind: Some(ToolCallKind::Edit),
            raw_input: None,
            content: None,
            locations: Some(vec![location(
                args.insert_line.map(|line| (line + 1.0).max(1.0)),
            )]),
        }),
    }
}
