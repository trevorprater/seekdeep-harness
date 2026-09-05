//! Fresh-process ACP client startup, result folding, cancellation, and teardown.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_acp::{
    AcpClient, AcpSessionUpdate, PermissionPolicy, acp_content_text, acp_stop_reason, to_acp_prompt,
};
use seekdeep_core::session::SessionId;
use seekdeep_llm::AbortSignal;
use seekdeep_subagent::{
    AssistantOutputFold, SubagentResult, SubagentRun, SubagentStartRequest, SubagentStopReason,
};
use seekdeep_subprocess::{
    SubprocessEnvironment, SubprocessHandleRef, SubprocessOutputMode, SubprocessService,
    SubprocessSpawnSpec, SubprocessStdinMode, SubprocessStdio,
};
use tokio::sync::Notify;

/// Child EOF flush window.
pub const DEFAULT_DISPOSE_EOF_GRACE_MS: f64 = 6_000.0;
/// Managed termination escalation grace.
pub const DEFAULT_DISPOSE_GRACE_MS: f64 = 3_000.0;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// Flattened post-publication error observer.
pub type AcpRunErrorObserver = Arc<dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync>;

/// Fully resolved child launch and policy inputs.
pub struct AcpRunSpec {
    /// Executable.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Absolute process and remote-session workspace.
    pub cwd: String,
    /// Automatic child permission policy.
    pub permission: PermissionPolicy,
    /// Explicit environment after shared scrub.
    pub env: std::collections::BTreeMap<String, String>,
    /// Cooperative EOF grace.
    pub dispose_eof_grace_ms: f64,
    /// Process termination grace.
    pub dispose_grace_ms: f64,
    /// Shared subprocess capability.
    pub subprocess: Arc<SubprocessService>,
    /// Optional flattened-failure sink.
    pub on_error: Option<AcpRunErrorObserver>,
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

struct AcpRun {
    id: SessionId,
    client: Arc<AcpClient>,
    child: SubprocessHandleRef,
    remote_session: seekdeep_acp::AcpSessionId,
    cancel: AbortSignal,
    result: Arc<ResultState>,
    teardown: Arc<TeardownState>,
    eof_grace_ms: f64,
}

impl SubagentRun for AcpRun {
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
            .abort_with_reason(serde_json::json!("ACP run disposed"));
        let client = Arc::clone(&self.client);
        let remote_session = self.remote_session.clone();
        tokio::spawn(async move {
            let _ = client.cancel(&remote_session).await;
        });
        if !self.teardown.started.swap(true, Ordering::AcqRel) {
            let teardown = Arc::clone(&self.teardown);
            let client = Arc::clone(&self.client);
            let child = Arc::clone(&self.child);
            let grace = self.eof_grace_ms;
            tokio::spawn(async move {
                *teardown.result.lock() = Some(
                    dispose_acp_child(&client, &child, grace)
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

/// Cooperatively closes ACP, then escalates managed process termination.
///
/// # Errors
///
/// Returns output shutdown, tree wait, or direct-process failures.
pub async fn dispose_acp_child(
    client: &AcpClient,
    child: &SubprocessHandleRef,
    eof_grace_ms: f64,
) -> anyhow::Result<()> {
    if child.pid().as_i64() <= 0 {
        let _ = child.done().await;
        return Ok(());
    }
    let _ = client.shutdown_output().await;
    let exited = tokio::select! {
        result = child.wait_for_exit(None) => result?,
        () = tokio::time::sleep(timer_duration(eof_grace_ms)) => false,
    };
    if !exited {
        child.terminate();
        anyhow::ensure!(
            child.wait_for_exit(None).await?,
            "subagent-acp: managed child tree did not exit"
        );
    }
    let _ = child.done().await?;
    Ok(())
}

/// Starts and publishes one child only after initialize and session/new complete.
///
/// # Errors
///
/// Returns pre-abort, spawn, pipe, handshake, session, cancellation, or rollback failures.
#[allow(clippy::too_many_lines)]
pub async fn start_acp_run(
    request: SubagentStartRequest,
    spec: AcpRunSpec,
) -> anyhow::Result<Arc<dyn SubagentRun>> {
    anyhow::ensure!(
        !request.signal.is_aborted(),
        "subagent request was aborted before the ACP child started"
    );
    let cancel = AbortSignal::default();
    let environment = spec
        .env
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect::<SubprocessEnvironment>();
    let child = spec.subprocess.spawn(SubprocessSpawnSpec {
        argv: std::iter::once(spec.command).chain(spec.args).collect(),
        cwd: spec.cwd.clone().into(),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Pipe,
            stdout: SubprocessOutputMode::Pipe,
            stderr: SubprocessOutputMode::Inherit,
        },
        grace_ms: spec.dispose_grace_ms,
        signal: None,
        env: Some(environment),
    })?;
    let prepared = prepare_client(&child, spec.permission).await;
    let client = match prepared {
        Ok(client) => client,
        Err(error) => {
            cancel.abort_with_reason(serde_json::json!("ACP startup pipe failure"));
            child.terminate();
            let _ = child.wait_for_exit(None).await;
            let _ = child.done().await;
            return Err(error);
        }
    };
    let fold = Arc::new(Mutex::new(AssistantOutputFold::default()));
    let observed = Arc::clone(&fold);
    client.on_update(Arc::new(move |update: &AcpSessionUpdate| {
        if update
            .update
            .get("sessionUpdate")
            .and_then(serde_json::Value::as_str)
            == Some("agent_message_chunk")
            && let Some(content) = update.update.get("content")
        {
            observed.lock().push_text(acp_content_text(content));
        }
    }));
    client.start();
    let startup = tokio::select! {
        result = async {
            client.initialize().await?;
            client.new_session(&spec.cwd).await
        } => result,
        () = request.signal.cancelled() => Err(anyhow::anyhow!("subagent cancelled before the ACP session started")),
        outcome = child.done() => Err(outcome.err().unwrap_or_else(|| anyhow::anyhow!("ACP child exited before startup"))),
    };
    let remote_session = match startup {
        Ok(session) => {
            if request.signal.is_aborted() {
                cancel.abort_with_reason(serde_json::json!("ACP startup cancelled"));
                let _ = dispose_acp_child(&client, &child, spec.dispose_eof_grace_ms).await;
                anyhow::bail!("subagent request was aborted before the ACP child started");
            }
            session
        }
        Err(error) => {
            if request.signal.is_aborted() {
                cancel.abort_with_reason(serde_json::json!("ACP startup cancelled"));
                let _ = dispose_acp_child(&client, &child, spec.dispose_eof_grace_ms).await;
                anyhow::bail!("subagent request was aborted before the ACP child started");
            }
            cancel.abort_with_reason(serde_json::json!("ACP startup failed"));
            let cleanup = dispose_acp_child(&client, &child, spec.dispose_eof_grace_ms).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "subagent-acp: startup failed and cleanup also failed: {error}; {cleanup}"
                )),
            };
        }
    };
    let result = Arc::new(ResultState::default());
    let run = Arc::new(AcpRun {
        id: SessionId::new(format!(
            "acp-run-{:016x}",
            NEXT_RUN.fetch_add(1, Ordering::AcqRel)
        )),
        client: Arc::clone(&client),
        child: Arc::clone(&child),
        remote_session: remote_session.clone(),
        cancel: cancel.clone(),
        result: Arc::clone(&result),
        teardown: Arc::new(TeardownState::default()),
        eof_grace_ms: spec.dispose_eof_grace_ms,
    });
    let prompt_client = Arc::clone(&client);
    let prompt_session = remote_session.clone();
    let prompt = to_acp_prompt(&request.prompt);
    let result_fold = Arc::clone(&fold);
    let result_state = Arc::clone(&result);
    let result_cancel = cancel.clone();
    let on_error = spec.on_error;
    tokio::spawn(async move {
        let attempt = tokio::select! {
            biased;
            () = result_cancel.cancelled() => Err(anyhow::anyhow!("ACP run cancelled")),
            result = prompt_client.prompt(&prompt_session, prompt) => result,
        };
        let output = result_fold.lock().collect().unwrap_or_default();
        let settled = if result_cancel.is_aborted() {
            SubagentResult {
                output,
                structured: None,
                stop_reason: SubagentStopReason::Aborted,
            }
        } else {
            match attempt {
                Ok(reason) => SubagentResult {
                    output,
                    structured: None,
                    stop_reason: acp_stop_reason(&reason),
                },
                Err(error) => {
                    if let Some(observer) = on_error {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            observer(&error, SubagentStopReason::Error);
                        }));
                    }
                    SubagentResult {
                        output,
                        structured: None,
                        stop_reason: SubagentStopReason::Error,
                    }
                }
            }
        };
        result_state.set(settled);
    });
    let request_cancel = cancel.clone();
    let cancel_client = Arc::clone(&client);
    let cancel_session = remote_session;
    let cancel_result = Arc::clone(&result);
    tokio::spawn(async move {
        tokio::select! {
            () = request.signal.cancelled() => {
                request_cancel.abort_with_reason(serde_json::json!("ACP request cancelled"));
                let _ = cancel_client.cancel(&cancel_session).await;
            }
            _ = cancel_result.get() => {}
        }
    });
    Ok(run)
}

async fn prepare_client(
    child: &SubprocessHandleRef,
    permission: PermissionPolicy,
) -> anyhow::Result<Arc<AcpClient>> {
    let stdin = child.stdin().ok_or_else(|| {
        anyhow::anyhow!("subagent-acp: subprocess implementation dropped piped stdin")
    })?;
    let stdout = child.stdout().ok_or_else(|| {
        anyhow::anyhow!("subagent-acp: subprocess implementation dropped piped stdout")
    })?;
    let output = stdin
        .take_writer()
        .await
        .ok_or_else(|| anyhow::anyhow!("subagent-acp: subprocess stdin was already claimed"))?;
    let input = stdout.take_reader().await;
    Ok(AcpClient::from_boxed(input, output, permission))
}

fn timer_duration(milliseconds: f64) -> std::time::Duration {
    std::time::Duration::from_secs_f64(milliseconds / 1_000.0)
}
