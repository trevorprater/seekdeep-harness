//! Codex child startup, result settlement, cancellation, and process-tree teardown.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_subagent::{SubagentResult, SubagentRun, SubagentStartRequest, SubagentStopReason};
use seekdeep_subprocess::{
    SubprocessEnvironment, SubprocessHandleRef, SubprocessOutputMode, SubprocessService,
    SubprocessSpawnSpec, SubprocessStdinMode, SubprocessStdio,
};
use serde_json::json;
use tokio::sync::Notify;

use crate::wire::CodexAppServerWire;

/// Default grace between subprocess termination tiers.
pub const DEFAULT_DISPOSE_GRACE_MS: f64 = 3_000.0;
static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

/// Post-publication failure observer.
pub type CodexRunErrorObserver = Arc<dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync>;

/// Fully resolved inputs for one app-server run.
pub struct CodexRunSpec {
    /// Parent workspace, also supplied to `thread/start`.
    pub cwd: String,
    /// Explicit environment after credential scrubbing.
    pub env: SubprocessEnvironment,
    /// Managed process-tree termination grace.
    pub dispose_grace_ms: f64,
    /// Shared subprocess capability implementation.
    pub subprocess: Arc<SubprocessService>,
    /// Optional diagnostic sink for flattened post-publication failures.
    pub on_error: Option<CodexRunErrorObserver>,
}

/// Resolves the fixed app-server argv for one platform spelling.
#[must_use]
pub fn codex_app_server_argv(platform: &str) -> Vec<String> {
    if matches!(platform, "win32" | "windows") {
        [
            "cmd.exe",
            "/d",
            "/s",
            "/c",
            "codex",
            "app-server",
            "--stdio",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else {
        ["codex", "app-server", "--stdio"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

/// Validates and preserves a nonempty text-only task.
///
/// # Errors
///
/// Returns the source-compatible text-only or empty-task failure.
pub fn text_task(prompt: &[ContentBlock]) -> anyhow::Result<Vec<String>> {
    if prompt.is_empty() {
        anyhow::bail!("subagent-codex: the one-shot task must contain only text blocks");
    }
    let mut texts = Vec::with_capacity(prompt.len());
    for block in prompt {
        let ContentBlock::Text { text } = block else {
            anyhow::bail!("subagent-codex: the one-shot task must contain only text blocks");
        };
        texts.push(text.clone());
    }
    anyhow::ensure!(
        texts.iter().any(|text| !text.trim().is_empty()),
        "subagent-codex: the one-shot task must not be empty"
    );
    Ok(texts)
}

/// Closes the wire, terminates the managed process tree, and proves quiescence.
///
/// # Errors
///
/// Returns provider liveness, tree-exit, or direct-process failures. A spawn-level
/// pid sentinel has no process tree and contains its `done` rejection.
pub async fn dispose_codex_child(
    wire: &CodexAppServerWire,
    child: &SubprocessHandleRef,
) -> anyhow::Result<()> {
    wire.close();
    let _ = wire.close_input().await;
    if child.pid().as_i64() <= 0 {
        let _ = child.done().await;
        return Ok(());
    }
    child.terminate();
    anyhow::ensure!(
        child.wait_for_exit(None).await?,
        "subagent-codex: managed app-server tree did not exit"
    );
    child.done().await?;
    Ok(())
}

#[derive(Default)]
struct ResultState {
    value: Mutex<Option<SubagentResult>>,
    notify: Notify,
}

impl ResultState {
    fn set(&self, result: SubagentResult) {
        *self.value.lock() = Some(result);
        self.notify.notify_waiters();
    }

    async fn get(&self) -> SubagentResult {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.value.lock().clone() {
                return result;
            }
            notified.await;
        }
    }
}

#[derive(Default)]
struct TeardownState {
    started: AtomicBool,
    outcome: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

impl TeardownState {
    fn set(&self, outcome: anyhow::Result<()>) {
        *self.outcome.lock() = Some(outcome.map_err(|error| error.to_string()));
        self.notify.notify_waiters();
    }

    async fn get(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }
}

struct CodexRun {
    id: SessionId,
    wire: Arc<CodexAppServerWire>,
    child: SubprocessHandleRef,
    cancel: AbortSignal,
    result: Arc<ResultState>,
    teardown: Arc<TeardownState>,
}

impl CodexRun {
    fn cancel_locally(&self) {
        if !self.cancel.is_aborted() {
            self.cancel
                .abort_with_reason(json!("subagent-codex: run cancelled locally"));
            self.wire.interrupt();
        }
    }
}

impl SubagentRun for CodexRun {
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
        self.cancel_locally();
        if !self.teardown.started.swap(true, Ordering::AcqRel) {
            let teardown = Arc::clone(&self.teardown);
            let wire = Arc::clone(&self.wire);
            let child = Arc::clone(&self.child);
            tokio::spawn(async move {
                teardown.set(dispose_codex_child(&wire, &child).await);
            });
        }
        let teardown = Arc::clone(&self.teardown);
        let result = Arc::clone(&self.result);
        Box::pin(async move {
            let outcome = teardown.get().await;
            let _ = result.get().await;
            outcome
        })
    }
}

/// Starts the app-server and publishes only after initialization and ephemeral thread creation.
///
/// # Errors
///
/// Returns task validation, pre-publication cancellation, spawn, protocol, or cleanup failures.
pub async fn start_codex_run(
    request: SubagentStartRequest,
    spec: CodexRunSpec,
) -> anyhow::Result<Arc<dyn SubagentRun>> {
    let texts = text_task(&request.prompt)?;
    anyhow::ensure!(
        !request.signal.is_aborted(),
        "subagent-codex: request was aborted before app-server startup"
    );
    let child = spawn_codex_child(&spec)?;
    let wire = prepare_wire_or_cleanup(&child).await?;
    wire.start();
    let startup = async {
        race_process(wire.initialize(request.signal.clone()), Arc::clone(&child)).await?;
        race_process(
            wire.start_thread(&spec.cwd, request.signal.clone()),
            Arc::clone(&child),
        )
        .await?;
        anyhow::ensure!(
            !request.signal.is_aborted(),
            "subagent-codex: request was aborted before run publication"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = startup {
        let cleanup = dispose_codex_child(&wire, &child).await;
        return match cleanup {
            Ok(()) if request.signal.is_aborted() => Err(anyhow::anyhow!(
                "subagent-codex: request was aborted before run publication"
            )),
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!(
                "subagent-codex: startup failed and app-server cleanup also failed: {error:#}; {cleanup:#}"
            )),
        };
    }

    let cancel = AbortSignal::default();
    let result = Arc::new(ResultState::default());
    let run = Arc::new(CodexRun {
        id: SessionId::new(format!(
            "codex-{:016x}",
            NEXT_RUN_ID.fetch_add(1, Ordering::AcqRel)
        )),
        wire: Arc::clone(&wire),
        child: Arc::clone(&child),
        cancel: cancel.clone(),
        result: Arc::clone(&result),
        teardown: Arc::new(TeardownState::default()),
    });

    let result_for_cancel = Arc::clone(&result);
    let signal = request.signal.clone();
    let cancel_for_request = cancel.clone();
    let wire_for_request = Arc::clone(&wire);
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {
                cancel_for_request.abort_with_reason(json!("subagent-codex: run cancelled locally"));
                wire_for_request.interrupt();
            }
            _ = result_for_cancel.get() => {}
        }
    });

    let result_state = Arc::clone(&result);
    let child_for_result = Arc::clone(&child);
    let wire_for_result = Arc::clone(&wire);
    let on_error = spec.on_error;
    tokio::spawn(async move {
        let attempt = tokio::select! {
            result = wire_for_result.run_turn(&texts, cancel.clone()) => result,
            process = child_for_result.done() => Err(process_failure(process)),
        };
        let settled = if cancel.is_aborted() {
            SubagentResult {
                output: wire_for_result.collect_output(),
                structured: None,
                stop_reason: SubagentStopReason::Aborted,
            }
        } else {
            match attempt {
                Ok(result) => result,
                Err(error) => {
                    if let Some(observer) = on_error {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            observer(&error, SubagentStopReason::Error);
                        }));
                    }
                    SubagentResult {
                        output: wire_for_result.collect_output(),
                        structured: None,
                        stop_reason: SubagentStopReason::Error,
                    }
                }
            }
        };
        result_state.set(settled);
    });

    Ok(run)
}

