//! Generation-safe MCP connection supervision with injected time.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Logger};
use seekdeep_llm::AbortSignal;
use serde_json::Value;
use tokio::sync::{Notify, OnceCell};

use crate::{
    Config, ResolvedReconnectPolicy,
    protocol::{McpClient, McpClientFactory},
    tools::{
        RegistrationFailure, ToolBridgeOptions, ToolDisposers, dispose_generation, sync_tools,
    },
};

const GENERATION_CLOSE_TIMEOUT_MS: f64 = 5_000.0;
const GENERATION_CLOSE_TIMEOUT_MILLIS: u64 = 5_000;

/// Injectable monotonic clock and sleeper for retry and close policies.
#[async_trait]
pub trait McpTiming: std::fmt::Debug + Send + Sync {
    /// Monotonic milliseconds in an arbitrary process-local epoch.
    fn now_ms(&self) -> f64;
    /// Waits the exact requested policy duration.
    async fn sleep(&self, milliseconds: f64);
}

/// Native Tokio timing boundary.
#[derive(Debug)]
pub struct TokioMcpTiming {
    origin: Instant,
}

impl Default for TokioMcpTiming {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

#[async_trait]
impl McpTiming for TokioMcpTiming {
    fn now_ms(&self) -> f64 {
        self.origin.elapsed().as_secs_f64() * 1_000.0
    }

    async fn sleep(&self, milliseconds: f64) {
        tokio::time::sleep(Duration::from_secs_f64(milliseconds / 1_000.0)).await;
    }
}

/// Injectable generation construction and timing dependencies.
#[derive(Clone)]
pub struct ConnectionRuntime {
    /// Fresh-client factory.
    pub factory: Arc<dyn McpClientFactory>,
    /// Monotonic policy time.
    pub timing: Arc<dyn McpTiming>,
}

impl std::fmt::Debug for ConnectionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionRuntime")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ConnectionOutcome {
    error: Option<String>,
}

#[derive(Default)]
struct ReadyState {
    value: Mutex<Option<ConnectionOutcome>>,
    notify: Notify,
}

impl ReadyState {
    fn set(&self, value: ConnectionOutcome) {
        let mut slot = self.value.lock();
        if slot.is_none() {
            *slot = Some(value);
            self.notify.notify_waiters();
        }
    }

    async fn get(&self) -> ConnectionOutcome {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.value.lock().clone() {
                return value;
            }
            notified.await;
        }
    }
}

/// Handle for one plugin instance's supervised generation loop.
pub struct ConnectionHandle {
    cancel: AbortSignal,
    ready: Arc<ReadyState>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>,
    shutdown: OnceCell<Result<(), String>>,
}

impl std::fmt::Debug for ConnectionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionHandle")
            .finish_non_exhaustive()
    }
}

impl ConnectionHandle {
    /// Waits for the initial connection and tool synchronization attempt.
    #[must_use]
    pub async fn initial_error(&self) -> Option<String> {
        self.ready.get().await.error
    }

    /// Stops backoff, quiesces the generation loop, and unregisters all tools once.
    ///
    /// # Errors
    ///
    /// Returns generation close or registration cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.cancel
            .abort_with_reason(Value::String("MCP client disposed".to_owned()));
        let result = self
            .shutdown
            .get_or_init(|| async {
                let task = self.task.lock().await.take();
                match task {
                    Some(task) => task
                        .await
                        .map_err(|error| error.to_string())?
                        .map_err(|error| format!("{error:#}")),
                    None => Ok(()),
                }
            })
            .await;
        result.clone().map_err(anyhow::Error::msg)
    }
}

/// Starts one supervised MCP connection immediately.
#[must_use]
pub fn start_connection(
    context: &Context,
    config: Config,
    policy: ResolvedReconnectPolicy,
    runtime: ConnectionRuntime,
) -> Arc<ConnectionHandle> {
    let cancel = AbortSignal::default();
    let ready = Arc::new(ReadyState::default());
    let task_cancel = cancel.clone();
    let task_ready = Arc::clone(&ready);
    let task_context = context.clone();
    let task = tokio::spawn(async move {
        run_supervisor(
            task_context,
            config,
            policy,
            runtime,
            task_cancel,
            task_ready,
        )
        .await
    });
    Arc::new(ConnectionHandle {
        cancel,
        ready,
        task: tokio::sync::Mutex::new(Some(task)),
        shutdown: OnceCell::new(),
    })
}

