//! Persistent owner-scoped `bash` tool over terminal sessions.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_llm::ContentBlock;
use seekdeep_terminal::{
    TERMINALS, TerminalReadResult, TerminalSendResult, TerminalSessionId, TerminalSessionStatus,
    TerminalSpawnRequest, TerminalWaitReason,
};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, TOOLS, TerminalCallView, ToolCallView, ToolDefinition,
    define_tool,
};
use seekdeep_util::timeout::{deadline, timeout_of};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";
const LOST_PREFIX_MESSAGE: &str = "<response clipped><NOTE>The beginning of this command output was dropped by the terminal scrollback limit. The following text is the earliest retained output.</NOTE>\n";
const SHELL_RESET_MESSAGE: &str = "The persistent bash shell was reset; the next bash call starts from the workspace with a fresh current directory and environment.";
const SHELL_PROMPT: &str = "__DSH_PERSISTENT_BASH_PROMPT__ ";
const TIMEOUT_CODE: &str = "PERSISTENT_BASH_TIMEOUT";
const SCROLLBACK_PAGE_LINES: f64 = 1_000.0;
const POLL_INTERVAL_MS: u64 = 25;
const DEFAULT_DESCRIPTION: &str = "Run commands in a persistent bash shell. State, including the current directory and exported environment variables, persists across calls for this agent.";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Loader plugin name.
pub const NAME: &str = "tool-bash-persistent";
/// Required services.
pub const INJECT: &[&str] = &["tools", "terminals"];

/// Persistent-shell configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// PTY backend type; defaults `shell`.
    pub backend_type: Option<String>,
    /// Per-command wall-clock timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Maximum returned UTF-16 code units before clipping.
    pub max_output_chars: Option<u64>,
    /// Model-facing description override.
    pub description: Option<String>,
}

