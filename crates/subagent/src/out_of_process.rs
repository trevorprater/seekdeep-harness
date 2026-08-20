//! Provider-side vocabulary for out-of-process subagent backends.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, ContentBlock};

use crate::types::{SubagentCapabilities, SubagentResult, SubagentRun, SubagentStopReason};

/// The capability advertisement of an out-of-process backend: none.
#[must_use]
pub fn no_start_capabilities() -> SubagentCapabilities {
    SubagentCapabilities {
        output_schema: false,
        depth_limit: false,
        tool_filter: false,
        persona: false,
    }
}

/// Asserts a configured timing bound is a positive finite number.
///
/// # Errors
///
/// Returns a non-positive or non-finite failure.
pub fn assert_positive_finite(prefix: &str, name: &str, value: f64) -> anyhow::Result<()> {
    if !value.is_finite() || value <= 0.0 {
        anyhow::bail!("{prefix}: {name} must be a positive finite number");
    }
    Ok(())
}

fn is_enterable_directory(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Asserts cwd can host the child: absolute and an existing directory.
///
/// # Errors
///
/// Returns a relative or non-enterable-directory failure.
pub fn assert_usable_cwd(prefix: &str, label: &str, cwd: &str) -> anyhow::Result<String> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        anyhow::bail!("{prefix}: {label} must be an absolute path: {cwd}");
    }
    if !is_enterable_directory(path) {
        anyhow::bail!("{prefix}: {label} is not an accessible directory: {cwd}");
    }
    Ok(cwd.to_owned())
}

/// Validates a configured cwd override once, at plugin load.
///
/// # Errors
///
/// Returns an empty, relative, or non-enterable-directory failure.
pub fn validate_configured_cwd(prefix: &str, cwd: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    if cwd.is_empty() {
        anyhow::bail!("{prefix}: config cwd must not be empty — omit the key to inherit the parent session cwd");
    }
    let resolved = if Path::new(cwd).is_absolute() {
        cwd.to_owned()
    } else {
        std::env::current_dir()?
            .join(cwd)
            .to_string_lossy()
            .into_owned()
    };
    assert_usable_cwd(prefix, "config cwd", &resolved).map(Some)
}

/// Resolves the child's working directory at start.
///
/// # Errors
///
/// Returns when neither the override nor the parent cwd exists.
pub fn resolve_child_cwd(
    prefix: &str,
    configured: Option<&str>,
    parent_cwd: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(configured) = configured {
        return Ok(configured.to_owned());
    }
    let Some(parent_cwd) = parent_cwd else {
        anyhow::bail!("{prefix}: no working directory for the child — configure cwd or delegate from a parent session that has one");
    };
    assert_usable_cwd(prefix, "parent session cwd", parent_cwd)
}

/// Inputs to settle_run_result.
pub struct RunResultSettlement {
    /// The turn attempt returning the terminal result.
    pub attempt: Box<
        dyn FnOnce() -> futures::future::BoxFuture<'static, anyhow::Result<SubagentResult>> + Send,
    >,
    /// Snapshot the provider exposes when cancellation wins settlement.
    pub collect_output: Box<dyn Fn() -> Vec<ContentBlock> + Send + Sync>,
    /// Whether local cancellation settled before the attempt's outcome is observed.
    pub cancelled: Box<dyn Fn() -> bool + Send + Sync>,
    /// Diagnostic sink for a failure flattened to a stop reason.
    pub on_error: Option<Box<dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync>>,
    /// The request's cancellation signal.
    pub signal: AbortSignal,
    /// The abort callback registered on the signal at start.
    pub on_abort: Box<dyn Fn() + Send + Sync>,
}

/// Settles an out-of-process run result under the never-reject contract.
pub async fn settle_run_result(parts: RunResultSettlement) -> SubagentResult {
    let _ = &parts.signal;
    let _ = &parts.on_abort;
    match (parts.attempt)().await {
        Ok(result) => {
            if (parts.cancelled)() {
                SubagentResult {
                    output: (parts.collect_output)(),
                    structured: None,
                    stop_reason: SubagentStopReason::Aborted,
                }
            } else {
                result
            }
        }
        Err(error) => {
            if (parts.cancelled)() {
                SubagentResult {
                    output: (parts.collect_output)(),
                    structured: None,
                    stop_reason: SubagentStopReason::Aborted,
                }
            } else {
                if let Some(on_error) = &parts.on_error {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        on_error(&error, SubagentStopReason::Error);
                    }));
                }
                SubagentResult {
                    output: (parts.collect_output)(),
                    structured: None,
                    stop_reason: SubagentStopReason::Error,
                }
            }
        }
    }
}

/// A remote-run handle with idempotent teardown.
pub struct SubprocessRunHandle {
    id: SessionId,
    result: Mutex<Option<SubagentResult>>,
    teardown: Mutex<Option<futures::future::BoxFuture<'static, ()>>>,
    started: AtomicBool,
    completed: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl SubprocessRunHandle {
    /// Publishes a remote-run handle.
    #[must_use]
    pub fn new(id: SessionId, teardown: futures::future::BoxFuture<'static, ()>) -> Arc<Self> {
        Arc::new(Self {
            id,
            result: Mutex::new(None),
            teardown: Mutex::new(Some(teardown)),
            started: AtomicBool::new(false),
            completed: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Records the already-settled result.
    pub fn set_result(&self, result: SubagentResult) {
        *self.result.lock() = Some(result);
    }
}

impl SubagentRun for SubprocessRunHandle {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<seekdeep_agent::Agent>> {
        None
    }

    fn result(&self) -> futures::future::BoxFuture<'static, SubagentResult> {
        let result = self.result.lock().clone();
        Box::pin(async move {
            result.unwrap_or(SubagentResult {
                output: Vec::new(),
                structured: None,
                stop_reason: SubagentStopReason::Error,
            })
        })
    }

    fn dispose(&self) -> futures::future::BoxFuture<'static, ()> {
        if !self.started.swap(true, Ordering::AcqRel) {
            let teardown = self.teardown.lock().take();
            let notify = self.notify.clone();
            let completed = self.completed.clone();
            tokio::spawn(async move {
                if let Some(teardown) = teardown {
                    teardown.await;
                }
                completed.store(true, Ordering::Release);
                notify.notify_waiters();
            });
        }
        let completed = self.completed.clone();
        let notify = self.notify.clone();
        Box::pin(async move {
            if completed.load(Ordering::Acquire) {
                return;
            }
            notify.notified().await;
        })
    }
}
