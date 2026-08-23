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
    std::fs::metadata(path).is_ok_and(|meta| {
        if !meta.is_dir() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
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
        anyhow::bail!(
            "{prefix}: config cwd must not be empty — omit the key to inherit the parent session cwd"
        );
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
        anyhow::bail!(
            "{prefix}: no working directory for the child — configure cwd or delegate from a parent session that has one"
        );
    };
    assert_usable_cwd(prefix, "parent session cwd", parent_cwd)
}

/// A one-shot turn attempt returning the terminal result.
#[allow(clippy::type_complexity)]
type AttemptClosure =
    dyn FnOnce() -> futures::future::BoxFuture<'static, anyhow::Result<SubagentResult>> + Send;

/// A diagnostic sink for a failure flattened to a stop reason.
#[allow(clippy::type_complexity)]
type ErrorSink = dyn Fn(&anyhow::Error, SubagentStopReason) + Send + Sync;

/// Inputs to `settle_run_result`.
pub struct RunResultSettlement {
    /// The turn attempt returning the terminal result.
    pub attempt: Box<AttemptClosure>,
    /// Snapshot the provider exposes when cancellation wins settlement.
    pub collect_output: Box<dyn Fn() -> Vec<ContentBlock> + Send + Sync>,
    /// Whether local cancellation settled before the attempt's outcome is observed.
    pub cancelled: Box<dyn Fn() -> bool + Send + Sync>,
    /// Diagnostic sink for a failure flattened to a stop reason.
    pub on_error: Option<Box<ErrorSink>>,
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

    fn result(&self) -> futures::future::BoxFuture<'static, anyhow::Result<SubagentResult>> {
        let result = self.result.lock().clone();
        Box::pin(async move {
            Ok(result.unwrap_or(SubagentResult {
                output: Vec::new(),
                structured: None,
                stop_reason: SubagentStopReason::Error,
            }))
        })
    }

    fn dispose(&self) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
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
                return Ok(());
            }
            notify.notified().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn out_of_process_capabilities_and_positive_bounds_are_fail_closed() {
        assert_eq!(no_start_capabilities(), SubagentCapabilities::default());
        assert!(assert_positive_finite("p", "graceMs", 1.0).is_ok());
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(assert_positive_finite("p", "graceMs", value).is_err());
        }
    }

    #[test]
    fn cwd_resolution_requires_an_absolute_enterable_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().to_string_lossy().into_owned();
        assert_eq!(
            assert_usable_cwd("p", "config cwd", &directory).unwrap(),
            directory
        );
        assert!(assert_usable_cwd("p", "config cwd", "relative/path").is_err());
        let file = temporary.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(assert_usable_cwd("p", "config cwd", &file.to_string_lossy()).is_err());
        assert_eq!(validate_configured_cwd("p", None).unwrap(), None);
        assert!(validate_configured_cwd("p", Some("")).is_err());
        assert_eq!(
            resolve_child_cwd("p", Some(&directory), None).unwrap(),
            directory
        );
        assert!(resolve_child_cwd("p", None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_without_search_permission_is_not_usable() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = assert_usable_cwd("p", "config cwd", &temporary.path().to_string_lossy());
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn result_settlement_contains_failures_and_preserves_cancelled_partial_output() {
        let result = settle_run_result(RunResultSettlement {
            attempt: Box::new(|| Box::pin(async { anyhow::bail!("transport died") })),
            collect_output: Box::new(|| {
                vec![ContentBlock::Text {
                    text: "partial".to_owned(),
                }]
            }),
            cancelled: Box::new(|| true),
            on_error: None,
            signal: AbortSignal::default(),
            on_abort: Box::new(|| {}),
        })
        .await;
        assert_eq!(result.stop_reason, SubagentStopReason::Aborted);
        assert_eq!(
            result.output,
            [ContentBlock::Text {
                text: "partial".to_owned()
            }]
        );

        let errors = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&errors);
        let failed = settle_run_result(RunResultSettlement {
            attempt: Box::new(|| Box::pin(async { anyhow::bail!("transport died") })),
            collect_output: Box::new(Vec::new),
            cancelled: Box::new(|| false),
            on_error: Some(Box::new(move |_, reason| {
                assert_eq!(reason, SubagentStopReason::Error);
                observed.fetch_add(1, Ordering::SeqCst);
                panic!("sink failure is contained");
            })),
            signal: AbortSignal::default(),
            on_abort: Box::new(|| {}),
        })
        .await;
        assert_eq!(failed.stop_reason, SubagentStopReason::Error);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn subprocess_handle_result_and_disposal_are_idempotent() {
        let teardown = Arc::new(AtomicUsize::new(0));
        let disposed = Arc::clone(&teardown);
        let handle = SubprocessRunHandle::new(
            SessionId::new("remote"),
            Box::pin(async move {
                disposed.fetch_add(1, Ordering::SeqCst);
            }),
        );
        handle.set_result(SubagentResult {
            output: Vec::new(),
            structured: None,
            stop_reason: SubagentStopReason::Completed,
        });
        assert_eq!(
            handle.result().await.unwrap().stop_reason,
            SubagentStopReason::Completed
        );
        handle.dispose().await.unwrap();
        handle.dispose().await.unwrap();
        assert_eq!(teardown.load(Ordering::SeqCst), 1);
    }
}