#[derive(Clone)]
struct ResolvedConfig {
    backend_type: String,
    timeout_ms: u64,
    max_output_chars: usize,
    description: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct BashArgs {
    command: String,
}

struct Markers {
    start: String,
    end: String,
}

struct RetainedOutput {
    text: String,
    truncated: bool,
}

struct CapturedOutput {
    text: String,
    incomplete: bool,
    exit_code: Option<i32>,
}

struct ShellRecord {
    owner: Arc<Agent>,
    id: TerminalSessionId,
}

struct PersistentShells {
    terminals: Arc<seekdeep_terminal::TerminalSessionService>,
    backend_type: String,
    lifecycle: seekdeep_llm::AbortSignal,
    live: Mutex<std::collections::HashMap<usize, ShellRecord>>,
    locks: Mutex<std::collections::HashMap<usize, Arc<tokio::sync::Mutex<()>>>>,
}

impl PersistentShells {
    fn install(
        context: &Context,
        terminals: Arc<seekdeep_terminal::TerminalSessionService>,
        backend_type: String,
    ) -> anyhow::Result<Arc<Self>> {
        let shells = Arc::new(Self {
            terminals,
            backend_type,
            lifecycle: seekdeep_llm::AbortSignal::default(),
            live: Mutex::new(std::collections::HashMap::new()),
            locks: Mutex::new(std::collections::HashMap::new()),
        });
        let cleanup = Arc::clone(&shells);
        context.own(EffectHandle::new(
            "tool-bash-persistent shell cleanup",
            move || {
                let shells = Arc::clone(&cleanup);
                Box::pin(async move {
                    shells.lifecycle.abort_with_reason(Value::String(
                        "tool-bash-persistent disposed during shell creation".to_owned(),
                    ));
                    let records = shells
                        .live
                        .lock()
                        .drain()
                        .map(|(_, value)| value)
                        .collect::<Vec<_>>();
                    let failures = futures::future::join_all(records.into_iter().map(|record| {
                        let terminals = shells.terminals.clone();
                        async move {
                            terminals
                                .kill(
                                    &record.owner,
                                    &record.id,
                                    Some("tool-bash-persistent disposed"),
                                )
                                .await
                        }
                    }))
                    .await
                    .into_iter()
                    .filter_map(Result::err)
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>();
                    if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(failures.join("; ")))
                    }
                })
            },
        ))?;
        Ok(shells)
    }

    fn owner_key(owner: &Arc<Agent>) -> usize {
        Arc::as_ptr(owner) as usize
    }

    fn lock_for(&self, owner: &Arc<Agent>) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .entry(Self::owner_key(owner))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn close(&self, owner: &Arc<Agent>, id: &TerminalSessionId, reason: &str) {
        if self
            .terminals
            .list(owner)
            .iter()
            .any(|snapshot| &snapshot.session_id == id)
        {
            let _ = self.terminals.kill(owner, id, Some(reason)).await;
        }
    }

    async fn reset(&self, owner: &Arc<Agent>, reason: &str) {
        let record = self.live.lock().remove(&Self::owner_key(owner));
        if let Some(record) = record {
            self.close(owner, &record.id, reason).await;
        }
    }

    async fn get(
        &self,
        owner: &Arc<Agent>,
        signal: &seekdeep_llm::AbortSignal,
    ) -> anyhow::Result<TerminalSessionId> {
        let key = Self::owner_key(owner);
        if let Some(id) = self.live.lock().get(&key).map(|record| record.id.clone())
            && self
                .terminals
                .list(owner)
                .iter()
                .any(|snapshot| snapshot.session_id == id)
        {
            return Ok(id);
        }
        self.live.lock().remove(&key);
        let combined = seekdeep_llm::AbortSignal::fuse(signal, &self.lifecycle);
        let spawned = self
            .terminals
            .spawn(
                owner.clone(),
                TerminalSpawnRequest {
                    terminal_type: self.backend_type.clone(),
                    name: None,
                    cwd: owner.session().header().cwd.clone(),
                },
                Some(combined.clone()),
            )
            .await
            .map_err(anyhow::Error::new)?;
        self.live.lock().insert(
            key,
            ShellRecord {
                owner: owner.clone(),
                id: spawned.session_id.clone(),
            },
        );
        let initialized = async {
            let operation = self
                .terminals
                .start_send(
                    owner,
                    &spawned.session_id,
                    seekdeep_terminal::TerminalSendRequest {
                        text: format!("stty -echo; PS1={}", quote_for_bash(SHELL_PROMPT)),
                        submit: true,
                        signal: Some(combined),
                    },
                )
                .map_err(anyhow::Error::new)?;
            let result = operation.done().await.map_err(anyhow::Error::new)?;
            if matches!(result.session_status, TerminalSessionStatus::Exited { .. })
                || result.wait_reason == TerminalWaitReason::Timeout
            {
                anyhow::bail!("persistent bash shell did not accept initialization");
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = initialized {
            self.reset(owner, "persistent bash initialization failed")
                .await;
            return Err(error);
        }
        Ok(spawned.session_id)
    }
}

fn resolve_config(config: Config) -> anyhow::Result<ResolvedConfig> {
    let backend_type = config.backend_type.unwrap_or_else(|| "shell".to_owned());
    let timeout_ms = config.timeout_ms.unwrap_or(300_000);
    let max_output_chars = config.max_output_chars.unwrap_or(16_000);
    let description = config
        .description
        .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_owned());
    anyhow::ensure!(
        !backend_type.trim().is_empty(),
        "tool-bash-persistent: backendType must be non-empty"
    );
    anyhow::ensure!(
        (1..=MAX_SAFE_INTEGER).contains(&timeout_ms),
        "tool-bash-persistent: timeoutMs must be a positive safe integer"
    );
    anyhow::ensure!(
        (1..=MAX_SAFE_INTEGER).contains(&max_output_chars),
        "tool-bash-persistent: maxOutputChars must be a positive safe integer"
    );
    anyhow::ensure!(
        !description.trim().is_empty(),
        "tool-bash-persistent: description must be non-empty"
    );
    Ok(ResolvedConfig {
        backend_type,
        timeout_ms,
        max_output_chars: usize::try_from(max_output_chars).unwrap_or(usize::MAX),
        description,
    })
}

fn markers() -> Markers {
    let nonce = Uuid::new_v4();
    Markers {
        start: format!("__DSH_PERSISTENT_BASH_START_{nonce}__"),
        end: format!("__DSH_PERSISTENT_BASH_END_{nonce}:"),
    }
}

