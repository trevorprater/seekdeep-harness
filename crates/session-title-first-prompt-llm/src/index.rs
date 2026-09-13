//! All-human-messages model provider for the session-title service.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_session_title::SessionTitleAutomaticMode;
use seekdeep_session_title_llm::{SessionTitleLlmConfig, register_session_title_llm_provider};
use serde_json::Value;

/// Cordis plugin name.
pub const NAME: &str = "session-title-first-prompt-llm";

/// Services required by this provider plugin.
pub const INJECT: &[&str] = &["sessionTitle", "llm", "sessions"];

/// The source-compatible admission schema for `SessionTitleLlmConfig`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> seekdeep_schemastery::Schema {
    seekdeep_session_title_llm::config_schema()
}

/// Registers this provider through the shared configuration and call policy.
///
/// # Errors
///
/// Returns missing-service, invalid-config, or duplicate-provider failures.
pub fn apply(ctx: &Context, config: &SessionTitleLlmConfig) -> anyhow::Result<()> {
    register_session_title_llm_provider(
        ctx,
        config,
        NAME,
        SessionTitleAutomaticMode::FirstPrompt,
        Arc::new(|messages| {
            let first = messages.first().ok_or_else(|| {
                anyhow::anyhow!("first-prompt title provider requires one human message")
            })?;
            Ok(vec![first.clone()])
        }),
    )
}

/// Builds the loader-compatible provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: SessionTitleLlmConfig = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
