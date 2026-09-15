//! Out-of-process ACP subagent provider.

mod run;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use seekdeep_acp::PermissionPolicy;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SUBAGENTS, SubagentCapabilities, SubagentProvider, SubagentRun,
    assert_positive_finite, no_start_capabilities, resolve_child_cwd, validate_configured_cwd,
};
use seekdeep_subprocess::{SUBPROCESS, SubprocessService};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use run::{
    AcpRunErrorObserver, AcpRunSpec, DEFAULT_DISPOSE_EOF_GRACE_MS, DEFAULT_DISPOSE_GRACE_MS,
    dispose_acp_child, start_acp_run,
};

/// Loader plugin name.
pub const NAME: &str = "subagent-acp";
/// Required services.
pub const INJECT: &[&str] = &["subagents", "subprocess"];

/// Child process, workspace, permission, environment, and teardown configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Provider registry name.
    pub provider_name: String,
    /// Executable.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Optional load-time-resolved workspace override.
    pub cwd: Option<String>,
    /// Automatic permission policy.
    pub permission: PermissionPolicy,
    /// Explicit child environment.
    pub env: BTreeMap<String, String>,
    /// Cooperative EOF grace.
    pub dispose_eof_grace_ms: f64,
    /// Termination escalation grace.
    pub dispose_grace_ms: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider_name: "acp".to_owned(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            permission: PermissionPolicy::Reject,
            env: BTreeMap::new(),
            dispose_eof_grace_ms: DEFAULT_DISPOSE_EOF_GRACE_MS,
            dispose_grace_ms: DEFAULT_DISPOSE_GRACE_MS,
        }
    }
}

struct AcpProvider {
    name: String,
    subprocess: Arc<SubprocessService>,
    config: Config,
    capabilities: SubagentCapabilities,
}

#[async_trait]
impl SubagentProvider for AcpProvider {
    fn name(&self) -> &str {
        &self.name
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
        let cwd = resolve_child_cwd(
            "subagent-acp",
            self.config.cwd.as_deref(),
            request.request.parent.session().header().cwd.as_deref(),
        )?;
        start_acp_run(
            request.request,
            AcpRunSpec {
                command: self.config.command.clone(),
                args: self.config.args.clone(),
                cwd,
                permission: self.config.permission,
                env: self.config.env.clone(),
                dispose_eof_grace_ms: self.config.dispose_eof_grace_ms,
                dispose_grace_ms: self.config.dispose_grace_ms,
                subprocess: Arc::clone(&self.subprocess),
                on_error: Some(Arc::new(|error, reason| {
                    tracing::warn!(stop_reason = reason.as_str(), error = %error, "ACP child run failed");
                })),
            },
        )
        .await
    }
}

/// Validates configuration and registers one provider.
///
/// # Errors
///
/// Returns configuration, cwd, service, duplicate-provider, or ownership failures.
pub fn apply(context: &Context, mut config: Config) -> anyhow::Result<()> {
    validate_config(&config)?;
    config.cwd = validate_configured_cwd("subagent-acp", config.cwd.as_deref())?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("subagent-acp requires subagents"))?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("subagent-acp requires subprocess"))?;
    let provider: Arc<dyn SubagentProvider> = Arc::new(AcpProvider {
        name: config.provider_name.clone(),
        subprocess,
        config,
        capabilities: no_start_capabilities(),
    });
    context.own(subagents.register_provider(provider)?)?;
    Ok(())
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.provider_name.is_empty(),
        "subagent-acp providerName must not be empty"
    );
    anyhow::ensure!(
        !config.command.is_empty(),
        "subagent-acp command is required"
    );
    for (name, value) in [
        ("disposeEofGraceMs", config.dispose_eof_grace_ms),
        ("disposeGraceMs", config.dispose_grace_ms),
    ] {
        assert_positive_finite("subagent-acp", name, value)?;
        anyhow::ensure!(
            value <= MAX_TIMER_DELAY_MS,
            "subagent-acp: {name} must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        );
    }
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
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