fn quote_for_bash(value: &str) -> String {
    format!(
        "$'{}'",
        value
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    )
}

fn wrap_command(command: &str, marker: &Markers) -> String {
    format!(
        "printf '%s\\n' {}; eval -- {}; __dsh_persistent_bash_status=$?; printf '%s%s\\n' {} \"$__dsh_persistent_bash_status\"",
        quote_for_bash(&marker.start),
        quote_for_bash(command),
        quote_for_bash(&marker.end)
    )
}

fn strip_prompt(text: &str) -> String {
    let mut result = text.trim_end_matches(['\r', '\n']).to_owned();
    while result.ends_with(SHELL_PROMPT) {
        result.truncate(result.len() - SHELL_PROMPT.len());
    }
    result.strip_suffix('\n').unwrap_or(&result).to_owned()
}

fn command_output(snapshot: &RetainedOutput, marker: &Markers) -> Option<CapturedOutput> {
    let end = snapshot.text.rfind(&marker.end)?;
    let after = &snapshot.text[end + marker.end.len()..];
    let status_text = after.trim_start_matches(['\r', '\n']);
    let digits = status_text
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let exit_code = digits.parse::<i32>().ok()?;
    let start_marker = snapshot.text[..end].rfind(&marker.start);
    let start = start_marker.map_or(0, |start| start + marker.start.len());
    Some(CapturedOutput {
        text: strip_prompt(snapshot.text[start..end].trim_start_matches(['\r', '\n'])),
        incomplete: start_marker.is_none(),
        exit_code: Some(exit_code),
    })
}

fn prompt_completed(result: &TerminalSendResult) -> bool {
    let viewport = result.viewport.trim_end_matches(['\r', '\n']);
    viewport.ends_with(SHELL_PROMPT)
}

fn next_scrollback_offset(page: &TerminalReadResult, offset: u64) -> Option<u64> {
    if page.text.is_empty() || page.line_end <= offset {
        None
    } else {
        Some(page.line_end)
    }
}

#[allow(clippy::cast_precision_loss)]
fn js_number(value: u64) -> f64 {
    value as f64
}

fn retained_scrollback(
    terminals: &seekdeep_terminal::TerminalSessionService,
    owner: &Arc<Agent>,
    id: &TerminalSessionId,
    latest: &TerminalReadResult,
) -> anyhow::Result<RetainedOutput> {
    let mut pages = if latest.text.is_empty() {
        Vec::new()
    } else {
        vec![latest.text.clone()]
    };
    let mut offset = latest.line_end;
    let mut truncated = latest.truncated;
    while offset < latest.total_lines {
        let page = terminals
            .read(
                owner,
                id,
                seekdeep_terminal::TerminalReadRequest {
                    offset: Some(js_number(offset)),
                    count: Some(SCROLLBACK_PAGE_LINES),
                },
            )
            .map_err(anyhow::Error::new)?;
        truncated |= page.truncated;
        if !page.text.is_empty() {
            pages.insert(0, page.text.clone());
        }
        let Some(next) = next_scrollback_offset(&page, offset) else {
            break;
        };
        if next >= page.total_lines {
            break;
        }
        offset = next;
    }
    Ok(RetainedOutput {
        text: pages.join("\n"),
        truncated,
    })
}

fn partial_output(
    snapshot: &RetainedOutput,
    marker: &Markers,
    fallback: &str,
    fallback_truncated: bool,
) -> CapturedOutput {
    if let Some(start) = snapshot.text.rfind(&marker.start) {
        return CapturedOutput {
            text: strip_prompt(
                snapshot.text[start + marker.start.len()..].trim_start_matches(['\r', '\n']),
            ),
            incomplete: snapshot.truncated,
            exit_code: None,
        };
    }
    let after_start = fallback.rfind(&marker.start).map_or(fallback, |start| {
        fallback[start + marker.start.len()..].trim_start_matches(['\r', '\n'])
    });
    let before_end = after_start
        .rfind(&marker.end)
        .map_or(after_start, |end| &after_start[..end]);
    CapturedOutput {
        text: strip_prompt(&before_end.replace(SHELL_PROMPT, "")),
        incomplete: snapshot.truncated || fallback_truncated || !fallback.contains(&marker.start),
        exit_code: None,
    }
}

