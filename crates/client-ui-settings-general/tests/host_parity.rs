//! Host onboarding namespace registration and teardown parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use seekdeep_client_ui_settings_general::{
    INVARIANT_NAME, ONBOARDING_SETTINGS_NAMESPACE, WELCOME_NOTICE_VERSION_FIELD, host_plugin,
    onboarding_settings_schema,
};
use seekdeep_cordis::Context;
use seekdeep_settings::{
    SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value};

struct MemoryStorage;

#[async_trait]
impl SettingsStorage for MemoryStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(SettingsDocument::default())
    }

    async fn persist(
        &self,
        _namespace: &SettingsNamespace,
        _section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn optional_host_registration_validates_and_tears_down_with_its_fiber() {
    let context = Context::new();
    let settings = SettingsService::install(&context, Arc::new(MemoryStorage))
        .await
        .unwrap();
    let fiber = context.plugin(host_plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    let namespace = settings_namespace(ONBOARDING_SETTINGS_NAMESPACE).unwrap();
    for _ in 0..100 {
        if settings.get(&namespace).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        settings
            .describe(false)
            .iter()
            .any(|row| row.ns == namespace)
    );
    let schema = onboarding_settings_schema();
    assert!(schema.resolve(&serde_json::json!({})).is_ok());
    assert!(
        schema
            .resolve(&serde_json::json!({WELCOME_NOTICE_VERSION_FIELD: "v1"}))
            .is_ok()
    );
    assert!(
        schema
            .resolve(&serde_json::json!({WELCOME_NOTICE_VERSION_FIELD: 1}))
            .is_err()
    );
    fiber.dispose().await.unwrap();
    for _ in 0..100 {
        if settings.get(&namespace).is_none() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        settings
            .describe(false)
            .iter()
            .all(|row| row.ns != namespace)
    );
}

#[tokio::test]
async fn host_plugin_has_no_mandatory_settings_edge() {
    let plugin = host_plugin();
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    fiber.dispose().await.unwrap();
}

#[test]
fn invariant_companion_keeps_the_exact_explained_empty_identity() {
    assert_eq!(INVARIANT_NAME, "client-ui-settings-general-invariant");
}