#[allow(clippy::too_many_lines)]
async fn run_supervisor(
    context: Context,
    config: Config,
    policy: ResolvedReconnectPolicy,
    runtime: ConnectionRuntime,
    cancel: AbortSignal,
    ready: Arc<ReadyState>,
) -> anyhow::Result<()> {
    let server_name = config.server_name().to_owned();
    let label = format!("mcp-client({server_name})");
    let logger = context.logger(Some("mcp-client"));
    let ordinary_options = ToolBridgeOptions {
        registration_failure: RegistrationFailure::Contain,
        server_name,
        tool_call_timeout_ms: config.tool_call_timeout_ms(),
    };
    let startup_options = ToolBridgeOptions {
        registration_failure: if config.fail_on_startup_error() {
            RegistrationFailure::Throw
        } else {
            RegistrationFailure::Contain
        },
        ..ordinary_options.clone()
    };
    let mut tools = ToolDisposers::new();
    let mut current: Option<Arc<dyn McpClient>> = None;
    let mut failed_attempts = 0_u64;
    let mut connected_at = None;
    let mut startup = true;

    loop {
        if cancel.is_aborted() {
            break;
        }
        let generation = match runtime.factory.create(&config).await {
            Ok(generation) => generation,
            Err(error) => {
                if startup {
                    ready.set(ConnectionOutcome {
                        error: Some(format!("{error:#}")),
                    });
                    startup = false;
                }
                log_warn(
                    &logger,
                    format!("{label}: connection attempt failed: {error}"),
                );
                if !schedule_next(
                    &logger,
                    &label,
                    &policy,
                    &runtime,
                    &cancel,
                    &mut failed_attempts,
                    &mut connected_at,
                    &mut tools,
                    false,
                )
                .await?
                {
                    break;
                }
                continue;
            }
        };
        current = Some(Arc::clone(&generation));
        let connect = tokio::select! {
            result = generation.connect() => result,
            () = cancel.cancelled() => break,
        };
        let attempt = match connect {
            Ok(()) if !generation.closed_signal().is_aborted() => {
                let options = if startup {
                    &startup_options
                } else {
                    &ordinary_options
                };
                tokio::select! {
                    result = sync_tools(Arc::clone(&generation), &context, options, &mut tools) => result,
                    () = cancel.cancelled() => break,
                }
            }
            Ok(()) => Err(anyhow::anyhow!("MCP generation closed during connect")),
            Err(error) => Err(error),
        };

        if let Err(error) = attempt {
            if startup {
                ready.set(ConnectionOutcome {
                    error: Some(format!("{error:#}")),
                });
                startup = false;
            }
            if !cancel.is_aborted() {
                log_warn(
                    &logger,
                    format!("{label}: connection attempt failed: {error}"),
                );
            }
            let quiesced =
                close_with_timeout(&generation, &runtime.timing, GENERATION_CLOSE_TIMEOUT_MS).await;
            current = None;
            if cancel.is_aborted() {
                break;
            }
            if !quiesced {
                log_error(
                    &logger,
                    format!(
                        "{label}: failed generation did not close within {GENERATION_CLOSE_TIMEOUT_MILLIS}ms — reconnect stopped to avoid overlapping server processes; reload the plugin or restart the Host to retry"
                    ),
                );
                break;
            }
            if !schedule_next(
                &logger,
                &label,
                &policy,
                &runtime,
                &cancel,
                &mut failed_attempts,
                &mut connected_at,
                &mut tools,
                false,
            )
            .await?
            {
                break;
            }
            continue;
        }

        if startup {
            ready.set(ConnectionOutcome::default());
            startup = false;
        }
        connected_at = Some(runtime.timing.now_ms());
        if failed_attempts > 0 {
            log_info(
                &logger,
                format!(
                    "{label}: reconnected and re-synced tools (attempt {failed_attempts}/{})",
                    policy.max_attempts
                ),
            );
        }

        let mut list_generation = generation.list_change_generation();
        let lost = loop {
            let closed = generation.closed_signal();
            tokio::select! {
                () = cancel.cancelled() => break false,
                () = closed.cancelled() => break true,
                () = generation.wait_list_change(list_generation) => {
                    if generation.closed_signal().is_aborted() {
                        break true;
                    }
                    list_generation = generation.list_change_generation();
                    log_info(&logger, format!("{label}: tool list changed, re-syncing"));
                    let sync_closed = generation.closed_signal();
                    let result = tokio::select! {
                        result = sync_tools(
                            Arc::clone(&generation),
                            &context,
                            &ordinary_options,
                            &mut tools,
                        ) => result,
                        () = cancel.cancelled() => break false,
                        () = sync_closed.cancelled() => break true,
                    };
                    if let Err(error) = result
                        && !cancel.is_aborted()
                    {
                        log_error(&logger, format!("{label}: tool re-sync failed: {error}"));
                    }
                }
            }
        };
        if !lost {
            break;
        }
        let _ = generation.close().await;
        current = None;
        if !schedule_next(
            &logger,
            &label,
            &policy,
            &runtime,
            &cancel,
            &mut failed_attempts,
            &mut connected_at,
            &mut tools,
            true,
        )
        .await?
        {
            break;
        }
    }

    if startup {
        ready.set(ConnectionOutcome {
            error: Some(format!("{label}: initial connection failed")),
        });
    }
    let mut failures = Vec::new();
    if let Some(generation) = current
        && !close_with_timeout(&generation, &runtime.timing, GENERATION_CLOSE_TIMEOUT_MS).await
    {
        log_error(
            &logger,
            format!(
                "{label}: generation did not close within {GENERATION_CLOSE_TIMEOUT_MILLIS}ms during disposal — server shutdown may be incomplete"
            ),
        );
    }
    if let Err(error) = dispose_generation(tools).await {
        failures.push(format!("{error:#}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("; "))
    }
}

#[allow(clippy::too_many_arguments)]
async fn schedule_next(
    logger: &Logger,
    label: &str,
    policy: &ResolvedReconnectPolicy,
    runtime: &ConnectionRuntime,
    cancel: &AbortSignal,
    failed_attempts: &mut u64,
    connected_at: &mut Option<f64>,
    tools: &mut ToolDisposers,
    lost_established_connection: bool,
) -> anyhow::Result<bool> {
    if !policy.enabled {
        let message = if lost_established_connection {
            "connection lost and reconnect is disabled — registered tools will fail until an HMR reload or Host restart"
        } else {
            "connection failed and reconnect is disabled — no tools were registered; reload the plugin or restart the Host to connect"
        };
        log_error(logger, format!("{label}: {message}"));
        cancel.cancelled().await;
        return Ok(false);
    }
    if connected_at.is_some_and(|started| runtime.timing.now_ms() - started >= policy.max_delay_ms)
    {
        *failed_attempts = 0;
    }
    *connected_at = None;
    *failed_attempts = failed_attempts.saturating_add(1);
    if *failed_attempts > policy.max_attempts {
        dispose_generation(std::mem::take(tools)).await?;
        log_error(
            logger,
            format!(
                "{label}: giving up after {} consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect",
                policy.max_attempts
            ),
        );
        cancel.cancelled().await;
        return Ok(false);
    }
    let mut delay = policy.initial_delay_ms;
    for _ in 1..*failed_attempts {
        delay = (delay * 2.0).min(policy.max_delay_ms);
        if delay >= policy.max_delay_ms {
            break;
        }
    }
    let action = if lost_established_connection {
        "connection lost; reconnecting"
    } else {
        "connection failed; retrying"
    };
    log_warn(
        logger,
        format!(
            "{label}: {action} in {}ms (attempt {}/{})",
            format_milliseconds(delay),
            *failed_attempts,
            policy.max_attempts
        ),
    );
    tokio::select! {
        () = cancel.cancelled() => Ok(false),
        () = runtime.timing.sleep(delay) => Ok(true),
    }
}

async fn close_with_timeout(
    generation: &Arc<dyn McpClient>,
    timing: &Arc<dyn McpTiming>,
    timeout_ms: f64,
) -> bool {
    let closed = generation.closed_signal();
    tokio::select! {
        () = closed.cancelled() => true,
        result = generation.close() => {
            if result.is_ok() || closed.is_aborted() {
                true
            } else {
                tokio::select! {
                    () = closed.cancelled() => true,
                    () = timing.sleep(timeout_ms) => false,
                }
            }
        },
        () = timing.sleep(timeout_ms) => false,
    }
}

fn format_milliseconds(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn log_error(logger: &Logger, message: String) {
    logger.error([Value::String(message)]);
}

fn log_warn(logger: &Logger, message: String) {
    logger.warn([Value::String(message)]);
}

fn log_info(logger: &Logger, message: String) {
    logger.info([Value::String(message)]);
}