fn utf16_prefix(text: &str, max_units: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    String::from_utf16_lossy(&units[..units.len().min(max_units)])
}

fn maybe_truncate(content: &str, max_chars: usize, incomplete: bool) -> String {
    if content.encode_utf16().count() <= max_chars && !incomplete {
        return content.to_owned();
    }
    format!("{}{TRUNCATED_MESSAGE}", utf16_prefix(content, max_chars))
}

fn append_status(content: &str, marker: Option<String>) -> String {
    marker.map_or_else(
        || content.to_owned(),
        |marker| {
            if content.is_empty() {
                marker
            } else {
                format!("{content}\n{marker}")
            }
        },
    )
}

fn render_captured(output: &CapturedOutput, max_chars: usize) -> String {
    let rendered = maybe_truncate(&output.text, max_chars, output.incomplete);
    let rendered = if output.incomplete && !output.text.is_empty() {
        format!("{LOST_PREFIX_MESSAGE}{rendered}")
    } else {
        rendered
    };
    append_status(
        &rendered,
        output
            .exit_code
            .filter(|code| *code != 0)
            .map(|code| format!("[exit code: {code}]")),
    )
}

fn render_shell_exit(content: &str, exit_code: Option<i32>, signal: Option<&str>) -> String {
    let marker = signal.map_or_else(
        || {
            exit_code.map_or_else(
                || "[shell exited]".to_owned(),
                |code| format!("[shell exited: code {code}]"),
            )
        },
        |signal| format!("[shell killed by signal: {signal}]"),
    );
    append_status(content, Some(marker))
}

