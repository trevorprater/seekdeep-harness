//! One SDK-runtime child lifecycle and canonical stop-reason mapping.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};
use seekdeep_subagent::{
    AssistantOutputFold, SubagentResult, SubagentRun, SubagentStartRequest, SubagentStopReason,
};
use tokio::sync::Notify;

/// Child EOF quiescence grace.
pub const DEFAULT_DISPOSE_EOF_GRACE_MS: f64 = 6_000.0;
/// Child termination confirmation grace.
pub const DEFAULT_DISPOSE_GRACE_MS: f64 = 3_000.0;
/// Protocol shutdown bound.
pub const DEFAULT_SHUTDOWN_TIMEOUT_MS: f64 = 1_000.0;
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
static NEXT_CHILD_SESSION: AtomicU64 = AtomicU64::new(1);

/// Flattened child-failure observer.
pub type SdkRunErrorObserver = Arc<dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync>;

/// Fully resolved child runtime launch and route specification.
pub struct SdkRunSpec {
    /// Runtime executable.
    pub command: String,
    /// Runtime args.
    pub args: Vec<String>,
    /// Absolute process and workspace cwd.
    pub cwd: String,
    /// Child provider route.
    pub provider: String,
    /// Child model.
    pub model: String,
    /// Optional output-token cap.
    pub max_tokens: Option<u64>,
    /// Complete scrubbed-plus-explicit environment.
    pub env: std::collections::BTreeMap<String, String>,
    /// Protocol shutdown bound.
    pub shutdown_timeout_ms: f64,
    /// EOF grace.
    pub dispose_eof_grace_ms: f64,
    /// Termination grace.
    pub dispose_grace_ms: f64,
    /// Optional flattened-failure sink.
    pub on_error: Option<SdkRunErrorObserver>,
}

/// Maps a child's last durable turn reason to the shared subagent reason.
#[must_use]
pub fn sdk_stop_reason(reason: Option<&serde_json::Value>) -> SubagentStopReason {
    match reason
        .and_then(|reason| reason.get("kind"))
        .and_then(serde_json::Value::as_str)
    {
        Some("completed") => SubagentStopReason::Completed,
        Some("max-tokens") => SubagentStopReason::MaxTokens,
        Some("aborted") => SubagentStopReason::Aborted,
        None | Some(_) => SubagentStopReason::Error,
    }
}

#[derive(Default)]
struct ResultState {
    result: Mutex<Option<SubagentResult>>,
    notify: Notify,
}

impl ResultState {
    fn set(&self, result: SubagentResult) {
        *self.result.lock() = Some(result);
        self.notify.notify_waiters();
    }

    async fn get(&self) -> SubagentResult {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.result.lock().clone() {
                return result;
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

struct SdkRun {
    id: SessionId,
    harness: Arc<DeepSeekHarness>,
    cancel: AbortSignal,
    result: Arc<ResultState>,
    teardown: Arc<TeardownState>,
}

impl SubagentRun for SdkRun {
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
            .abort_with_reason(serde_json::json!("subagent SDK run disposed"));
        if !self.teardown.started.swap(true, Ordering::AcqRel) {
            let teardown = Arc::clone(&self.teardown);
            let harness = Arc::clone(&self.harness);
            tokio::spawn(async move {
                *teardown.result.lock() =
                    Some(harness.close().await.map_err(|error| error.to_string()));
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

/// Starts and publishes one SDK child after initialize succeeds.
///
/// # Errors
///
/// Returns pre-publication cancellation, launch, handshake, or rollback failures.
#[allow(clippy::too_many_lines)]
pub async fn start_sdk_run(
    request: SubagentStartRequest,
    spec: SdkRunSpec,
) -> anyhow::Result<Arc<dyn SubagentRun>> {
    anyhow::ensure!(
        !request.signal.is_aborted(),
        "subagent request was aborted before the SDK child started"
    );
    let mut launch = HarnessClientOptions::new(spec.command);
    launch.args = spec.args;
    launch.cwd = Some(spec.cwd.clone());
    launch.env = Some(spec.env);
    launch.shutdown_timeout_ms = spec.shutdown_timeout_ms;
    launch.dispose_eof_grace_ms = spec.dispose_eof_grace_ms;
    launch.dispose_grace_ms = spec.dispose_grace_ms;
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch,
        cwd: Some(spec.cwd),
        provider: Some(spec.provider),
        model: Some(spec.model),
        max_tokens: spec.max_tokens,
    })?;
    let startup = tokio::select! {
        result = harness.start() => result,
        () = request.signal.cancelled() => Err(anyhow::anyhow!("subagent cancelled before the SDK child initialized")),
    };
    if let Err(error) = startup {
        let _ = harness.close().await;
        return if request.signal.is_aborted() {
            Err(anyhow::anyhow!(
                "subagent request was aborted before the SDK child started"
            ))
        } else {
            Err(error)
        };
    }
    if request.signal.is_aborted() {
        let _ = harness.close().await;
        anyhow::bail!("subagent request was aborted before the SDK child started");
    }
    let child_session = SessionId::new(format!(
        "session-{:016x}",
        NEXT_CHILD_SESSION.fetch_add(1, Ordering::AcqRel)
    ));
    let cancel = AbortSignal::default();
    let result = Arc::new(ResultState::default());
    let run = Arc::new(SdkRun {
        id: SessionId::new(format!(
            "seekdeep-sdk-{:016x}",
            NEXT_RUN.fetch_add(1, Ordering::AcqRel)
        )),
        harness: Arc::clone(&harness),
        cancel: cancel.clone(),
        result: Arc::clone(&result),
        teardown: Arc::new(TeardownState::default()),
    });
    let fold = Arc::new(Mutex::new(AssistantOutputFold::default()));
    let observed_fold = Arc::clone(&fold);
    let observed_session = child_session.clone();
    let observer = Arc::new(
        move |notification: &seekdeep_sdk_client::HarnessNotification| {
            if notification.method != "session.event"
                || notification
                    .params
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    != Some(observed_session.as_str())
            {
                return;
            }
            if let Ok(event) = serde_json::from_value::<SessionEvent>(
                notification
                    .params
                    .get("event")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            ) {
                observed_fold.lock().push(&event);
            }
        },
    );
    let result_state = Arc::clone(&result);
    let harness_for_run = Arc::clone(&harness);
    let prompt = request.prompt;
    let on_error = spec.on_error;
    let result_cancel = cancel.clone();
    tokio::spawn(async move {
        let outcome = tokio::select! {
            run = harness_for_run.run(prompt, RunOptions { session_id: Some(child_session), on_notification: Some(observer) }) => run,
            () = result_cancel.cancelled() => Err(anyhow::anyhow!("subagent SDK run cancelled")),
        };
        let output: Vec<ContentBlock> = fold.lock().collect().unwrap_or_default();
        let settled = if result_cancel.is_aborted() {
            SubagentResult {
                output,
                structured: None,
                stop_reason: SubagentStopReason::Aborted,
            }
        } else {
            match outcome {
                Ok(run) => {
                    let reason = run
                        .events
                        .iter()
                        .rev()
                        .find(|event| event.event_type == "turn/end")
                        .and_then(|event| event.data.get("reason"));
                    SubagentResult {
                        output,
                        structured: None,
                        stop_reason: sdk_stop_reason(reason),
                    }
                }
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
    let result_for_cancel = Arc::clone(&result);
    tokio::spawn(async move {
        tokio::select! {
            () = request.signal.cancelled() => request_cancel.abort_with_reason(serde_json::json!("subagent SDK run cancelled")),
            _ = result_for_cancel.get() => {}
        }
    });
    Ok(run)
}
