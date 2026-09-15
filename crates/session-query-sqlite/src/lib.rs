//! `SQLite` full-text provider for the session-query service.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_session_query::{SessionQueryEngine, SessionQueryService};
use serde_json::Value;

pub mod engine;
pub mod query;
pub mod schema;

pub use engine::{
    OpenAt, ResolvedSqliteSessionQueryConfig, SESSION_QUERY_SQLITE_DEFAULT_LIMIT,
    SESSION_QUERY_SQLITE_MAX_LIMIT, SESSION_QUERY_SQLITE_SNIPPET_CHARS, SqliteSessionQueryConfig,
    SqliteSessionQueryEngine,
};

/// Cordis plugin name.
pub const NAME: &str = "session-query-sqlite";
/// Live sessions are required by the combined query service.
pub const INJECT: &[&str] = &["sessions"];
/// Boot-context slot for a launcher-owned derived-index path.
pub const SESSION_QUERY_SQLITE_PATH_KEY: &str = "launcherSessionQueryPath";

/// Builds the source-compatible `SQLite` session-query plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: SqliteSessionQueryConfig = serde_json::from_value(config)?;
            let engine = SqliteSessionQueryEngine::new(&context, config)?;
            let erased: Arc<dyn SessionQueryEngine> = engine.clone();
            SessionQueryService::new(erased).provide(&context)?;
            let closing = engine.clone();
            context.own(EffectHandle::new("session-query-sqlite.close", move || {
                Box::pin(async move {
                    closing.close().await;
                    Ok(())
                })
            }))?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: SqliteSessionQueryConfig = serde_json::from_value(value.clone())?;
        config.validate()?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the `SQLite` session-query plugin and returns its lifecycle fiber.
///
/// # Errors
///
/// Returns inactive-context, dependency, configuration, or eager-open failures.
pub fn install(
    context: &Context,
    config: SqliteSessionQueryConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
