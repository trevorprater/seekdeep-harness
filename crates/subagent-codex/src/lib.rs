//! Fixed one-shot provider backed by the official `codex app-server --stdio` process.

mod run;
mod wire;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SUBAGENTS, SubagentCapabilities, SubagentProvider, SubagentRun,
    assert_positive_finite, no_start_capabilities, resolve_child_cwd,
};
use seekdeep_subprocess::{SUBPROCESS, SubprocessEnvironment, SubprocessService};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use run::{
    CodexRunErrorObserver, CodexRunSpec, DEFAULT_DISPOSE_GRACE_MS, codex_app_server_argv,
    dispose_codex_child, start_codex_run, text_task,
};
pub use wire::CodexAppServerWire;

/// Loader plugin name.
pub const NAME: &str = "subagent-codex";
/// Required capability services.
pub const INJECT: &[&str] = &["subagents", "subprocess"];

/// Deployment-owned environment and process-release bound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Explicit environment layered over the credential-scrubbed parent environment.
    pub env: BTreeMap<String, String>,
    /// Grace in milliseconds for app-server process-tree termination.
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

struct CodexProvider {
    subprocess: Arc<SubprocessService>,
    config: Config,
    capabilities: SubagentCapabilities,
}

#[async_trait]
impl SubagentProvider for CodexProvider {
    fn name(&self) -> &'static str {
        "codex"
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
                "subagent-codex: no working directory for the child — delegate from a parent session that has one"
            );
        };
        let cwd = resolve_child_cwd("subagent-codex", None, Some(&parent_cwd))?;
        let env: SubprocessEnvironment = self
            .config
            .env
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();
        start_codex_run(
            request.request,
            CodexRunSpec {
                cwd,
                env,
                dispose_grace_ms: self.config.dispose_grace_ms,
                subprocess: Arc::clone(&self.subprocess),
                on_error: Some(Arc::new(|error, stop_reason| {
                    tracing::warn!(
                        stop_reason = stop_reason.as_str(),
                        error = %error,
                        "subagent-codex: child run failed"
                    );
                })),
            },
        )
        .await
    }
}

/// Registers the fixed `codex` provider for this plugin lifecycle.
///
/// # Errors
///
/// Returns invalid timing, missing-service, duplicate-provider, or ownership failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    assert_positive_finite("subagent-codex", "disposeGraceMs", config.dispose_grace_ms)?;
    anyhow::ensure!(
        config.dispose_grace_ms <= MAX_TIMER_DELAY_MS,
        "subagent-codex: disposeGraceMs must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("subagent-codex requires subagents"))?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("subagent-codex requires subprocess"))?;
    let provider: Arc<dyn SubagentProvider> = Arc::new(CodexProvider {
        subprocess,
        config,
        capabilities: no_start_capabilities(),
    });
    let effect = subagents.register_provider(provider)?;
    context.own(effect)?;
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = if value.is_null() {
        Config::default()
    } else {
        serde_json::from_value(value.clone())?
    };
    assert_positive_finite("subagent-codex", "disposeGraceMs", config.dispose_grace_ms)?;
    anyhow::ensure!(
        config.dispose_grace_ms <= MAX_TIMER_DELAY_MS,
        "subagent-codex: disposeGraceMs must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible provider plugin.
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
