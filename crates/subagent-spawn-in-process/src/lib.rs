//! Fresh-session in-process subagent provider.

use std::sync::Arc;

use async_trait::async_trait;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_subagent::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest, SUBAGENTS,
    SubagentCapabilities, SubagentProvider, SubagentRun,
};
use seekdeep_subagent_in_process_driver::{InProcessRunOptions, start_in_process_run};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Loader plugin name.
pub const NAME: &str = "subagent-spawn-in-process";
/// The provider depends only on the subagent registry.
pub const INJECT: &[&str] = &["subagents"];

/// Provider registry name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Name exposed through the subagent provider registry.
    pub provider_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider_name: "spawn".to_owned(),
        }
    }
}

struct SpawnInProcessProvider {
    name: String,
    capabilities: SubagentCapabilities,
}

impl SpawnInProcessProvider {
    fn new(name: String) -> Arc<Self> {
        Arc::new(Self {
            name,
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
        })
    }
}

#[async_trait]
impl SubagentProvider for SpawnInProcessProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    fn supports_continuable(&self) -> bool {
        true
    }

    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        start_in_process_run(request, InProcessRunOptions::default()).await
    }

    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        Ok(ContinuableCreateSpec::default())
    }
}

/// Registers one fresh-session provider for this plugin lifecycle.
///
/// # Errors
///
/// Returns missing-service, duplicate-provider, or lifecycle ownership failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("subagent-spawn-in-process requires subagents"))?;
    let provider: Arc<dyn SubagentProvider> = SpawnInProcessProvider::new(config.provider_name);
    let effect = subagents.register_provider(provider)?;
    context.own(effect)?;
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(serde_json::to_value(Config::default())?);
    }
    let config: Config = serde_json::from_value(value.clone())?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible spawn provider plugin.
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
