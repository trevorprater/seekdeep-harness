//! Basic replay-aware compaction backend.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent, AgentStatus, RequestErrorAction};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentRequestErrorEvent, AgentStatusChanged};
use seekdeep_commands::CommandId;
use seekdeep_compaction::service::{
    COMPACTION, CompactionAgentContext, CompactionEngine, CompactionResult, CompactionRoutingOptions,
    CompactionService, CompactionTrigger, ManualCompactAgentContext,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_llm::{AbortSignal, CONTEXT_WINDOW_EXCEEDED_CODE, LLM};
use seekdeep_schemastery::Schema;
use seekdeep_token_meter::{TOKEN_METER, TokenMeter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{resolve_compact_spec, resolve_config, resolve_target_policy, TargetPressureConfigError};
use crate::region::{
    RegionDependencies, RegionSummarize, assert_no_active_compaction, compact_surface_region,
    select_compactable_range, CompactionTransactionOptions, Stability,
};
use crate::summarizer::{SummarizationInput, SummaryResult, Target, summarize_with_llm};
use crate::types::{BasicCompactionConfig, ResolvedConfig};

/// Cordis plugin name.
pub const NAME: &str = "compaction-basic";

/// Services required by the backend.
pub const INJECT: &[&str] = &["llm", "tokenMeter", "sessions"];

/// The source-compatible admission schema for [`BasicCompactionConfig`].
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

/// Dependency-light compaction backend using the token meter for pressure,
/// retention, and summary-convergence pricing.
pub struct BasicCompactionEngine {
    context: Context,
    /// Resolved and validated compaction configuration.
    pub config: ResolvedConfig,
    warned_pressure_targets: Mutex<std::collections::HashSet<String>>,
    overflow_retries: Mutex<std::collections::HashMap<usize, u64>>,
    overflow_agents: Mutex<std::collections::HashMap<usize, Arc<Agent>>>,
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
            warned_pressure_targets: Mutex::new(std::collections::HashSet::new()),
            overflow_retries: Mutex::new(std::collections::HashMap::new()),
            overflow_agents: Mutex::new(std::collections::HashMap::new()),
        });
        let service = CompactionService::new(backend.clone());
        service.provide(context)?;
        if backend.config.auto {
            backend.register_automatic_compaction()?;
        }
        Ok(backend)
    }

    fn register_automatic_compaction(self: &Arc<Self>) -> anyhow::Result<()> {
        let ctx = self.context.clone();
        let pre_step_backend = self.clone();
        ctx.events().on_waterfall(
            &ctx,
            "agent/pre-step",
            move |_, args, next| {
                let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                    return Box::pin(async { Err(anyhow::anyhow!("agent/pre-step missing payload")) });
                };
                let agent = event.agent.clone();
                let signal = event.payload.signal.clone();
                let backend = pre_step_backend.clone();
                Box::pin(async move {
                    if !signal.is_aborted() {
                        let compact_agent = compaction_agent_context(&agent);
                        if let Err(error) = backend
                            .compact_if_needed(&compact_agent, CompactionTrigger::Pressure, &signal)
                            .await
                        {
                            tracing::warn!(
                                "step compaction failed: {}; continuing the turn",
                                error_chain_message(&error)
                            );
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
                        .remove(&Arc::as_ptr(&event.agent) as usize);
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
                    .cloned();
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
                    return Box::pin(async { Err(anyhow::anyhow!("agent/request-error missing payload")) });
                };
                let agent = event.agent.clone();
                let signal = event.payload.signal.clone();
                let failure_code = event.payload.failure.code.clone();
                let backend = error_backend.clone();
                Box::pin(async move {
                    if failure_code != CONTEXT_WINDOW_EXCEEDED_CODE || signal.is_aborted() {
                        return next.run().await;
                    }
                    let session_key = Arc::as_ptr(agent.session()) as usize;
                    backend
                        .overflow_agents
                        .lock()
                        .insert(session_key, agent.clone());
                    let Some(target) = routed_target(agent.session()) else {
                        return next.run().await;
                    };
                    let policy = resolve_target_policy(&backend.config, &target);
                    let agent_key = Arc::as_ptr(&agent) as usize;
                    let retries = backend.overflow_retries.lock().get(&agent_key).copied().unwrap_or(0);
                    if retries >= policy.max_overflow_retries {
                        return next.run().await;
                    }
                    let compact_agent = compaction_agent_context(&agent);
                    match backend
                        .compact_if_needed(&compact_agent, CompactionTrigger::ContextOverflow, &signal)
                        .await
                    {
                        Ok(_) => {
                            backend.overflow_retries.lock().insert(agent_key, retries + 1);
                            let reply = EventReply::Value(Arc::new(RequestErrorAction::Retry));
                            Ok(reply)
                        }
                        Err(error) => {
                            tracing::warn!(
                                "context-overflow compaction failed: {}; preserving the original request error",
                                error_chain_message(&error)
                            );
                            next.run().await
                        }
                    }
                })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    /// Summarizes the replayed conversation region through a one-shot LLM call.
    fn summarize(
        &self,
        input: &SummarizationInput,
        session: &Arc<Session>,
        fallback: Option<Target>,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, anyhow::Result<SummaryResult>> {
        let ctx = self.context.clone();
        let config = self.config.clone();
        let input = input.clone();
        let session = session.clone();
        Box::pin(async move {
            let target = fallback.as_ref().map_or_else(
                || routed_target(&session),
                |fallback| Some(fallback.clone()),
            );
            let config = target.map_or(config.clone(), |target| {
                resolve_target_policy(&config, &target)
            });
            summarize_with_llm(&ctx, &summary_config(&config), &input, &session, target, signal).await
        })
    }

    fn region_dependencies(&self) -> RegionDependencies {
        let backend: Arc<BasicCompactionEngine> = unsafe {
            // SAFETY: `self` always lives inside an `Arc` created in `new`.
            Arc::from_raw(std::ptr::from_ref(self) as *const BasicCompactionEngine)
        };
        let meter = self.context.get(TOKEN_METER).expect("token meter present");
        let backend_for_hook = backend.clone();
        let summarize: RegionSummarize = Arc::new(move |input, session, fallback, signal| {
            backend_for_hook.summarize(&input, &session, fallback, signal)
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
        let policy = resolve_target_policy(&self.config, &target);
        let meter = self.context.get(TOKEN_METER).ok_or_else(|| anyhow::anyhow!("compaction-basic requires tokenMeter"))?;
        let mut measurement = meter.measure(&agent.session, None)?;

        if trigger == CompactionTrigger::ContextOverflow {
            let range = select_compactable_range(&agent.session, &measurement, 0)?;
            let Some(range) = range else {
                return Ok(None);
            };
            return self
                .compact_region(range.start, range.end, agent, Some(signal))
                .await
                .map(Some);
        }

        let llm = self.context.get(LLM).ok_or_else(|| anyhow::anyhow!("compaction-basic requires llm"))?;
        let context_info = llm
            .resolve_model_info(&target.provider, &target.model, Some(signal))
            .await?;
        assert_no_active_compaction(&agent.session, "automatic pressure compaction")?;
        let target_key = format!("{}/{}", target.provider, target.model);
        let Some(context) = context_info.context else {
            return Err(TargetPressureConfigError {
                target_key,
                message: format!(
                    "compaction-basic: no context capacity for {target_key}; configure contextWindow on that adapter model"
                ),
            }
            .into());
        };
        let spec = resolve_compact_spec(&policy, context.context_window)?;
        if measurement.total_tokens < spec.threshold_tokens {
            return Ok(None);
        }

        let mut result = None;
        for _ in 0..=spec.compaction_retries {
            let range = select_compactable_range(&agent.session, &measurement, spec.retain_tokens)?;
            let Some(range) = range else {
                return Ok(result);
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
        let meter = self.context.get(TOKEN_METER).ok_or_else(|| anyhow::anyhow!("compaction-basic requires tokenMeter"))?;
        let fallback = routing_target(&agent.options);
        let dependencies = self.region_dependencies();
        let source_command_id = source_command_id.cloned();
        let runner = agent.run_maintenance.clone();
        let operation = runner(Box::new(move |_agent_signal| {
            let session = session.clone();
            let meter = meter.clone();
            let dependencies = dependencies.clone();
            let fallback = fallback.clone();
            let source_command_id = source_command_id.clone();
            Box::pin(async move {
                let range = select_compactable_range(&session, &meter.measure(&session, None)?, 0)?;
                let Some(range) = range else {
                    return Ok(None);
                };
                let options = CompactionTransactionOptions {
                    owner: None,
                    stability: Stability::SelectedSpan,
                    flush: None,
                    source_command_id,
                };
                compact_surface_region(
                    &dependencies,
                    &session,
                    range.start,
                    range.end,
                    fallback,
                    options,
                    None,
                )
                .await
                .map(Some)
            })
        }));
        operation.await.map_err(|error| {
            if error.downcast_ref::<seekdeep_compaction::service::ManualCompactionError>().is_some() {
                error
            } else {
                anyhow::anyhow!("manual compaction requires an idle agent with no waking queued work: {error}")
            }
        })
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
        let options = CompactionTransactionOptions {
            owner: Some(0),
            stability: Stability::WholeSurface,
            flush: None,
            source_command_id: None,
        };
        compact_surface_region(
            &dependencies,
            &agent.session,
            start,
            end,
            fallback,
            options,
            signal.cloned(),
        )
        .await
    }
}

/// Resolves the exact provider/model durably routed for the latest request.
fn routed_target(session: &Arc<Session>) -> Option<Target> {
    let config = session.request_header()?.config;
    if config.provider.as_str().is_empty() || config.model.as_str().is_empty() {
        return None;
    }
    Some(Target {
        provider: config.provider.as_str().to_owned(),
        model: config.model.as_str().to_owned(),
    })
}

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

fn compaction_agent_context(agent: &Arc<Agent>) -> CompactionAgentContext {
    CompactionAgentContext {
        session: agent.session().clone(),
        options: CompactionRoutingOptions {
            provider: agent.options().provider.as_ref().map(|p| p.as_str().to_owned()),
            model: agent.options().model.as_ref().map(|m| m.as_str().to_owned()),
        },
    }
}

fn summary_config(config: &crate::types::ResolvedTargetPolicy) -> crate::summarizer::SummaryConfig {
    crate::summarizer::SummaryConfig {
        summarization_provider: config.summarization_provider.clone(),
        summarization_model: config.summarization_model.clone(),
        max_tokens: config.max_tokens,
    }
}

fn error_chain_message(error: &anyhow::Error) -> String {
    format!("{error:#}")
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
