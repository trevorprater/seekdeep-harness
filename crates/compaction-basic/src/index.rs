//! Replay-aware basic compaction backend.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Weak},
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent, AgentStatus, RequestErrorAction};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentRequestErrorEvent, AgentStatusChanged};
use seekdeep_commands::CommandId;
use seekdeep_compaction::{
    CompactionResult,
    service::{
        CompactionAgentContext, CompactionEngine, CompactionRoutingOptions, CompactionService,
        CompactionTrigger, MaintenanceTask, ManualCompactAgentContext, ManualCompactionError,
        ManualCompactionErrorCode,
    },
};
use seekdeep_compaction_tool_result_pruner::index::TOOL_RESULT_PRUNER;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, events::Next};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_llm::{AbortSignal, CONTEXT_WINDOW_EXCEEDED_CODE, LLM};
use seekdeep_schemastery::Schema;
use seekdeep_token_meter::TOKEN_METER;
use serde_json::Value;

use crate::config::{
    TargetPressureConfigError, resolve_compact_spec, resolve_config, resolve_target_policy,
};
use crate::region::{
    CompactionTransactionOptions, RegionDependencies, RegionSummarize, Stability,
    assert_no_active_compaction, compact_surface_region, select_compactable_range,
};
use crate::summarizer::{
    SummarizationInput, SummaryConfig, SummaryResult, Target, summarize_with_llm,
};
use crate::types::{BasicCompactionConfig, CompactionTarget, ResolvedConfig};

/// Cordis plugin name.
pub const NAME: &str = "compaction-basic";

/// Services required by the replay-aware backend.
pub const INJECT: &[&str] = &["llm", "tokenMeter", "sessions"];

/// The source-compatible admission schema for `BasicCompactionConfig`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([
        ("thresholdRatio", Schema::number()),
        ("retainRatio", Schema::number()),
        ("retainTokens", Schema::number().step(1.0).min(0.0)),
        ("summarizationProvider", Schema::string()),
        ("summarizationModel", Schema::string()),
        ("maxTokens", Schema::number().step(1.0).min(1.0)),
        ("compactionRetries", Schema::number().step(1.0).min(0.0)),
        ("maxOverflowRetries", Schema::number().step(1.0).min(0.0)),
        ("modelPolicies", Schema::array(model_policy_schema())),
        ("auto", Schema::boolean()),
    ])
}

fn model_policy_schema() -> Schema {
    Schema::object([
        ("provider", Schema::string().required()),
        ("model", Schema::string().required()),
        ("thresholdRatio", Schema::number()),
        ("retainRatio", Schema::number()),
        ("retainTokens", Schema::number().step(1.0).min(0.0)),
        ("summarizationProvider", Schema::string()),
        ("summarizationModel", Schema::string()),
        ("maxTokens", Schema::number().step(1.0).min(1.0)),
        ("compactionRetries", Schema::number().step(1.0).min(0.0)),
        ("maxOverflowRetries", Schema::number().step(1.0).min(0.0)),
    ])
}

/// Dependency-light compaction backend using the singleton token meter for
/// pressure, retention, cited source events, and summary-convergence pricing.
pub struct BasicCompactionEngine {
    context: Context,
    /// Resolved and validated compaction configuration.
    pub config: ResolvedConfig,
    warned_pressure_targets: Mutex<HashSet<String>>,
    overflow_retries: Mutex<HashMap<usize, u64>>,
    overflow_agents: Mutex<HashMap<usize, Weak<Agent>>>,
}

impl BasicCompactionEngine {
    /// Builds, resolves, publishes, and lifecycle-owns the backend.
    ///
    /// # Errors
    ///
    /// Returns invalid-configuration, duplicate-service, or inactive-owner
    /// failures.
    pub fn new(context: &Context, config: &BasicCompactionConfig) -> anyhow::Result<Arc<Self>> {
        let backend = Arc::new(Self {
            context: context.clone(),
            config: resolve_config(config)?,
            warned_pressure_targets: Mutex::new(HashSet::new()),
            overflow_retries: Mutex::new(HashMap::new()),
            overflow_agents: Mutex::new(HashMap::new()),
        });
        CompactionService::new(backend.clone()).provide(context)?;
        if backend.config.auto {
            backend.register_automatic_compaction()?;
        }
        Ok(backend)
    }

