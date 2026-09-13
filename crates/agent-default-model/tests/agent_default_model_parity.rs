//! Source-oracle parity for the default Agent model settings seam.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::ModelSelection;
use seekdeep_agent_default_model::{
    AGENT_DEFAULT_MODEL, AgentDefaultModelConfig, settings_namespace_id,
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_llm::ReasoningEffortId;
use seekdeep_settings::{SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct MemorySettings {
    document: Mutex<SettingsDocument>,
}

#[async_trait]
impl SettingsStorage for MemorySettings {
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
        namespace: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.document.lock().insert(
            namespace.as_str().to_owned(),
            Value::Object(section.clone()),
        );
        Ok(())
    }
}

struct Bench {
    context: Context,
    settings_fiber: Arc<Fiber>,
    model_fiber: Arc<seekdeep_cordis::PluginFiber>,
}

impl Bench {
    async fn boot() -> Self {
        let context = Context::new();
        let settings_fiber = Fiber::active_child("memory-settings");
        let provider = context.with_fiber(settings_fiber.clone());
        SettingsService::install(&provider, Arc::new(MemorySettings::default()))
            .await
            .expect("settings");
        let model_fiber = seekdeep_agent_default_model::install(
            &context,
            AgentDefaultModelConfig {
                provider: "deepseek-official".into(),
                model: "deepseek-v4-flash".into(),
            },
        )
        .expect("mount");
        model_fiber.await_settled().await.expect("model service");
        Self {
            context,
            settings_fiber,
            model_fiber,
        }
    }

    fn service(&self) -> Arc<seekdeep_agent_default_model::AgentDefaultModel> {
        self.context.get(AGENT_DEFAULT_MODEL).expect("service")
    }

    async fn dispose(self) {
        self.model_fiber.dispose().await.expect("model dispose");
        self.settings_fiber
            .dispose()
            .await
            .expect("settings dispose");
    }
}

fn selection(provider: &str, model: &str, effort: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: provider.into(),
        model: model.into(),
        reasoning_effort: effort.map(ReasoningEffortId::new),
    }
}

#[tokio::test]
async fn resolves_user_layer_over_composition_entry() {
    let bench = Bench::boot().await;
    let service = bench.service();
    assert_eq!(
        service.current_selection(),
        selection("deepseek-official", "deepseek-v4-flash", None)
    );
    service
        .save_selection(&selection("acme-gateway", "acme-large", Some("high")))
        .await
        .expect("save");
    assert_eq!(
        service.current_selection(),
        selection("acme-gateway", "acme-large", Some("high"))
    );
    bench.dispose().await;
}

#[tokio::test]
async fn clears_stored_effort_when_saved_selection_has_none() {
    let bench = Bench::boot().await;
    let service = bench.service();
    service
        .save_selection(&selection("acme-gateway", "acme-large", Some("high")))
        .await
        .expect("save effort");
    service
        .save_selection(&selection("acme-gateway", "acme-plain", None))
        .await
        .expect("clear effort");
    assert_eq!(
        service.current_selection(),
        selection("acme-gateway", "acme-plain", None)
    );
    let document = bench
        .context
        .get(seekdeep_settings::SETTINGS)
        .expect("settings")
        .describe(false);
    assert_eq!(
        document[0].user,
        Some(json!({
            "provider": "acme-gateway",
            "model": "acme-plain"
        }))
    );
    bench.dispose().await;
}

#[tokio::test]
async fn layers_hand_written_partial_section_over_entry() {
    let bench = Bench::boot().await;
    bench
        .context
        .get(seekdeep_settings::SETTINGS)
        .expect("settings")
        .replace(
            &settings_namespace_id(),
            json!({ "model": "deepseek-reasoner" }),
            None,
        )
        .await
        .expect("replace");
    assert_eq!(
        bench.service().current_selection(),
        selection("deepseek-official", "deepseek-reasoner", None)
    );
    bench.dispose().await;
}

#[tokio::test]
async fn falls_back_to_composition_entry_when_settings_provider_detaches() {
    let bench = Bench::boot().await;
    let service = bench.service();
    service
        .save_selection(&selection("acme-gateway", "acme-large", None))
        .await
        .expect("save");
    assert_eq!(
        service.current_selection().provider.as_str(),
        "acme-gateway"
    );
    bench
        .settings_fiber
        .dispose()
        .await
        .expect("settings dispose");
    assert_eq!(
        service.current_selection(),
        selection("deepseek-official", "deepseek-v4-flash", None)
    );
    bench.model_fiber.dispose().await.expect("model dispose");
}

#[tokio::test]
async fn keeps_composition_entry_when_no_settings_provider_is_mounted() {
    let context = Context::new();
    let fiber = seekdeep_agent_default_model::install(
        &context,
        AgentDefaultModelConfig {
            provider: "p".into(),
            model: "m".into(),
        },
    )
    .expect("mount");
    fiber.await_settled().await.expect("service");
    let service = context.get(AGENT_DEFAULT_MODEL).expect("service");
    service
        .save_selection(&selection("other", "other", None))
        .await
        .expect("no-op save");
    assert_eq!(service.current_selection(), selection("p", "m", None));
    fiber.dispose().await.expect("dispose");
}
