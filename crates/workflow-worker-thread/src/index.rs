//! Worker-thread workflow engine: each run executes its model-written script
//! in an escapable context on a fresh worker and bridges `agent()` calls to host
//! subagents.

use std::sync::Arc;

use boa_engine::{Source, context::ContextBuilder, script::Script};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{SUBAGENTS, SubagentRuntime};
use seekdeep_workflow::{
    WorkflowEngine, WorkflowResultInfo, WorkflowRun, WorkflowRunId, WorkflowRunInfo,
    WorkflowStartRequest, emit_workflow_event, index::WorkflowEngineService,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    host::WorkerRun,
    meta::validate_meta,
    types::{WorkerInit, WorkerLimits},
};

/// Cordis plugin name.
pub const NAME: &str = "workflow-worker-thread";
/// Services required by the engine.
pub const INJECT: &[&str] = &["subagents"];

/// Plugin config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The provider children run on.
    pub provider: String,
    /// Concurrent `agent()` ceiling; 0 auto-resolves.
    pub max_concurrent_agents: u64,
    /// Total `agent()` calls one run may start.
    pub max_total_agents: u64,
    /// Items accepted by a single `parallel()`/`pipeline()` call.
    pub max_items_per_call: u64,
    /// Timeout for the script's initial synchronous slice.
    pub sync_timeout_ms: u64,
    /// Cancellation grace before forced settlement.
    pub dispose_grace_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "spawn".to_owned(),
            max_concurrent_agents: 0,
            max_total_agents: 1000,
            max_items_per_call: 4096,
            sync_timeout_ms: 5000,
            dispose_grace_ms: 5000,
        }
    }
}

/// Whether a body opens with the Claude Code-style meta header.
fn has_meta_statement(body: &str) -> bool {
    let trimmed = body.trim_start();
    let mut rest = trimmed;
    for keyword in ["export", "const", "meta"] {
        let Some(after) = rest.strip_prefix(keyword) else {
            return false;
        };
        rest = after;
        if keyword != "meta" {
            let Some(after) = rest.strip_prefix(char::is_whitespace) else {
                return false;
            };
            rest = after;
        }
    }
    // The last keyword must be followed by a word boundary.
    !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Parse-check the body with the same wrapper the worker compiles.
fn assert_body_parses(body: &str, _name: &str) {
    assert!(
        !has_meta_statement(body),
        "workflow meta rides the meta request field, not the script: remove the export const meta statement from the body"
    );
    let source = format!(
        "(async () => {{
{body}
}})()"
    );
    let mut context = ContextBuilder::new().build().expect("workflow context");
    if let Err(error) = Script::parse(Source::from_bytes(&source), None, &mut context) {
        panic!("workflow script does not parse: {error}");
    }
}

/// Resolve one run's provider route before publishing work.
fn resolve_subagent_provider(
    context: &Context,
    configured: &str,
    override_provider: Option<&str>,
) -> String {
    let provider = override_provider.unwrap_or(configured);
    assert!(
        !provider.is_empty() && provider == provider.trim(),
        "workflow subagentProvider must be a non-empty normalized string"
    );
    let subagents = context.get(SUBAGENTS).expect("workflow requires subagents");
    assert!(
        subagents.get_provider(provider).is_some(),
        "no subagent provider registered for \"{provider}\""
    );
    provider.to_owned()
}

/// Resolve one run's total-child cap against the engine deployment ceiling.
fn resolve_max_total_agents(requested: Option<u64>, ceiling: u64) -> u64 {
    let Some(requested) = requested else {
        return ceiling;
    };
    assert!(
        requested >= 1,
        "workflow maxTotalAgents must be a positive safe integer"
    );
    assert!(
        requested <= ceiling,
        "workflow maxTotalAgents {requested} exceeds the engine ceiling {ceiling}"
    );
    requested
}

/// Auto-resolve the concurrency ceiling.
fn resolve_max_concurrent(configured: u64) -> u64 {
    if configured != 0 {
        return configured;
    }
    let cores = std::thread::available_parallelism().map_or(1, |value| value.get() as u64);
    cores.saturating_sub(2).clamp(1, 16)
}

/// The worker-thread workflow engine service.
pub struct WorkerThreadWorkflowEngine {
    context: Context,
    subagents: Arc<SubagentRuntime>,
    config: Config,
}

impl WorkerThreadWorkflowEngine {
    /// Constructs the engine service.
    ///
    /// # Errors
    ///
    /// Returns when the subagents service is not mounted.
    pub fn new(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let subagents = context
            .get(SUBAGENTS)
            .ok_or_else(|| anyhow::anyhow!("workflow requires subagents"))?;
        Ok(Arc::new(Self {
            context: context.clone(),
            subagents,
            config,
        }))
    }
}

impl WorkflowEngine for WorkerThreadWorkflowEngine {
    fn start(&self, request: WorkflowStartRequest) -> Arc<dyn WorkflowRun> {
        let meta = validate_meta(&request.meta).expect("invalid workflow meta");
        assert_body_parses(&request.script, &meta.name);
        let provider = resolve_subagent_provider(
            &self.context,
            &self.config.provider,
            request.subagent_provider.as_deref(),
        );
        let max_total_agents =
            resolve_max_total_agents(request.max_total_agents, self.config.max_total_agents);
        let id = WorkflowRunId::new(Uuid::new_v4().to_string());
        let info = WorkflowRunInfo {
            id: id.clone(),
            meta: meta.clone(),
        };
        let limits = WorkerLimits {
            max_concurrent_agents: resolve_max_concurrent(self.config.max_concurrent_agents),
            max_total_agents,
            max_items_per_call: self.config.max_items_per_call,
            sync_timeout_ms: self.config.sync_timeout_ms,
        };
        let _init = WorkerInit {
            meta: meta.clone(),
            body: request.script.clone(),
            args: request.args.clone(),
            limits: limits.clone(),
        };
        let run = WorkerRun::new(
            &self.context,
            Arc::clone(&self.subagents),
            id,
            meta,
            request.parent,
            request.script,
            request.args,
            limits,
            provider,
            info.clone(),
        );

        let _ = emit_workflow_event(
            &self.context,
            seekdeep_workflow::WorkflowEventName::Start,
            &seekdeep_cordis::EventArgs::one(info.clone()),
        );

        let context = self.context.clone();
        let run_end = Arc::clone(&run);
        tokio::spawn(async move {
            let settled = run_end.result().await;
            let _ = emit_workflow_event(
                &context,
                seekdeep_workflow::WorkflowEventName::End,
                &seekdeep_cordis::EventArgs::from_values(vec![
                    Arc::new(info),
                    Arc::new(WorkflowResultInfo {
                        stop_reason: settled.stop_reason,
                        error: settled.error,
                        agents_started: settled.agents_started,
                    }),
                ]),
            );
        });

        run
    }
}

/// Builds the loader-compatible workflow-worker-thread plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            let engine = WorkerThreadWorkflowEngine::new(&context, config)?;
            WorkflowEngineService::new(engine).provide(&context)?;
            Ok(())
        })
    })
}