#[allow(clippy::too_many_lines)] // One source-compatible PTY polling transaction owns every exit arm.
async fn execute_command(
    shells: &PersistentShells,
    owner: &Arc<Agent>,
    command: &str,
    config: &ResolvedConfig,
    upstream: &seekdeep_llm::AbortSignal,
) -> anyhow::Result<String> {
    let deadline = deadline(Some(upstream), js_number(config.timeout_ms), TIMEOUT_CODE)?;
    let id = shells.get(owner, &deadline.signal).await?;
    let marker = markers();
    let wrapped = wrap_command(command, &marker);
    let mut first = true;
    let mut fallback = String::new();
    let mut fallback_truncated = false;
    loop {
        let operation = match shells.terminals.start_send(
            owner,
            &id,
            seekdeep_terminal::TerminalSendRequest {
                text: if first {
                    wrapped.clone()
                } else {
                    String::new()
                },
                submit: first,
                signal: Some(deadline.signal.clone()),
            },
        ) {
            Ok(operation) => operation,
            Err(error) => {
                shells.reset(owner, "persistent bash send failed").await;
                return Err(anyhow::Error::new(error));
            }
        };
        first = false;
        let result = match operation.done().await {
            Ok(result) => result,
            Err(error) => {
                shells.reset(owner, "persistent bash send failed").await;
                return Err(anyhow::Error::new(error));
            }
        };
        let incremental = operation.read_output();
        if incremental.delta.is_empty() {
            fallback.clone_from(&result.viewport);
        } else {
            fallback.push_str(&incremental.delta);
        }
        fallback_truncated |= incremental.truncated || result.truncated;
        let latest = shells
            .terminals
            .read(
                owner,
                &id,
                seekdeep_terminal::TerminalReadRequest {
                    offset: Some(0.0),
                    count: Some(SCROLLBACK_PAGE_LINES),
                },
            )
            .map_err(anyhow::Error::new)?;
        if let Some(timed_out) = timeout_of(&deadline.signal, Some(TIMEOUT_CODE)) {
            let snapshot = retained_scrollback(&shells.terminals, owner, &id, &latest)?;
            let partial = render_captured(
                &partial_output(&snapshot, &marker, &fallback, fallback_truncated),
                config.max_output_chars,
            );
            shells
                .reset(owner, "persistent bash command timed out")
                .await;
            return Ok(format!(
                "Your command timed out after {} seconds or experienced an OOM error. Below is partial output:\n{partial}\n{SHELL_RESET_MESSAGE}",
                (timed_out.timeout_ms / 1_000.0).round()
            ));
        }
        if deadline.signal.is_aborted() {
            shells.reset(owner, "persistent bash command aborted").await;
            anyhow::bail!(
                "persistent bash command aborted: {}",
                deadline.signal.reason().unwrap_or(Value::Null)
            );
        }
        if latest.text.contains(&marker.end) {
            let complete = command_output(
                &retained_scrollback(&shells.terminals, owner, &id, &latest)?,
                &marker,
            );
            if let Some(complete) = complete {
                return Ok(render_captured(&complete, config.max_output_chars));
            }
        }
        if let TerminalSessionStatus::Exited { exit_code, signal } = &result.session_status {
            let snapshot = retained_scrollback(&shells.terminals, owner, &id, &latest)?;
            let partial = render_captured(
                &partial_output(&snapshot, &marker, &fallback, fallback_truncated),
                config.max_output_chars,
            );
            shells.reset(owner, "persistent bash shell exited").await;
            let signal_name = signal
                .iter()
                .next()
                .map(seekdeep_subprocess::ProcessSignal::as_str);
            let status = render_shell_exit(&partial, *exit_code, signal_name);
            return Ok(format!("{status}\n{SHELL_RESET_MESSAGE}"));
        }
        if prompt_completed(&result) {
            let snapshot = retained_scrollback(&shells.terminals, owner, &id, &latest)?;
            return Ok(render_captured(
                &partial_output(&snapshot, &marker, &fallback, fallback_truncated),
                config.max_output_chars,
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn definition(
    shells: Arc<PersistentShells>,
    config: ResolvedConfig,
) -> anyhow::Result<ToolDefinition> {
    let description = config.description.clone();
    define_tool(
        DefineToolOptions::new(
            "bash",
            description,
            serde_json::json!({
                "command": {
                    "type": "string",
                    "required": true,
                    "description": "The bash command to run. Relative path is preferred in the command."
                }
            }),
            DefineToolOutput::new(
                serde_json::json!({ "type": "string" }),
                Arc::new(|_args: &BashArgs, value: &String| {
                    Ok(vec![ContentBlock::Text { text: value.clone() }])
                }),
            ),
            Arc::new(move |args: BashArgs, run| {
                let shells = Arc::clone(&shells);
                let config = config.clone();
                Box::pin(async move {
                    anyhow::ensure!(
                        !args.command.trim().is_empty(),
                        "command must be a non-empty string"
                    );
                    let owner = run.execution().agent.clone().ok_or_else(|| {
                        anyhow::anyhow!("bash requires an owning agent session")
                    })?;
                    let lock = shells.lock_for(&owner);
                    let _guard = lock.lock().await;
                    if run.signal().is_aborted() {
                        anyhow::bail!("persistent bash command aborted before execution");
                    }
                    execute_command(&shells, &owner, &args.command, &config, &run.signal()).await
                })
            }),
        )
        .present_call(Arc::new(|args: &BashArgs| {
            Some(ToolCallView::Terminal(TerminalCallView {
                title: args.command.clone(),
                description: None,
                cwd: None,
            }))
        })),
    )
}

/// Registers the persistent `bash` tool and its shell lifecycle.
///
/// # Errors
///
/// Returns invalid config, missing-service, schema, registration, or ownership failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    let config = resolve_config(config)?;
    let terminals = context
        .get(TERMINALS)
        .ok_or_else(|| anyhow::anyhow!("tool-bash-persistent requires terminals"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-bash-persistent requires tools"))?;
    let shells = PersistentShells::install(context, terminals, config.backend_type.clone())?;
    tools.register(context, definition(shells, config)?)?;
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let mut config = if value.is_null() {
        Config::default()
    } else {
        serde_json::from_value::<Config>(value.clone())?
    };
    config
        .backend_type
        .get_or_insert_with(|| "shell".to_owned());
    config.timeout_ms.get_or_insert(300_000);
    config.max_output_chars.get_or_insert(16_000);
    config
        .description
        .get_or_insert_with(|| DEFAULT_DESCRIPTION.to_owned());
    resolve_config(config.clone())?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible persistent Bash plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            apply(&context, config)
        })
    })
    .with_config_validator(normalize_config)
}