    /// Registers automatic between-step pressure and model-request overflow
    /// recovery listeners.
    ///
    /// # Errors
    ///
    /// Returns inactive-owner registration failures.
    #[allow(clippy::too_many_lines)]
    fn register_automatic_compaction(self: &Arc<Self>) -> anyhow::Result<()> {
        let ctx = self.context.clone();

        let pre_step_backend = self.clone();
        ctx.events().on_waterfall(
            &ctx,
            "agent/pre-step",
            move |_, args, next| {
                let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                    return Box::pin(async move {
                        Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                    });
                };
                let agent = event.agent.clone();
                let signal = event.payload.signal.clone();
                let backend = pre_step_backend.clone();
                Box::pin(async move {
                    if !signal.is_aborted() {
                        let compact_agent = compaction_agent_context(&agent);
                        match backend
                            .compact_if_needed(&compact_agent, CompactionTrigger::Pressure, &signal)
                            .await
                        {
                            Ok(Some(result)) => log_compaction(&result, "step pressure"),
                            Ok(None) => {}
                            Err(error) => {
                                let mut skip_warning = false;
                                if let Some(config_error) =
                                    error.downcast_ref::<TargetPressureConfigError>()
                                {
                                    let mut warned = backend.warned_pressure_targets.lock();
                                    if warned.contains(&config_error.target_key) {
                                        skip_warning = true;
                                    } else {
                                        warned.insert(config_error.target_key.clone());
                                    }
                                }
                                if !skip_warning {
                                    tracing::warn!(
                                        "step compaction failed: {error}; continuing the turn"
                                    );
                                }
                            }
                        }
                    }
                    next.run().await
                })
            },
            EventOptions::default(),
        )?;

