//! Default model selection for agents without a session-specific selection.

use std::sync::Arc;

use seekdeep_agent::ModelSelection;
use seekdeep_cordis::{Context, Plugin, PluginFiber, ServiceKey};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{ModelId, ProviderId, ReasoningEffortId};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{
    SETTINGS, SettingsNamespace, SettingsSectionSource, install_settings_section,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "agent-default-model";
/// This service can run without an optional settings provider.
pub const INJECT: &[&str] = &[];
/// Typed Cordis service slot corresponding to `ctx.agentDefaultModel`.
pub const AGENT_DEFAULT_MODEL: ServiceKey<AgentDefaultModel> = ServiceKey::new("agentDefaultModel");

/// Composition entry for the default model selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaultModelConfig {
    /// Registered provider route.
    pub provider: ProviderId,
    /// Provider-owned model identifier.
    pub model: ModelId,
}

/// Stored and composed default-model settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaultModelSettings {
    /// Registered provider route.
    pub provider: ProviderId,
    /// Provider-owned model identifier.
    pub model: ModelId,
    /// Adapter-owned reasoning effort, or provider/default behavior when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl From<AgentDefaultModelConfig> for AgentDefaultModelSettings {
    fn from(config: AgentDefaultModelConfig) -> Self {
        Self {
            provider: config.provider,
            model: config.model,
            reasoning_effort: None,
        }
    }
}

/// Returns the stable settings namespace owned by this package.
#[must_use]
pub fn settings_namespace_id() -> SettingsNamespace {
    SettingsNamespace::new("agent-default-model")
}

/// Returns the source-compatible schema for stored default-model settings.
#[must_use]
pub fn settings_schema() -> Schema {
    Schema::object([
        ("provider", Schema::string().required()),
        ("model", Schema::string().required()),
        ("reasoningEffort", Schema::string()),
    ])
}

/// Live default-model service with optional settings layering.
pub struct AgentDefaultModel {
    context: Context,
    source: SettingsSectionSource,
    _settings_fiber: Arc<PluginFiber>,
}

impl std::fmt::Debug for AgentDefaultModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentDefaultModel")
            .field("current_selection", &self.current_selection())
            .finish_non_exhaustive()
    }
}

impl AgentDefaultModel {
    async fn open(context: &Context, config: AgentDefaultModelConfig) -> anyhow::Result<Arc<Self>> {
        let entry = AgentDefaultModelSettings::from(config);
        let installed = install_settings_section(
            context,
            &settings_namespace_id(),
            settings_schema(),
            serde_json::to_value(entry)?,
            None,
            Arc::new(|| Ok(())),
        )?;
        installed.fiber.await_settled().await?;
        Ok(Arc::new(Self {
            context: context.clone(),
            source: installed.source,
            _settings_fiber: installed.fiber,
        }))
    }

    /// Reads a detached current default model selection.
    ///
    /// # Panics
    ///
    /// Panics only if the settings subsystem violates its contract by exposing
    /// a value that did not pass this package's registered schema.
    #[must_use]
    pub fn current_selection(&self) -> ModelSelection {
        let settings: AgentDefaultModelSettings = serde_json::from_value(self.source.get())
            .expect("settings schema keeps the selected default model well formed");
        ModelSelection {
            provider: settings.provider,
            model: settings.model,
            reasoning_effort: settings.reasoning_effort.map(ReasoningEffortId::new),
        }
    }

    /// Saves the complete selection when a settings provider is mounted.
    ///
    /// A deployment without a provider deliberately retains its composition
    /// entry and treats this operation as a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns validation, persistence, or settings lifecycle failures.
    pub async fn save_selection(&self, next: &ModelSelection) -> anyhow::Result<()> {
        let Some(settings) = self.context.get(SETTINGS) else {
            return Ok(());
        };
        let mut section = serde_json::Map::from_iter([
            (
                "provider".to_owned(),
                Value::String(next.provider.to_string()),
            ),
            ("model".to_owned(), Value::String(next.model.to_string())),
        ]);
        if let Some(effort) = &next.reasoning_effort {
            section.insert(
                "reasoningEffort".to_owned(),
                Value::String(effort.as_str().to_owned()),
            );
        }
        settings
            .replace(&settings_namespace_id(), Value::Object(section), None)
            .await
    }
}

/// Builds the loader-compatible default-model plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: AgentDefaultModelConfig = serde_json::from_value(config)?;
            let service = AgentDefaultModel::open(&context, config).await?;
            context.provide(AGENT_DEFAULT_MODEL, service)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: AgentDefaultModelConfig = serde_json::from_value(value.clone())?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the service as a lifecycle-owned plugin fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install(
    context: &Context,
    config: AgentDefaultModelConfig,
) -> anyhow::Result<Arc<PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

/// Registers the package's intentionally empty invariant companion.
///
/// Settings validation owns the only mutable relationship observed by this
/// service.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-agent-default-model", InvariantInstaller::noop())
}
