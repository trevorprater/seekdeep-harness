//! Live settings precedence and deletion cleanup for the preset default.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust, SETTINGS_NAMESPACE,
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_settings::{SettingsDocument, SettingsService, SettingsStorage, settings_namespace};
use serde_json::{Map, Value, json};

struct MemoryStorage {
    document: Mutex<SettingsDocument>,
}

#[async_trait]
impl SettingsStorage for MemoryStorage {
    fn writable(&self) -> bool {
        true
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

async fn settings(context: &Context, document: Value) -> (Arc<Fiber>, Arc<SettingsService>) {
    let Value::Object(document) = document else {
        panic!("settings document must be an object");
    };
    let owner = Fiber::active_child("settings provider");
    let service = SettingsService::install(
        &context.with_fiber(owner.clone()),
        Arc::new(MemoryStorage {
            document: Mutex::new(document),
        }),
    )
    .await
    .unwrap();
    (owner, service)
}

async fn preset(root: &std::path::Path, id: &str) {
    let directory = root.join(id);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(directory.join(COMPOSITION_FILE), "[]\n")
        .await
        .unwrap();
}

fn registry(context: &Context, root: &std::path::Path) -> Arc<AgentPresetRegistry> {
    AgentPresetRegistry::new(
        context,
        PluginCatalog::new(),
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "standard".to_owned(),
                roots: vec![PresetRoot {
                    path: root.to_string_lossy().into_owned(),
                    trust: PresetTrust::User,
                }],
                include_user_root: false,
            },
            user_root: None,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn user_default_updates_live_and_provider_unload_reveals_composition_default() {
    let context = Context::new();
    let root = tempfile::tempdir().unwrap();
    preset(root.path(), "standard").await;
    preset(root.path(), "minimal").await;
    let (owner, service) = settings(
        &context,
        json!({ "agent-presets": { "default": "minimal" } }),
    )
    .await;
    let registry = registry(&context, root.path());
    assert_eq!(registry.default_id(), "minimal");
    service
        .update(
            &settings_namespace(SETTINGS_NAMESPACE).unwrap(),
            json!({ "default": "standard" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(registry.default_id(), "standard");
    owner.dispose().await.unwrap();
    assert_eq!(registry.default_id(), "standard");

    let (_replacement_owner, _) = settings(
        &context,
        json!({ "agent-presets": { "default": "minimal" } }),
    )
    .await;
    assert_eq!(registry.default_id(), "minimal");
    assert_eq!(registry.resolve(None).await.unwrap().id, "minimal");
}

#[tokio::test]
async fn removing_the_selected_user_preset_clears_only_its_override() {
    let context = Context::new();
    let root = tempfile::tempdir().unwrap();
    preset(root.path(), "standard").await;
    preset(root.path(), "mine").await;
    let (_owner, service) =
        settings(&context, json!({ "agent-presets": { "default": "mine" } })).await;
    let registry = registry(&context, root.path());
    assert_eq!(registry.default_id(), "mine");
    registry.remove("mine").await.unwrap();
    assert_eq!(registry.default_id(), "standard");
    let descriptor = service
        .describe(true)
        .into_iter()
        .find(|descriptor| descriptor.ns.as_str() == SETTINGS_NAMESPACE)
        .unwrap();
    assert_eq!(descriptor.user, Some(json!({})));
    assert_eq!(descriptor.value, json!({ "default": "standard" }));
}

#[tokio::test]
async fn default_changes_affect_only_later_mounts_and_unknown_values_fail_at_resolution() {
    let context = Context::new();
    let root = tempfile::tempdir().unwrap();
    preset(root.path(), "standard").await;
    preset(root.path(), "minimal").await;
    let (_owner, service) = settings(
        &context,
        json!({ "agent-presets": { "default": "minimal" } }),
    )
    .await;
    let registry = registry(&context, root.path());
    let first = create_scope(&context, ScopeKey::new(), None).unwrap();
    registry.mount(&first.context, None).await.unwrap();
    service
        .update(
            &settings_namespace(SETTINGS_NAMESPACE).unwrap(),
            json!({ "default": "standard" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        registry.composed_preset(&first.context).as_deref(),
        Some("minimal")
    );
    let second = create_scope(&context, ScopeKey::new(), None).unwrap();
    registry.mount(&second.context, None).await.unwrap();
    assert_eq!(
        registry.composed_preset(&second.context).as_deref(),
        Some("standard")
    );

    service
        .update(
            &settings_namespace(SETTINGS_NAMESPACE).unwrap(),
            json!({ "default": "future" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(registry.default_id(), "future");
    assert_eq!(registry.list().await.unwrap().len(), 2);
    assert!(registry.resolve(None).await.is_err());
}