        let status_backend = self.clone();
        ctx.events().on_sync(
            &ctx,
            "agent/status",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentStatusChanged>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                if event.payload.status == AgentStatus::Idle {
                    status_backend
                        .overflow_retries
                        .lock()
                        .remove(&(Arc::as_ptr(&event.agent) as usize));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let session_backend = self.clone();
        ctx.events().on_sync(
            &ctx,
            "session/event",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let Some(event) = args.get::<SessionEvent>(1) else {
                    return Ok(EventReply::Undefined);
                };
                if event.event_type != "assistant/message" {
                    return Ok(EventReply::Undefined);
                }
                let agent = session_backend
                    .overflow_agents
                    .lock()
                    .get(&(Arc::as_ptr(&session) as usize))
                    .and_then(Weak::upgrade);
                if let Some(agent) = agent {
                    session_backend
                        .overflow_retries
                        .lock()
                        .remove(&(Arc::as_ptr(&agent) as usize));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let error_backend = self.clone();
        ctx.events().on_waterfall(
            &ctx,
            "agent/request-error",
            move |_, args, next| {
                let Some(event) = args.get::<AgentEvent<AgentRequestErrorEvent>>(0) else {
                    return Box::pin(async move {
                        Err(anyhow::anyhow!("agent/request-error lacks its payload"))
                    });
                };
                let backend = error_backend.clone();
                Box::pin(async move { recover_overflow(&backend, &event, next).await })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    /// Binds the effective token meter and the dynamically dispatched
    /// summarizer hook for one region transaction.
    fn region_dependencies(&self) -> RegionDependencies {
        let meter = self.context.get(TOKEN_METER).expect("token meter present");
        let ctx = self.context.clone();
        let config = self.config.clone();
        let summarize: RegionSummarize = Arc::new(move |input, session, fallback, signal| {
            let ctx = ctx.clone();
            let config = config.clone();
            Box::pin(async move {
                summarize_region(&ctx, &config, &input, &session, fallback, signal).await
            })
        });
        RegionDependencies {
            meter: meter.clone(),
            summarize,
        }
    }
}

#[async_trait]
impl CompactionEngine for BasicCompactionEngine {
    async fn compact_if_needed(
        &self,
        agent: &CompactionAgentContext,
        trigger: CompactionTrigger,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<CompactionResult>> {
        let Some(target) = routed_target(&agent.session) else {
            return Ok(None);
        };
        let policy = resolve_target_policy(&self.config, &to_compaction_target(&target));
        let meter = self
            .context
            .get(TOKEN_METER)
            .ok_or_else(|| anyhow::anyhow!("compaction-basic requires tokenMeter"))?;
        let mut measurement = meter.measure(&agent.session, None)?;
        let prune = self.context.get(TOOL_RESULT_PRUNER);

        if trigger == CompactionTrigger::ContextOverflow {
            if let Some(prune) = &prune {
                prune.prune_session(&agent.session)?;
                measurement = meter.measure(&agent.session, None)?;
            }
            let range = select_compactable_range(&agent.session, &measurement, 0)?;
            let Some(range) = range else {
                return Ok(None);
            };
            return self
                .compact_region(range.start, range.end, agent, Some(signal))
                .await
                .map(Some);
        }

        let llm = self
            .context
            .get(LLM)
            .ok_or_else(|| anyhow::anyhow!("compaction-basic requires llm"))?;
        let info = llm
            .resolve_model_info(
                target.provider.as_str(),
                target.model.as_str(),
                Some(signal),
            )
            .await?;
        assert_no_active_compaction(&agent.session, "automatic pressure compaction")?;
        let target_key = format!("{}/{}", target.provider, target.model);
        let Some(context) = info.context else {
            return Err(TargetPressureConfigError {
                message: format!(
                    "compaction-basic: no context capacity for {target_key}; configure contextWindow on that adapter model"
                ),
                target_key,
            }
            .into());
        };
        let spec = resolve_compact_spec(&policy, context.context_window)?;
        if measurement.total_tokens < spec.threshold_tokens {
            return Ok(None);
        }
        if let Some(prune) = &prune {
            prune.prune_session(&agent.session)?;
            measurement = meter.measure(&agent.session, None)?;
        }
        if measurement.total_tokens < spec.threshold_tokens {
            return Ok(None);
        }

        let mut result = None;
        for _ in 0..=spec.compaction_retries {
            let range = select_compactable_range(&agent.session, &measurement, spec.retain_tokens)?;
            let Some(range) = range else {
                if result.is_none() {
                    return Ok(None);
                }
                break;
            };
            result = Some(
                self.compact_region(range.start, range.end, agent, Some(signal))
                    .await?,
            );
            measurement = meter.measure(&agent.session, None)?;
            if measurement.total_tokens < spec.threshold_tokens {
                return Ok(result);
            }
        }

        Err(anyhow::anyhow!(
            "compaction still above threshold after {} compaction attempts ({} estimated tokens >= threshold {})",
            spec.compaction_retries + 1,
            measurement.total_tokens,
            spec.threshold_tokens
        ))
    }

    async fn compact_now(
        &self,
        agent: &ManualCompactAgentContext,
        signal: &AbortSignal,
        source_command_id: Option<&CommandId>,
    ) -> anyhow::Result<Option<CompactionResult>> {
        if signal.is_aborted() {
            anyhow::bail!("compaction aborted");
        }
        let session = agent.session.clone();
        let fallback = routing_target(&agent.options);
        let dependencies = self.region_dependencies();
        let sessions = self.context.get(SESSIONS);
        let caller_signal = signal.clone();
        let source_command_id = source_command_id.cloned();
        let runner = agent.run_maintenance.clone();

        let task: MaintenanceTask = Box::new(move |agent_signal: AbortSignal| {
            let operation_signal = AbortSignal::fuse(&agent_signal, &caller_signal);
            let session = session.clone();
            let fallback = fallback.clone();
            let dependencies = dependencies.clone();
            let sessions = sessions.clone();
            let source_command_id = source_command_id.clone();
            Box::pin(async move {
                let operation = async {
                    if operation_signal.is_aborted() {
                        anyhow::bail!("compaction aborted");
                    }
                    let measurement = dependencies.meter.measure(&session, None)?;
                    let range = select_compactable_range(&session, &measurement, 0)?;
                    let Some(range) = range else {
                        return Ok(None);
                    };
                    let flush: Option<BoxFuture<'static, anyhow::Result<()>>> =
                        sessions.map(|sessions| {
                            let session = session.clone();
                            Box::pin(async move { sessions.flush(&session).await.map(|_| ()) })
                                as BoxFuture<'static, anyhow::Result<()>>
                        });
                    compact_surface_region(
                        &dependencies,
                        &session,
                        range.start,
                        range.end,
                        fallback,
                        CompactionTransactionOptions {
                            owner: None,
                            stability: Stability::SelectedSpan,
                            flush,
                            source_command_id,
                        },
                        Some(operation_signal.clone()),
                    )
                    .await
                    .map(Some)
                }
                .await;

                match operation {
                    Ok(result) => Ok(result),
                    Err(error) => {
                        let mapped = if agent_signal.is_aborted()
                            && agent_signal.reason().is_some()
                            && operation_signal.reason() == agent_signal.reason()
                        {
                            anyhow::Error::from(ManualCompactionError::new(
                                ManualCompactionErrorCode::Cancelled,
                                "manual compaction was cancelled",
                            ))
                        } else if operation_signal.is_aborted() {
                            anyhow::anyhow!("compaction aborted")
                        } else {
                            error
                        };
                        Err(anyhow::Error::from(CompactNowTaskFailure(mapped)))
                    }
                }
            }) as BoxFuture<'static, anyhow::Result<Option<CompactionResult>>>
        });

        match runner(task).await {
            Ok(result) => Ok(result),
            Err(error) => match error.downcast::<CompactNowTaskFailure>() {
                Ok(CompactNowTaskFailure(inner)) => Err(inner),
                Err(_) => Err(ManualCompactionError::new(
                    ManualCompactionErrorCode::Busy,
                    "manual compaction requires an idle agent with no waking queued work",
                )
                .into()),
            },
        }
    }

    async fn compact_region(
        &self,
        start: u64,
        end: u64,
        agent: &CompactionAgentContext,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<CompactionResult> {
        let dependencies = self.region_dependencies();
        let fallback = routing_target(&agent.options);
        compact_surface_region(
            &dependencies,
            &agent.session,
            start,
            end,
            fallback,
            CompactionTransactionOptions {
                owner: Some(0),
                stability: Stability::WholeSurface,
                flush: None,
                source_command_id: None,
            },
            signal.cloned(),
        )
        .await
    }
}

/// Marks a task-produced failure so the idle-only reservation failure reported
/// by the maintenance runner remains distinguishable from operation failures.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct CompactNowTaskFailure(anyhow::Error);

/// Resolves the exact provider/model durably routed for the latest request.
fn routed_target(session: &Arc<Session>) -> Option<Target> {
    let config = &session.request_header()?.config;
    if config.provider.as_str().is_empty() || config.model.as_str().is_empty() {
        return None;
    }
    Some(Target {
        provider: config.provider.as_str().to_owned(),
        model: config.model.as_str().to_owned(),
    })
}

/// Resolves the agent-options fallback target used by the summarizer.
fn routing_target(options: &CompactionRoutingOptions) -> Option<Target> {
    let provider = options.provider.as_deref()?;
    let model = options.model.as_deref()?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(Target {
        provider: provider.to_owned(),
        model: model.to_owned(),
    })
}

fn to_compaction_target(target: &Target) -> CompactionTarget {
    CompactionTarget {
        provider: target.provider.clone(),
        model: target.model.clone(),
    }
}

fn compaction_agent_context(agent: &Arc<Agent>) -> CompactionAgentContext {
    CompactionAgentContext {
        session: agent.session().clone(),
        options: CompactionRoutingOptions {
            provider: agent
                .options()
                .provider
                .as_ref()
                .map(|p| p.as_str().to_owned()),
            model: agent
                .options()
                .model
                .as_ref()
                .map(|m| m.as_str().to_owned()),
        },
    }
}

/// Summarizes one replayed region through a direct one-shot ctx.llm.stream call,
/// reusing the conversation's routed target for policy selection.
async fn summarize_region(
    ctx: &Context,
    config: &ResolvedConfig,
    input: &SummarizationInput,
    session: &Arc<Session>,
    fallback: Option<Target>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<SummaryResult> {
    let conversation = routed_target(session).or(fallback.clone());
    let summary_config = match conversation.as_ref() {
        Some(target) => {
            let policy = resolve_target_policy(config, &to_compaction_target(target));
            SummaryConfig {
                summarization_provider: policy.summarization_provider.clone(),
                summarization_model: policy.summarization_model.clone(),
                max_tokens: policy.max_tokens,
            }
        }
        None => SummaryConfig {
            summarization_provider: config.summarization_provider.clone(),
            summarization_model: config.summarization_model.clone(),
            max_tokens: config.max_tokens,
        },
    };
    summarize_with_llm(ctx, &summary_config, input, session, fallback, signal).await
}

/// Handles canonical context-overflow recovery, returning a retry action only
/// when durable surface progress advanced.
async fn recover_overflow(
    backend: &Arc<BasicCompactionEngine>,
    event: &AgentEvent<AgentRequestErrorEvent>,
    next: Next,
) -> anyhow::Result<EventReply> {
    let agent = &event.agent;
    let signal = &event.payload.signal;
    if event.payload.failure.code != CONTEXT_WINDOW_EXCEEDED_CODE || signal.is_aborted() {
        return next.run().await;
    }
    let session_key = Arc::as_ptr(agent.session()) as usize;
    backend
        .overflow_agents
        .lock()
        .insert(session_key, Arc::downgrade(agent));
    let Some(target) = routed_target(agent.session()) else {
        return next.run().await;
    };
    let policy = resolve_target_policy(&backend.config, &to_compaction_target(&target));
    let agent_key = Arc::as_ptr(agent) as usize;
    let retries = backend
        .overflow_retries
        .lock()
        .get(&agent_key)
        .copied()
        .unwrap_or(0);
    if retries >= policy.max_overflow_retries {
        return next.run().await;
    }

    let generation = agent.session().replace_generation();
    let compact_agent = compaction_agent_context(agent);
    match backend
        .compact_if_needed(&compact_agent, CompactionTrigger::ContextOverflow, signal)
        .await
    {
        Ok(result) => {
            if signal.is_aborted() || agent.session().replace_generation() <= generation {
                return next.run().await;
            }
            if let Some(result) = result {
                log_compaction(&result, "context overflow recovery");
            }
            backend
                .overflow_retries
                .lock()
                .insert(agent_key, retries + 1);
            Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
        }
        Err(error) => {
            if !signal.is_aborted() && agent.session().replace_generation() > generation {
                tracing::warn!(
                    "context-overflow compaction failed after durable surface progress: {error}; retrying from the replacement surface"
                );
                backend
                    .overflow_retries
                    .lock()
                    .insert(agent_key, retries + 1);
                Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
            } else {
                let outcome = if signal.is_aborted() {
                    "cancellation prevents retry"
                } else {
                    "preserving the original request error"
                };
                tracing::warn!("context-overflow compaction failed: {error}; {outcome}");
                next.run().await
            }
        }
    }
}

fn log_compaction(result: &CompactionResult, trigger: &str) {
    tracing::info!(
        "compaction ({}): shadowed {} surface nodes (seqs {}-{}, ~{} tokens)",
        trigger,
        result.shadowed_seqs.len(),
        result.shadowed_range.start,
        result.shadowed_range.end,
        result.shadowed_token_count
    );
}

/// Builds the loader-compatible compaction backend plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: BasicCompactionConfig = serde_json::from_value(config)?;
            BasicCompactionEngine::new(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
