//! One-shot Claude Code startup, strict result folding, and quiescent teardown.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_subagent::{SubagentResult, SubagentRun, SubagentStartRequest, SubagentStopReason};
use seekdeep_subprocess::{SubprocessHandleRef, SubprocessService};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    sync::Notify,
};

use crate::process::{claude_spawn_spec, prompt_frame};

/// Default grace between managed subprocess termination tiers.
pub const DEFAULT_DISPOSE_GRACE_MS: f64 = 3_000.0;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// Post-publication failure observer.
pub type ClaudeCodeRunErrorObserver = Arc<dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync>;

/// Fully resolved inputs for one native Claude Code query.
pub struct ClaudeCodeRunSpec {
    /// Parent workspace.
    pub cwd: String,
    /// Resolved native executable.
    pub executable: String,
    /// Explicit environment layered after shared credential scrubbing.
    pub env: std::collections::BTreeMap<String, String>,
    /// Process-tree termination grace.
    pub dispose_grace_ms: f64,
    /// Shared subprocess capability.
    pub subprocess: Arc<SubprocessService>,
    /// Optional flattened-failure sink.
    pub on_error: Option<ClaudeCodeRunErrorObserver>,
}

/// Validates and concatenates one text-only task.
///
/// # Errors
///
/// Returns source-compatible empty, blank, or non-text diagnostics.
pub fn text_task(prompt: &[ContentBlock]) -> anyhow::Result<String> {
    if prompt.is_empty() {
        anyhow::bail!("subagent-claude-code: the one-shot task must contain only text blocks");
    }
    let mut task = String::new();
    let mut nonblank = false;
    for block in prompt {
        let ContentBlock::Text { text } = block else {
            anyhow::bail!("subagent-claude-code: the one-shot task must contain only text blocks");
        };
        nonblank |= !text.trim().is_empty();
        task.push_str(text);
    }
    anyhow::ensure!(
        nonblank,
        "subagent-claude-code: the one-shot task must not be empty"
    );
    Ok(task)
}

/// Requires one strict successful result message and returns its final text.
///
/// # Errors
///
/// Rejects every error subtype, error-marked success, or blank answer.
pub fn successful_result(message: &Value) -> anyhow::Result<String> {
    let subtype = message
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if subtype == "success" {
        let answer = message.get("result").and_then(Value::as_str).unwrap_or("");
        let is_error = message
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        anyhow::ensure!(
            !is_error && !answer.trim().is_empty(),
            "subagent-claude-code: Claude Code failed: success result was marked as an error or contained no answer"
        );
        return Ok(answer.to_owned());
    }
    let errors = message
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    let detail = if errors.is_empty() { subtype } else { &errors };
    anyhow::bail!("subagent-claude-code: Claude Code failed: {detail}")
}

#[derive(Default)]
struct ResultState {
    value: Mutex<Option<SubagentResult>>,
    notify: Notify,
}

impl ResultState {
    fn set(&self, value: SubagentResult) {
        *self.value.lock() = Some(value);
        self.notify.notify_waiters();
    }

    async fn get(&self) -> SubagentResult {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.value.lock().clone() {
                return value;
            }
            notified.await;
        }
    }
}

#[derive(Default)]
struct TeardownState {
    started: AtomicBool,
    result: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

struct ClaudeCodeRun {
    id: SessionId,
    child: SubprocessHandleRef,
    cancel: AbortSignal,
    result: Arc<ResultState>,
    teardown: Arc<TeardownState>,
}

impl SubagentRun for ClaudeCodeRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<seekdeep_agent::Agent>> {
        None
    }

    fn result(&self) -> futures::future::BoxFuture<'static, anyhow::Result<SubagentResult>> {
        let result = Arc::clone(&self.result);
        Box::pin(async move { Ok(result.get().await) })
    }

    fn dispose(&self) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        self.cancel
            .abort_with_reason(serde_json::json!("Claude Code run disposed"));
        if !self.teardown.started.swap(true, Ordering::AcqRel) {
            let teardown = Arc::clone(&self.teardown);
            let child = Arc::clone(&self.child);
            tokio::spawn(async move {
                *teardown.result.lock() = Some(
                    dispose_claude_code_child(&child)
                        .await
                        .map_err(|error| error.to_string()),
                );
                teardown.notify.notify_waiters();
            });
        }
        let teardown = Arc::clone(&self.teardown);
        let result = Arc::clone(&self.result);
        Box::pin(async move {
            loop {
                let notified = teardown.notify.notified();
                let outcome = { teardown.result.lock().clone() };
                if let Some(outcome) = outcome {
                    let _ = result.get().await;
                    return outcome.map_err(anyhow::Error::msg);
                }
                notified.await;
            }
        })
    }
}

/// Terminates the managed process tree and waits for its direct outcome.
///
/// # Errors
///
/// Returns one cleanup failure directly or aggregates multiple failures.
pub async fn dispose_claude_code_child(child: &SubprocessHandleRef) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    if child.pid().as_i64() > 0 {
        child.terminate();
        match child.wait_for_exit(None).await {
            Ok(true) => {}
            Ok(false) => failures.push("managed Claude Code tree did not exit".to_owned()),
            Err(error) => failures.push(error.to_string()),
        }
    }
    if let Err(error) = child.done().await {
        failures.push(error.to_string());
    }
    match failures.len() {
        0 => Ok(()),
        1 => Err(anyhow::anyhow!(failures.remove(0))),
        _ => Err(anyhow::anyhow!(
            "subagent-claude-code: query and process cleanup failed: {}",
            failures.join("; ")
        )),
    }
}

