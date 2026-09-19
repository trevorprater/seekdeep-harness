//! Fresh-process full Harness runtime provider driven through the Rust SDK client.

mod run;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SUBAGENTS, SubagentCapabilities, SubagentProvider, SubagentRun,
    assert_positive_finite, no_start_capabilities, resolve_child_cwd, validate_configured_cwd,
};
use seekdeep_subprocess::scrubbed_parent_env;
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use run::{
    DEFAULT_DISPOSE_EOF_GRACE_MS, DEFAULT_DISPOSE_GRACE_MS, DEFAULT_SHUTDOWN_TIMEOUT_MS,
    SdkRunErrorObserver, SdkRunSpec, sdk_stop_reason, start_sdk_run,
};

/// Loader plugin name.
pub const NAME: &str = "subagent-seekdeep-sdk";
/// Only the shared subagent registry is required.
pub const INJECT: &[&str] = &["subagents"];

/// Full child runtime launch and route configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Provider name exposed on the subagent registry.
    pub provider_name: String,
    /// Runtime executable.
    pub command: String,
    /// Runtime arguments.
    pub args: Vec<String>,
    /// Optional load-time-resolved working-directory override.
    pub cwd: Option<String>,
    /// Child runtime provider route.
    pub provider: String,
    /// Child runtime model.
    pub model: String,
    /// Optional child output-token cap.
    pub max_tokens: Option<u64>,
    /// Explicit child environment layered over the shared scrub.
    pub env: BTreeMap<String, String>,
    /// Protocol shutdown bound.
    pub shutdown_timeout_ms: f64,
    /// Cooperative EOF quiescence grace.
    pub dispose_eof_grace_ms: f64,
    /// Termination confirmation grace.
    pub dispose_grace_ms: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider_name: "seekdeep-sdk".to_owned(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            provider: "deepseek-official".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_tokens: None,
            env: BTreeMap::new(),
            shutdown_timeout_ms: DEFAULT_SHUTDOWN_TIMEOUT_MS,
            dispose_eof_grace_ms: DEFAULT_DISPOSE_EOF_GRACE_MS,
            dispose_grace_ms: DEFAULT_DISPOSE_GRACE_MS,
        }
    }
}

struct SdkProvider {
    name: String,
    config: Config,
    capabilities: SubagentCapabilities,
}

#[async_trait]
impl SubagentProvider for SdkProvider {
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
            "subagent-seekdeep-sdk",
            self.config.cwd.as_deref(),
            request.request.parent.session().header().cwd.as_deref(),
        )?;
        let mut env = scrubbed_parent_env()
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        env.extend(self.config.env.clone());
        start_sdk_run(
            request.request,
            SdkRunSpec {
                command: self.config.command.clone(),
                args: self.config.args.clone(),
                cwd,
                provider: self.config.provider.clone(),
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                env,
                shutdown_timeout_ms: self.config.shutdown_timeout_ms,
                dispose_eof_grace_ms: self.config.dispose_eof_grace_ms,
                dispose_grace_ms: self.config.dispose_grace_ms,
                on_error: Some(Arc::new(|error, reason| {
                    tracing::warn!(stop_reason = reason.as_str(), error = %error, "subagent SDK child failed");
                })),
            },
        )
        .await
    }
}

/// Validates config and registers one out-of-process SDK provider.
///
/// # Errors
///
/// Returns config, cwd, missing-service, duplicate-provider, or ownership failures.
pub fn apply(context: &Context, mut config: Config) -> anyhow::Result<()> {
    validate_config(&config)?;
    config.cwd = validate_configured_cwd("subagent-seekdeep-sdk", config.cwd.as_deref())?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("subagent-seekdeep-sdk requires subagents"))?;
    let provider: Arc<dyn SubagentProvider> = Arc::new(SdkProvider {
        name: config.provider_name.clone(),
        config,
        capabilities: no_start_capabilities(),
    });
    let effect = subagents.register_provider(provider)?;
    context.own(effect)?;
    Ok(())
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.provider_name.is_empty(),
        "subagent-seekdeep-sdk providerName must not be empty"
    );
    anyhow::ensure!(
        !config.command.is_empty(),
        "subagent-seekdeep-sdk command is required"
    );
    if let Some(max_tokens) = config.max_tokens {
        anyhow::ensure!(
            (1..=9_007_199_254_740_991).contains(&max_tokens),
            "subagent-seekdeep-sdk maxTokens must be a positive safe integer"
        );
    }
    assert_positive_finite(
        "subagent-seekdeep-sdk",
        "shutdownTimeoutMs",
        config.shutdown_timeout_ms,
    )?;
    assert_positive_finite(
        "subagent-seekdeep-sdk",
        "disposeEofGraceMs",
        config.dispose_eof_grace_ms,
    )?;
    assert_positive_finite(
        "subagent-seekdeep-sdk",
        "disposeGraceMs",
        config.dispose_grace_ms,
    )?;
    anyhow::ensure!(
        [
            config.shutdown_timeout_ms,
            config.dispose_eof_grace_ms,
            config.dispose_grace_ms,
        ]
        .into_iter()
        .all(|value| value <= MAX_TIMER_DELAY_MS),
        "subagent-seekdeep-sdk timing bounds must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config: Config = serde_json::from_value(value.clone())?;
    validate_config(&config)?;
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
