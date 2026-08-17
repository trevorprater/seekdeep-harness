//! Per-session JSONL persistence backend.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_session_persistence::{
    MAX_WRITE_BATCH_DELAY_MS, SessionPersistence, SessionPersistenceService,
};
use serde_json::Value;

/// Durable filesystem backend.
pub mod backend;
/// Artifact format and path helpers.
pub mod format;
/// Package-owned invariant companion.
pub mod invariant;
/// Concatenated checksummed Zstandard frame primitives.
pub mod zstd;

pub use backend::{JsonlConfig, JsonlSessionPersistence};
pub use invariant::{INVARIANT_NAME, register_invariant};

pub use format::{
    JsonlCompression, SessionLogScan, encode_segment, encode_segment_units, event_lines,
    header_line, log_path, parse_header_meta, project_dir, project_key, scan_log, session_dir,
};
pub use zstd::{ZstdFrameRange, ZstdFrameScan, scan_zstd_frames};

/// Cordis plugin name.
pub const NAME: &str = "session-persistence-jsonl";
/// Live Session ownership is required for write-path installation.
pub const INJECT: &[&str] = &["sessions"];

/// Builds the source-compatible JSONL Session persistence service plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: JsonlConfig = serde_json::from_value(config)?;
            let sessions = context.get(SESSIONS).ok_or_else(|| {
                anyhow::anyhow!("session-persistence-jsonl lost required sessions service")
            })?;
            let backend = JsonlSessionPersistence::build(sessions, config)?;
            let erased: Arc<dyn SessionPersistence> = backend.clone();
            SessionPersistenceService::new(erased).provide(&context)?;
            backend.install_write_path(&context)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: JsonlConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(!config.root.as_os_str().is_empty(), "root is required");
        anyhow::ensure!(
            config.prepared_session_cache_size > 0,
            "preparedSessionCacheSize must be a positive integer"
        );
        anyhow::ensure!(
            (1..=MAX_WRITE_BATCH_DELAY_MS).contains(&config.write_batch_max_delay_ms),
            "writeBatchMaxDelayMs must be an integer between 1 and 2147483647"
        );
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the JSONL plugin and returns its typed lifecycle fiber.
///
/// # Errors
///
/// Returns inactive-context failures.
pub fn install(
    context: &Context,
    config: JsonlConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

#[cfg(test)]
mod plugin_tests {
    use seekdeep_core::{
        session::{AppendOptions, SessionId},
        session_store::{CreateSessionOptions, SessionStore},
    };
    use seekdeep_session_persistence::SESSION_PERSISTENCE;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn plugin_publishes_drains_and_withdraws_the_service() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let mounted = install(
            &context,
            JsonlConfig {
                root: temporary.path().to_owned(),
                pack_chunks: true,
                compression: JsonlCompression::None,
                write_batch_max_delay_ms: 60_000,
                prepared_session_cache_size: 5,
            },
        )
        .expect("plugin");
        mounted.await_settled().await.expect("active");
        let persistence = context.get(SESSION_PERSISTENCE).expect("service");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("plugin-drain")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("turn end");
        mounted.dispose().await.expect("dispose and drain");
        assert!(context.get(SESSION_PERSISTENCE).is_none());
        assert_eq!(
            persistence
                .persistence()
                .inspect(session.id(), None)
                .await
                .expect("durable after dispose")
                .events,
            session.events()
        );
    }
}
