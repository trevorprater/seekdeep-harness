//! Cooperative per-tool timeout policy.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventOptions, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::ContentBlock;
use seekdeep_tools::{
    ToolDispatchExecution, ToolErrorInfo, ToolExecutionFailure, ToolExecutionResult, ToolFailure,
    ToolRuntime,
};
use seekdeep_util::timeout::{deadline, timeout_of};

/// Stable policy-owned timeout code.
pub const TOOL_TIMEOUT: &str = "TOOL_TIMEOUT";

/// Installs the `tools/execute` timeout wrapper.
///
/// A tool without declared timeout metadata delegates unchanged. For a
/// budgeted tool, the wrapper temporarily installs a fused deadline signal,
/// awaits the tool to reach quiescence, restores the upstream signal, and only
/// then substitutes a structured `TOOL_TIMEOUT` result when its own timer won.
///
/// # Errors
///
/// Returns when the owning context is inactive.
pub fn install(context: &Context, tools: &Arc<ToolRuntime>) -> anyhow::Result<EffectHandle> {
    let tools_for_middleware = tools.clone();
    Ok(tools.on_execute(
        context,
        move |execution, next| {
            let tools = tools_for_middleware.clone();
            async move {
                let Some(timeout_ms) = tools
                    .get(&execution.name, execution.scope_key())
                    .and_then(|definition| definition.timeout_ms)
                else {
                    return next.run().await;
                };

                let upstream = execution.signal();
                let deadline = deadline(Some(&upstream), timeout_ms, TOOL_TIMEOUT)?;
                let restore = DispatchSignalRestore::new(execution, deadline.signal.clone());
                let result = next.run().await;
                let timed_out = timeout_of(&deadline.signal, Some(TOOL_TIMEOUT)).is_some();
                drop(restore);
                drop(deadline);
                match result {
                    Ok(_) if timed_out => Ok(tool_timeout_result(timeout_ms)),
                    result => result,
                }
            }
        },
        EventOptions::default(),
    )?)
}

struct DispatchSignalRestore {
    execution: ToolDispatchExecution,
    upstream: Option<seekdeep_util::abort::AbortSignal>,
}

impl DispatchSignalRestore {
    fn new(
        execution: ToolDispatchExecution,
        replacement: seekdeep_util::abort::AbortSignal,
    ) -> Self {
        let upstream = execution.replace_dispatch_signal(replacement);
        Self {
            execution,
            upstream: Some(upstream),
        }
    }
}

impl Drop for DispatchSignalRestore {
    fn drop(&mut self) {
        if let Some(upstream) = self.upstream.take() {
            let _derived = self.execution.replace_dispatch_signal(upstream);
        }
    }
}

fn tool_timeout_result(timeout_ms: f64) -> ToolExecutionResult {
    let message = format!("tool call timed out after {timeout_ms}ms");
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: vec![ContentBlock::Text {
            text: format!("Error: {message}"),
        }],
        error: ToolFailure {
            message,
            info: Some(ToolErrorInfo {
                name: "ToolTimeoutError".to_owned(),
                code: TOOL_TIMEOUT.to_owned(),
            }),
        },
        meta: None,
        additional_contexts: Vec::new(),
    })
}

