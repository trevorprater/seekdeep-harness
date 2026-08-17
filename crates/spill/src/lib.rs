//! Storage seam for oversized model-facing text artifacts.

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::CallId;
use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Opaque model-facing handle for one spilled artifact.
    pub struct SpillLocator;
);

/// Save-time storage namespace for a spilled artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpillOwner {
    /// Session that owns newly produced storage.
    pub session_id: SessionId,
}

/// Descriptive tool and call that produced one spilled artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpillSource {
    /// Tool whose result was spilled.
    pub tool_name: String,
    /// Model-issued call identity.
    pub call_id: CallId,
    /// Short human label for the artifact.
    pub label: String,
}

/// One request to persist complete text to a spill artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveTextSpill {
    /// Save-time namespace.
    pub owner: SpillOwner,
    /// Descriptive producer fields.
    pub source: SpillSource,
    /// Caller-suggested base name; a backend treats it as a hint, never a path.
    pub suggested_name: String,
    /// Full text to persist verbatim as UTF-8.
    pub content: String,
}

/// Saved spill artifact and model-facing retrieval guidance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpillRef {
    /// Opaque backend-produced locator.
    pub locator: SpillLocator,
    /// Exact UTF-8 byte length persisted.
    pub bytes: u64,
    /// Backend-specific model-facing retrieval guidance.
    pub retrieval_hint: String,
}

/// Backend contract for spill storage.
#[async_trait]
pub trait SpillBackend: Send + Sync + 'static {
    /// Persists the full content or returns the real storage failure.
    async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef>;
}

/// Spill service exposed through `ctx.spillStore`.
#[derive(Clone)]
pub struct SpillStore {
    backend: Arc<dyn SpillBackend>,
}

impl fmt::Debug for SpillStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SpillStore").finish_non_exhaustive()
    }
}

impl SpillStore {
    /// Wraps one backend implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn SpillBackend>) -> Self {
        Self { backend }
    }

    /// Persists the full content and returns its backend reference.
    ///
    /// # Errors
    ///
    /// Returns the backend's storage failure unchanged.
    pub async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef> {
        self.backend.save_text(input).await
    }

    /// Provides this store on the `spillStore` service slot for the current fiber.
    ///
    /// # Errors
    ///
    /// Returns standard Cordis duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(SPILL_STORE, self.clone())
    }
}

/// Typed Cordis service slot corresponding to `ctx.spillStore`.
pub const SPILL_STORE: ServiceKey<SpillStore> = ServiceKey::new("spillStore");

/// Registers the spill seam's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-spill", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;

    #[derive(Default)]
    struct StubStore {
        last: Mutex<Option<SaveTextSpill>>,
    }

    #[async_trait]
    impl SpillBackend for StubStore {
        async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef> {
            *self.last.lock() = Some(input.clone());
            Ok(SpillRef {
                locator: SpillLocator::new(format!("/stub/{}", input.suggested_name)),
                bytes: input.content.len() as u64,
                retrieval_hint: "Use the stub reader.".to_owned(),
            })
        }
    }

    fn request(content: &str) -> SaveTextSpill {
        SaveTextSpill {
            owner: SpillOwner {
                session_id: SessionId::new("s1"),
            },
            source: SpillSource {
                tool_name: "web_fetch".to_owned(),
                call_id: CallId::new("c1"),
                label: "result".to_owned(),
            },
            suggested_name: "web_fetch.txt".to_owned(),
            content: content.to_owned(),
        }
    }

    #[tokio::test]
    async fn service_registers_delegates_rejects_duplicates_and_disposes() {
        let context = Context::new();
        let backend = Arc::new(StubStore::default());
        let store = Arc::new(SpillStore::new(backend.clone()));
        let effect = store.provide(&context).unwrap();
        let result = context
            .get(SPILL_STORE)
            .unwrap()
            .save_text(request("héllo"))
            .await
            .unwrap();
        assert_eq!(result.locator.as_str(), "/stub/web_fetch.txt");
        assert_eq!(result.bytes, "héllo".len() as u64);
        assert_eq!(backend.last.lock().as_ref().unwrap().content, "héllo");

        let second = Arc::new(SpillStore::new(Arc::new(StubStore::default())));
        assert!(second.provide(&context).is_err());
        effect.dispose().await.unwrap();
        assert!(context.get(SPILL_STORE).is_none());
    }

    #[test]
    fn wire_shape_is_exact_and_invariant_reserves_renamed_package() {
        let request = request("body");
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "owner": { "sessionId": "s1" },
                "source": { "toolName": "web_fetch", "callId": "c1", "label": "result" },
                "suggestedName": "web_fetch.txt",
                "content": "body"
            })
        );
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap(),
        );
        let _registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-spill"));
    }
}
