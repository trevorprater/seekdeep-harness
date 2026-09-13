//! Fixed one-shot Claude Code provider over the native stream-json CLI.

mod process;
mod run;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SUBAGENTS, SubagentCapabilities, SubagentProvider, SubagentRun,
    assert_positive_finite, no_start_capabilities, resolve_child_cwd,
};
use seekdeep_subprocess::{SUBPROCESS, SubprocessService};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use process::{
    CLAUDE_STREAM_ARGS, WINDOWS_BATCH_EXECUTABLE_ENV, claude_spawn_spec, prompt_frame,
};
pub use run::{
    ClaudeCodeRunErrorObserver, ClaudeCodeRunSpec, DEFAULT_DISPOSE_GRACE_MS,
    dispose_claude_code_child, start_claude_code_run, successful_result, text_task,
};

/// Loader plugin name.
pub const NAME: &str = "subagent-claude-code";
/// Required services.
pub const INJECT: &[&str] = &["subagents", "subprocess"];

/// Deployment-owned environment and process-release bound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Explicit environment layered over credential-scrubbed ambient values.
    pub env: BTreeMap<String, String>,
    /// Managed process-tree termination grace.
    pub dispose_grace_ms: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            dispose_grace_ms: DEFAULT_DISPOSE_GRACE_MS,
        }
    }
}

struct ClaudeCodeProvider {
    subprocess: Arc<SubprocessService>,
    config: Config,
    capabilities: SubagentCapabilities,
}

#[async_trait]
impl SubagentProvider for ClaudeCodeProvider {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        let parent_cwd = request.request.parent.session().header().cwd.clone();
        let Some(parent_cwd) = parent_cwd else {
            anyhow::bail!(
                "subagent-claude-code: no working directory for the child — delegate from a parent session that has one"
            );
        };
        let cwd = resolve_child_cwd("subagent-claude-code", None, Some(&parent_cwd))?;
        let executable = self
            .subprocess
            .resolve_executable(
                "claude",
                Some(&self.config.env),
                Some(request.request.signal.clone()),
            )
            .await?;
        start_claude_code_run(
            request.request,
            ClaudeCodeRunSpec {
                cwd,
                executable,
                env: self.config.env.clone(),
                dispose_grace_ms: self.config.dispose_grace_ms,
                subprocess: Arc::clone(&self.subprocess),
                on_error: Some(Arc::new(|error, reason| {
                    tracing::warn!(stop_reason = reason.as_str(), error = %error, "Claude Code child run failed");
                })),
            },
        )
        .await
    }
}

/// Validates configuration and registers the fixed provider.
///
/// # Errors
///
/// Returns timing, missing-service, duplicate-provider, or ownership failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    validate_config(&config)?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("subagent-claude-code requires subagents"))?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("subagent-claude-code requires subprocess"))?;
    let provider: Arc<dyn SubagentProvider> = Arc::new(ClaudeCodeProvider {
        subprocess,
        config,
        capabilities: no_start_capabilities(),
    });
    context.own(subagents.register_provider(provider)?)?;
    Ok(())
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    assert_positive_finite(
        "subagent-claude-code",
        "disposeGraceMs",
        config.dispose_grace_ms,
    )?;
    anyhow::ensure!(
        config.dispose_grace_ms <= MAX_TIMER_DELAY_MS,
        "subagent-claude-code: disposeGraceMs must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(serde_json::to_value(Config::default())?);
    }
    let config: Config = serde_json::from_value(value.clone())?;
    validate_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible namespace plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, config)
        })
    })
    .with_config_validator(normalize_config)
}