/// Registers the policy's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-tool-timeout-policy", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use seekdeep_llm::{AbortSignal, CallId};
    use seekdeep_tools::{
        ToolDefinition, ToolExecutionInput, ToolOutputDefinition, ToolRuntimeConfig,
        assert_supported_json_schema,
    };
    use serde_json::{Map, Value, json};

    use super::*;

    fn definition(
        name: &str,
        timeout_ms: Option<f64>,
        execute: seekdeep_tools::runtime::ToolExecute,
    ) -> ToolDefinition {
        let mut definition = ToolDefinition::new(
            name,
            "test tool",
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(assert_supported_json_schema(json!({ "type": "string" })).unwrap()),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap_or_default().to_owned(),
                    }])
                }),
            ),
            execute,
        );
        definition.timeout_ms = timeout_ms;
        definition
    }

    fn input(name: &str, signal: AbortSignal) -> ToolExecutionInput {
        ToolExecutionInput::new(CallId::new("call"), name, json!({}), signal)
    }

    fn setup() -> (Context, Arc<ToolRuntime>, EffectHandle) {
        let context = Context::new();
        let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
        let policy = install(&context, &tools).unwrap();
        (context, tools, policy)
    }

    #[tokio::test]
    async fn unbudgeted_tool_delegates_with_exact_signal() {
        let (context, tools, _policy) = setup();
        let upstream = AbortSignal::default();
        let expected = upstream.clone();
        let same = Arc::new(AtomicBool::new(false));
        let observed = same.clone();
        tools
            .register(
                &context,
                definition(
                    "probe",
                    None,
                    Arc::new(move |_, execution| {
                        observed.store(execution.signal() == expected, Ordering::Release);
                        Box::pin(async { Ok(json!("ok")) })
                    }),
                ),
            )
            .unwrap();
        let result = tools.execute(input("probe", upstream)).await;
        assert!(!result.is_error());
        assert!(same.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn fast_budgeted_tool_gets_derived_signal_and_keeps_result() {
        let (context, tools, _policy) = setup();
        let upstream = AbortSignal::default();
        let expected = upstream.clone();
        let derived = Arc::new(AtomicBool::new(false));
        let observed = derived.clone();
        tools
            .register(
                &context,
                definition(
                    "fast",
                    Some(10_000.0),
                    Arc::new(move |_, execution| {
                        observed.store(execution.signal() != expected, Ordering::Release);
                        Box::pin(async { Ok(json!("ok")) })
                    }),
                ),
            )
            .unwrap();
        let result = tools.execute(input("fast", upstream)).await;
        assert!(!result.is_error());
        assert!(derived.load(Ordering::Acquire));
        assert_eq!(
            result.content(),
            &[ContentBlock::Text { text: "ok".into() }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn own_deadline_replaces_cooperative_result_with_structured_timeout() {
        let (context, tools, _policy) = setup();
        tools
            .register(
                &context,
                definition(
                    "slow",
                    Some(100.0),
                    Arc::new(|_, execution| {
                        Box::pin(async move {
                            execution.signal().cancelled().await;
                            Ok(json!("stopped cooperatively"))
                        })
                    }),
                ),
            )
            .unwrap();
        let pending = tokio::spawn({
            let tools = tools.clone();
            async move { tools.execute(input("slow", AbortSignal::default())).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        let result = pending.await.unwrap();
        assert_eq!(
            result.error(),
            Some(&ToolFailure {
                message: "tool call timed out after 100ms".into(),
                info: Some(ToolErrorInfo {
                    name: "ToolTimeoutError".into(),
                    code: TOOL_TIMEOUT.into()
                }),
            })
        );
        assert_eq!(
            result.content(),
            &[ContentBlock::Text {
                text: "Error: tool call timed out after 100ms".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn own_deadline_also_replaces_tool_owned_abort_failure() {
        let (context, tools, _policy) = setup();
        tools
            .register(
                &context,
                definition(
                    "aborter",
                    Some(100.0),
                    Arc::new(|_, execution| {
                        Box::pin(async move {
                            execution.signal().cancelled().await;
                            Err(anyhow::anyhow!("web fetch aborted"))
                        })
                    }),
                ),
            )
            .unwrap();
        let pending = tokio::spawn({
            let tools = tools.clone();
            async move {
                tools
                    .execute(input("aborter", AbortSignal::default()))
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        let result = pending.await.unwrap();
        assert_eq!(
            result
                .error()
                .and_then(|failure| failure.info.as_ref())
                .map(|info| info.code.as_str()),
            Some(TOOL_TIMEOUT)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_abort_first_stays_registry_aborted() {
        let (context, tools, _policy) = setup();
        let entered = Arc::new(tokio::sync::Notify::new());
        tools
            .register(
                &context,
                definition(
                    "slow",
                    Some(100.0),
                    Arc::new({
                        let entered = entered.clone();
                        move |_, execution| {
                            let entered = entered.clone();
                            Box::pin(async move {
                                entered.notify_one();
                                execution.signal().cancelled().await;
                                Ok(json!("stopped"))
                            })
                        }
                    }),
                ),
            )
            .unwrap();
        let upstream = AbortSignal::default();
        let pending = tokio::spawn({
            let tools = tools.clone();
            let signal = upstream.clone();
            async move { tools.execute(input("slow", signal)).await }
        });
        entered.notified().await;
        upstream.abort_with_reason(json!("user cancelled"));
        let result = pending.await.unwrap();
        assert_eq!(
            result
                .error()
                .and_then(|failure| failure.info.as_ref())
                .map(|info| info.code.as_str()),
            Some(seekdeep_tools::TOOL_ABORTED)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_first_survives_later_upstream_abort_and_cleanup() {
        let (context, tools, _policy) = setup();
        let saw_abort = Arc::new(tokio::sync::Notify::new());
        let release_cleanup = Arc::new(tokio::sync::Notify::new());
        tools
            .register(
                &context,
                definition(
                    "slow-cleanup",
                    Some(100.0),
                    Arc::new({
                        let saw_abort = saw_abort.clone();
                        let release_cleanup = release_cleanup.clone();
                        move |_, execution| {
                            let saw_abort = saw_abort.clone();
                            let release_cleanup = release_cleanup.clone();
                            Box::pin(async move {
                                execution.signal().cancelled().await;
                                saw_abort.notify_one();
                                release_cleanup.notified().await;
                                Ok(json!("cleanup complete"))
                            })
                        }
                    }),
                ),
            )
            .unwrap();
        let upstream = AbortSignal::default();
        let pending = tokio::spawn({
            let tools = tools.clone();
            let signal = upstream.clone();
            async move { tools.execute(input("slow-cleanup", signal)).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        saw_abort.notified().await;
        upstream.abort_with_reason(json!("too late"));
        release_cleanup.notify_one();
        let result = pending.await.unwrap();
        assert_eq!(
            result
                .error()
                .and_then(|failure| failure.info.as_ref())
                .map(|info| info.code.as_str()),
            Some(TOOL_TIMEOUT)
        );
    }

    #[tokio::test]
    async fn restores_upstream_before_post_execute_middleware() {
        let (context, tools, _policy) = setup();
        tools
            .register(
                &context,
                definition(
                    "fast",
                    Some(10_000.0),
                    Arc::new(|_, _| Box::pin(async { Ok(json!("ok")) })),
                ),
            )
            .unwrap();
        let upstream = AbortSignal::default();
        let expected = upstream.clone();
        let restored = Arc::new(AtomicBool::new(false));
        let observed = restored.clone();
        tools
            .on_post_execute(
                &context,
                move |execution, _result, next| {
                    observed.store(execution.signal() == expected, Ordering::Release);
                    async move { next.run().await }
                },
                EventOptions::default(),
            )
            .unwrap();
        let result = tools.execute(input("fast", upstream)).await;
        assert!(!result.is_error());
        assert!(restored.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn disposal_removes_wrapper_and_invariant_reserves_name() {
        let (context, tools, policy) = setup();
        let upstream = AbortSignal::default();
        let expected = upstream.clone();
        let saw_upstream = Arc::new(AtomicBool::new(false));
        let observed = saw_upstream.clone();
        tools
            .register(
                &context,
                definition(
                    "probe",
                    Some(10_000.0),
                    Arc::new(move |_, execution| {
                        observed.store(execution.signal() == expected, Ordering::Release);
                        Box::pin(async { Ok(json!("ok")) })
                    }),
                ),
            )
            .unwrap();
        policy.dispose().await.unwrap();
        tools.execute(input("probe", upstream)).await;
        assert!(saw_upstream.load(Ordering::Acquire));

        let registry =
            InvariantRegistry::install(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap();
        let invariant = register_invariant(&registry).unwrap();
        invariant.await_ready().await.unwrap();
        assert!(registry.is_registered("seekdeep-tool-timeout-policy"));
    }
}