/// Starts and publishes one Claude Code run after acquiring its managed process.
///
/// # Errors
///
/// Returns task, pre-abort, spawn, pipe, write, or rollback failures.
#[allow(clippy::too_many_lines)]
pub async fn start_claude_code_run(
    request: SubagentStartRequest,
    spec: ClaudeCodeRunSpec,
) -> anyhow::Result<Arc<dyn SubagentRun>> {
    let task = text_task(&request.prompt)?;
    anyhow::ensure!(
        !request.signal.is_aborted(),
        "subagent-claude-code: request was aborted before SDK startup"
    );
    let cancel = AbortSignal::default();
    let spawn = claude_spawn_spec(
        &spec.executable,
        &spec.cwd,
        &spec.env,
        spec.dispose_grace_ms,
        cancel.clone(),
        if cfg!(windows) { "win32" } else { "other" },
    )?;
    let child = spec.subprocess.spawn(spawn)?;
    let stdin = child.stdin().ok_or_else(|| {
        anyhow::anyhow!("subagent-claude-code: managed process dropped piped stdin")
    });
    let stdout = child.stdout().ok_or_else(|| {
        anyhow::anyhow!("subagent-claude-code: managed process dropped piped stdout")
    });
    let (stdin, stdout) = match (stdin, stdout) {
        (Ok(stdin), Ok(stdout)) => (stdin, stdout),
        (Err(error), _) | (_, Err(error)) => {
            cancel.abort_with_reason(serde_json::json!("startup pipe failure"));
            let cleanup = dispose_claude_code_child(&child).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "subagent-claude-code: startup failed and CLI cleanup also failed: {error}; {cleanup}"
                )),
            };
        }
    };
    let Some(mut output) = stdin.take_writer().await else {
        cancel.abort_with_reason(serde_json::json!("startup pipe claimed"));
        let _ = dispose_claude_code_child(&child).await;
        anyhow::bail!("subagent-claude-code: managed process stdin was already claimed");
    };
    let input = stdout.take_reader().await;
    let mut frame = serde_json::to_vec(&prompt_frame(&task))?;
    frame.push(b'\n');
    if let Err(error) = async {
        output.write_all(&frame).await?;
        output.shutdown().await
    }
    .await
    {
        cancel.abort_with_reason(serde_json::json!("startup write failed"));
        let cleanup = dispose_claude_code_child(&child).await;
        return match cleanup {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!(
                "subagent-claude-code: startup failed and CLI cleanup also failed: {error}; {cleanup}"
            )),
        };
    }
    if child.pid().as_i64() <= 0 || request.signal.is_aborted() {
        cancel.abort_with_reason(serde_json::json!("startup cancelled or failed"));
        let cleanup = dispose_claude_code_child(&child).await;
        if request.signal.is_aborted() {
            anyhow::bail!("subagent-claude-code: request was aborted before SDK startup");
        }
        return match cleanup {
            Ok(()) => Err(anyhow::anyhow!(
                "subagent-claude-code: official SDK did not publish a controllable Claude Code process"
            )),
            Err(cleanup) => Err(anyhow::anyhow!(
                "subagent-claude-code: startup failed and CLI cleanup also failed: {cleanup}"
            )),
        };
    }
    let result = Arc::new(ResultState::default());
    let run = Arc::new(ClaudeCodeRun {
        id: SessionId::new(format!(
            "claude-code-{:016x}",
            NEXT_RUN.fetch_add(1, Ordering::AcqRel)
        )),
        child: Arc::clone(&child),
        cancel: cancel.clone(),
        result: Arc::clone(&result),
        teardown: Arc::new(TeardownState::default()),
    });
    let result_state = Arc::clone(&result);
    let result_cancel = cancel.clone();
    let result_child = Arc::clone(&child);
    let on_error = spec.on_error;
    tokio::spawn(async move {
        let read = async move {
            let mut lines = BufReader::new(input).lines();
            let mut answer = None;
            while let Some(line) = lines.next_line().await? {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("type").and_then(Value::as_str) == Some("result") {
                    answer = Some(successful_result(&message)?);
                }
            }
            let outcome = result_child.done().await?;
            anyhow::ensure!(
                outcome.exit_code == Some(0) && outcome.signal.is_none(),
                "subagent-claude-code: Claude Code process exited unsuccessfully: {outcome:?}"
            );
            answer.ok_or_else(|| {
                anyhow::anyhow!("subagent-claude-code: Claude Code ended without a result")
            })
        };
        let attempt = tokio::select! {
            biased;
            () = result_cancel.cancelled() => Err(anyhow::anyhow!("subagent-claude-code: run cancelled locally")),
            result = read => result,
        };
        let settled = if result_cancel.is_aborted() {
            SubagentResult {
                output: Vec::new(),
                structured: None,
                stop_reason: SubagentStopReason::Aborted,
            }
        } else {
            match attempt {
                Ok(answer) => SubagentResult {
                    output: vec![ContentBlock::Text { text: answer }],
                    structured: None,
                    stop_reason: SubagentStopReason::Completed,
                },
                Err(error) => {
                    if let Some(observer) = on_error {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            observer(&error, SubagentStopReason::Error);
                        }));
                    }
                    SubagentResult {
                        output: Vec::new(),
                        structured: None,
                        stop_reason: SubagentStopReason::Error,
                    }
                }
            }
        };
        result_state.set(settled);
    });
    let request_cancel = cancel.clone();
    let result_for_cancel = Arc::clone(&result);
    tokio::spawn(async move {
        tokio::select! {
            () = request.signal.cancelled() => request_cancel.abort_with_reason(serde_json::json!("Claude Code request cancelled")),
            _ = result_for_cancel.get() => {}
        }
    });
    Ok(run)
}
