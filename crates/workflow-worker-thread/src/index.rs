//! Worker-thread workflow engine: each run executes its model-written script
//! in an escapable context on a fresh worker and bridges `agent()` calls to host
//! subagents.

use std::{path::PathBuf, sync::Arc};

use boa_engine::{Source, context::ContextBuilder, script::Script};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{SUBAGENTS, SubagentRuntime};
use seekdeep_workflow::{
    WorkflowEngine, WorkflowError, WorkflowErrorCode, WorkflowResultInfo, WorkflowRun,
    WorkflowRunId, WorkflowRunInfo, WorkflowStartRequest, emit_workflow_event,
    index::WorkflowEngineService,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{host::WorkerRun, meta::validate_meta, process::WorkerCommand, types::WorkerLimits};

/// Cordis plugin name.
pub const NAME: &str = "workflow-worker-thread";
/// Services required by the engine.
pub const INJECT: &[&str] = &["subagents"];

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

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
fn assert_body_parses(body: &str, _name: &str) -> anyhow::Result<()> {
    if has_meta_statement(body) {
        return Err(WorkflowError::new(
            "workflow meta rides the meta request field, not the script: remove the export const meta statement from the body",
            WorkflowErrorCode::ScriptParse,
        )
        .into());
    }
    let source = format!(
        "(async () => {{
{body}
}})()"
    );
    let mut context = ContextBuilder::new().build().map_err(|error| {
        WorkflowError::new(
            format!("workflow script parser could not initialize: {error}"),
            WorkflowErrorCode::ScriptParse,
        )
    })?;
    if let Err(error) = Script::parse(Source::from_bytes(&source), None, &mut context) {
        return Err(WorkflowError::new(
            format!("workflow script does not parse: {error}"),
            WorkflowErrorCode::ScriptParse,
        )
        .into());
    }
    Ok(())
}

/// Resolve one run's provider route before publishing work.
fn resolve_subagent_provider(
    context: &Context,
    configured: &str,
    override_provider: Option<&str>,
) -> anyhow::Result<String> {
    let provider = override_provider.unwrap_or(configured);
    if provider.is_empty() || provider != provider.trim() {
        return Err(WorkflowError::new(
            "workflow subagentProvider must be a non-empty normalized string",
            WorkflowErrorCode::InvalidArgument,
        )
        .into());
    }
    let subagents = context.get(SUBAGENTS).ok_or_else(|| {
        WorkflowError::new("workflow requires subagents", WorkflowErrorCode::AgentStart)
    })?;
    if subagents.get_provider(provider).is_none() {
        return Err(WorkflowError::new(
            format!("no subagent provider registered for \"{provider}\""),
            WorkflowErrorCode::AgentStart,
        )
        .into());
    }
    Ok(provider.to_owned())
}

/// Resolve one run's total-child cap against the engine deployment ceiling.
fn resolve_max_total_agents(requested: Option<u64>, ceiling: u64) -> anyhow::Result<u64> {
    let Some(requested) = requested else {
        return Ok(ceiling);
    };
    if !(1..=MAX_SAFE_INTEGER).contains(&requested) {
        return Err(WorkflowError::new(
            "workflow maxTotalAgents must be a positive safe integer",
            WorkflowErrorCode::InvalidArgument,
        )
        .into());
    }
    if requested > ceiling {
        return Err(WorkflowError::new(
            format!("workflow maxTotalAgents {requested} exceeds the engine ceiling {ceiling}"),
            WorkflowErrorCode::InvalidArgument,
        )
        .into());
    }
    Ok(requested)
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
    worker: Option<WorkerCommand>,
}

impl WorkerThreadWorkflowEngine {
    /// Constructs the engine service.
    ///
    /// # Errors
    ///
    /// Returns for invalid limits or when the subagents service is not mounted.
    pub fn new(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        Self::new_with_optional_worker(context, config, discover_worker()?)
    }

