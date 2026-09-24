//! Dynamic settings integration parity for the `DeepSeek` provider plugin.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber};
use seekdeep_llm::{LLM, LlmRuntime};
use seekdeep_llm_deepseek::{DeepSeekCatalogModel, DeepSeekConfig, install};
use seekdeep_settings::{
    SETTINGS, SettingsDocument, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value, json};

struct MemorySettingsStorage {
    document: Mutex<SettingsDocument>,
}

impl MemorySettingsStorage {
    fn new(document: Value) -> Arc<Self> {
        let Value::Object(document) = document else {
            panic!("settings fixture document must be an object");
        };
        Arc::new(Self {
            document: Mutex::new(document),
        })
    }
}

#[async_trait]
impl SettingsStorage for MemorySettingsStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &seekdeep_settings::SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.document
            .lock()
            .insert(namespace.to_string(), Value::Object(section.clone()));
        Ok(())
    }
}

struct Harness {
    context: Context,
    runtime: Arc<LlmRuntime>,
    settings_fiber: Arc<Fiber>,
    provider_fiber: Arc<seekdeep_cordis::PluginFiber>,
}

impl Harness {
    async fn boot(config: DeepSeekConfig) -> Self {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).unwrap();
        let settings_fiber = Fiber::active_child("settings-provider");
        SettingsService::install(
            &context.with_fiber(settings_fiber.clone()),
            MemorySettingsStorage::new(json!({})),
        )
        .await
        .unwrap();
        let provider_fiber = install(&context, config).unwrap();
        provider_fiber.await_settled().await.unwrap();
        Self {
            context,
            runtime,
            settings_fiber,
            provider_fiber,
        }
    }

    fn settings(&self) -> Arc<SettingsService> {
        self.context.get(SETTINGS).unwrap()
    }
}

fn model(id: &str) -> DeepSeekCatalogModel {
    DeepSeekCatalogModel {
        id: id.to_owned(),
        name: None,
        description: None,
        context_window: None,
        max_tokens: None,
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if predicate() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn catalog_is_live_and_resolver_invalid_snapshots_keep_the_last_good_generation() {
    let harness = Harness::boot(DeepSeekConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        ..DeepSeekConfig::default()
    })
    .await;
    let ns = settings_namespace("llm-deepseek").unwrap();
    assert_eq!(
        harness
            .runtime
            .list_models("deepseek-official")
            .await
            .unwrap()
            .len(),
        2
    );
    harness
        .settings()
        .update(
            &ns,
            json!({ "models": [{ "id": "settings-model", "name": "From Settings" }] }),
            None,
        )
        .await
        .unwrap();
    let catalog = harness
        .runtime
        .list_models("deepseek-official")
        .await
        .unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(
        (catalog[0].id.as_str(), catalog[0].name.as_str()),
        ("settings-model", "From Settings")
    );

    harness
        .settings()
        .update(
            &ns,
            json!({ "models": [{ "id": "dup" }, { "id": "dup" }] }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .runtime
            .list_models("deepseek-official")
            .await
            .unwrap()[0]
            .id
            .as_str(),
        "settings-model"
    );
    harness
        .settings()
        .update(&ns, json!({ "models": [{ "id": "recovered" }] }), None)
        .await
        .unwrap();
    assert_eq!(
        harness
            .runtime
            .list_models("deepseek-official")
            .await
            .unwrap()[0]
            .id
            .as_str(),
        "recovered"
    );
}

#[tokio::test]
async fn retry_policy_replaces_the_route_atomically_without_an_empty_registry_window() {
    let harness = Harness::boot(DeepSeekConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        ..DeepSeekConfig::default()
    })
    .await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    let runtime = harness.runtime.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "llm/adapters-updated",
            move |_, _| {
                capture.lock().push(
                    runtime
                        .list_providers()
                        .into_iter()
                        .map(|provider| provider.id.to_string())
                        .collect::<Vec<_>>(),
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    harness
        .settings()
        .update(
            &settings_namespace("llm-deepseek").unwrap(),
            json!({
                "retryPolicy": {
                    "mode": "always",
                    "backoff": {
                        "initialDelayMs": 25,
                        "maxDelayMs": 100,
                        "jitterRatio": 0.2
                    }
                }
            }),
            None,
        )
        .await
        .unwrap();
    wait_until(|| {
        harness
            .runtime
            .provider_retry_policy("deepseek-official")
            .is_ok_and(|policy| {
                policy.max_retries().is_none()
                    && (policy.initial_delay_ms() - 25.0).abs() <= f64::EPSILON
                    && (policy.max_delay_ms() - 100.0).abs() <= f64::EPSILON
                    && (policy.jitter_ratio() - 0.2).abs() <= f64::EPSILON
            })
    })
    .await;
    assert_eq!(
        harness.runtime.list_providers()[0].id.as_str(),
        "deepseek-official"
    );
    assert_eq!(*observed.lock(), vec![vec!["deepseek-official".to_owned()]]);
}

#[tokio::test]
async fn settings_detach_falls_back_to_the_composition_catalog() {
    let harness = Harness::boot(DeepSeekConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        models: Some(vec![model("entry-model")]),
        ..DeepSeekConfig::default()
    })
    .await;
    harness
        .settings()
        .update(
            &settings_namespace("llm-deepseek").unwrap(),
            json!({ "models": [{ "id": "user-model" }] }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .runtime
            .list_models("deepseek-official")
            .await
            .unwrap()[0]
            .id
            .as_str(),
        "user-model"
    );
    harness.settings_fiber.dispose().await.unwrap();
    wait_until(|| harness.context.get(SETTINGS).is_none()).await;
    let catalog = harness
        .runtime
        .list_models("deepseek-official")
        .await
        .unwrap();
    assert_eq!(catalog[0].id.as_str(), "entry-model");
    harness.provider_fiber.dispose().await.unwrap();
    assert!(harness.runtime.list_providers().is_empty());
}

#[tokio::test]
async fn settings_descriptor_uses_the_live_namespace_and_canonical_schema() {
    let harness = Harness::boot(DeepSeekConfig::default()).await;
    let descriptor = harness.settings().describe(false).pop().unwrap();
    assert_eq!(descriptor.ns.as_str(), "llm-deepseek");
    assert_eq!(descriptor.applies, seekdeep_settings::SettingsApplies::Live);
    assert_eq!(descriptor.value["apiKeyEnv"], "DEEPSEEK_API_KEY");
    assert_eq!(descriptor.value["maxTokens"], 256_000);
    assert_eq!(descriptor.value["models"].as_array().unwrap().len(), 2);
    let uid = descriptor.schema["uid"].as_u64().unwrap();
    assert_eq!(descriptor.schema["refs"][uid.to_string()]["type"], "object");
    assert_eq!(harness.context.get(LLM).unwrap().list_providers().len(), 1);
}