fn spawn_codex_child(spec: &CodexRunSpec) -> anyhow::Result<SubprocessHandleRef> {
    spec.subprocess.spawn(SubprocessSpawnSpec {
        argv: codex_app_server_argv(if cfg!(windows) { "win32" } else { "other" }),
        cwd: spec.cwd.clone().into(),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Pipe,
            stdout: SubprocessOutputMode::Pipe,
            stderr: SubprocessOutputMode::Inherit,
        },
        grace_ms: spec.dispose_grace_ms,
        signal: None,
        env: Some(spec.env.clone()),
    })
}

async fn prepare_wire_or_cleanup(
    child: &SubprocessHandleRef,
) -> anyhow::Result<Arc<CodexAppServerWire>> {
    match prepare_wire(child).await {
        Ok(wire) => Ok(wire),
        Err(error) => {
            child.terminate();
            let _ = child.wait_for_exit(None).await;
            let _ = child.done().await;
            Err(error)
        }
    }
}

async fn prepare_wire(child: &SubprocessHandleRef) -> anyhow::Result<Arc<CodexAppServerWire>> {
    let stdout = child.stdout().ok_or_else(|| {
        anyhow::anyhow!("subagent-codex: subprocess implementation dropped piped stdout")
    })?;
    let stdin = child.stdin().ok_or_else(|| {
        anyhow::anyhow!("subagent-codex: subprocess implementation dropped piped stdin")
    })?;
    let input = stdout.take_reader().await;
    let output = stdin
        .take_writer()
        .await
        .ok_or_else(|| anyhow::anyhow!("subagent-codex: subprocess stdin was already claimed"))?;
    Ok(CodexAppServerWire::new(input, output))
}

async fn race_process<T>(
    operation: impl std::future::Future<Output = anyhow::Result<T>>,
    child: SubprocessHandleRef,
) -> anyhow::Result<T> {
    tokio::select! {
        result = operation => result,
        process = child.done() => Err(process_failure(process)),
    }
}

fn process_failure(
    outcome: anyhow::Result<seekdeep_subprocess::SubprocessOutcome>,
) -> anyhow::Error {
    match outcome {
        Err(error) => error,
        Ok(outcome) => anyhow::anyhow!(
            "subagent-codex: app-server exited before the run settled (code {}, signal {})",
            outcome
                .exit_code
                .map_or("null".to_owned(), |code| code.to_string()),
            outcome
                .signal
                .as_ref()
                .map_or("null", seekdeep_subprocess::ProcessSignal::as_str)
        ),
    }
}