    /// Constructs an engine pinned to one compiled worker executable.
    ///
    /// # Errors
    ///
    /// Returns for an absent executable, invalid limits, or missing services.
    pub fn new_with_worker_path(
        context: &Context,
        config: Config,
        worker_path: impl Into<PathBuf>,
    ) -> anyhow::Result<Arc<Self>> {
        let worker_path = worker_path.into();
        anyhow::ensure!(
            worker_path.is_file(),
            "workflow worker executable does not exist: {}",
            worker_path.display()
        );
        Self::new_with_optional_worker(
            context,
            config,
            Some(WorkerCommand {
                path: worker_path,
                integrated: false,
            }),
        )
    }

    fn new_with_optional_worker(
        context: &Context,
        config: Config,
        worker: Option<WorkerCommand>,
    ) -> anyhow::Result<Arc<Self>> {
        validate_config(&config)?;
        let subagents = context
            .get(SUBAGENTS)
            .ok_or_else(|| anyhow::anyhow!("workflow requires subagents"))?;
        Ok(Arc::new(Self {
            context: context.clone(),
            subagents,
            config,
            worker,
        }))
    }
}

fn worker_executable_name() -> &'static str {
    if cfg!(windows) {
        "seekdeep-workflow-worker.exe"
    } else {
        "seekdeep-workflow-worker"
    }
}

fn discover_worker() -> anyhow::Result<Option<WorkerCommand>> {
    let current = std::env::current_exe()?;
    let mut directories = Vec::new();
    if let Some(directory) = current.parent() {
        directories.push(directory.to_owned());
        if directory.file_name().is_some_and(|name| name == "deps")
            && let Some(profile) = directory.parent()
        {
            directories.push(profile.to_owned());
        }
    }
    let found = directories
        .into_iter()
        .map(|directory| directory.join(worker_executable_name()))
        .find(|candidate| candidate.is_file());
    if let Some(path) = found {
        return Ok(Some(WorkerCommand {
            path,
            integrated: false,
        }));
    }
    if current
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "seekdeep")
    {
        return Ok(Some(WorkerCommand {
            path: current,
            integrated: true,
        }));
    }
    if !cfg!(debug_assertions) {
        anyhow::bail!(
            "workflow worker executable {} is missing beside the harness",
            worker_executable_name()
        );
    }
    Ok(None)
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    for (name, value, allow_zero) in [
        ("maxConcurrentAgents", config.max_concurrent_agents, true),
        ("maxTotalAgents", config.max_total_agents, false),
        ("maxItemsPerCall", config.max_items_per_call, false),
        ("syncTimeoutMs", config.sync_timeout_ms, false),
        ("disposeGraceMs", config.dispose_grace_ms, true),
    ] {
        anyhow::ensure!(
            value <= MAX_SAFE_INTEGER && (allow_zero || value > 0),
            "{name} must be {} safe integer",
            if allow_zero {
                "a non-negative"
            } else {
                "a positive"
            }
        );
    }
    Ok(())
}

impl WorkflowEngine for WorkerThreadWorkflowEngine {
    fn start(&self, request: WorkflowStartRequest) -> anyhow::Result<Arc<dyn WorkflowRun>> {
        let meta = validate_meta(&request.meta)?;
        assert_body_parses(&request.script, &meta.name)?;
        let provider = resolve_subagent_provider(
            &self.context,
            &self.config.provider,
            request.subagent_provider.as_deref(),
        )?;
        let max_total_agents =
            resolve_max_total_agents(request.max_total_agents, self.config.max_total_agents)?;
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
        let input_signal = request.signal;
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
            self.config.dispose_grace_ms,
            self.worker.as_ref(),
        )?;

        let _ = emit_workflow_event(
            &self.context,
            seekdeep_workflow::WorkflowEventName::Start,
            &seekdeep_cordis::EventArgs::one(info.clone()),
        );
        run.launch(input_signal);

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

        Ok(run)
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
