//! Host settings namespace registration and teardown parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use seekdeep_client_locale::{LOCALE_SETTINGS_NAMESPACE, host_plugin};
use seekdeep_cordis::Context;
use seekdeep_settings::{
    SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value, json};

#[derive(Default)]
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
        Ok(Map::new())
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
async fn optional_host_registration_validates_and_tears_down() {
    let context = Context::new();
    let settings = SettingsService::install(&context, Arc::new(MemoryStorage))
        .await
        .unwrap();
    let fiber = context.plugin(host_plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    let namespace = settings_namespace(LOCALE_SETTINGS_NAMESPACE).unwrap();
    for _ in 0..100 {
        if settings.get(&namespace).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(settings.get(&namespace), Some(json!({})));
    settings
        .update(&namespace, json!({"preference":"en"}), None)
        .await
        .unwrap();
    assert_eq!(settings.get(&namespace), Some(json!({"preference":"en"})));
    assert!(
        settings
            .update(&namespace, json!({"preference":"fr"}), None)
            .await
            .is_err()
    );
    fiber.dispose().await.unwrap();
    for _ in 0..100 {
        if settings.get(&namespace).is_none() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(settings.get(&namespace), None);
}

#[test]
fn host_plugin_has_no_mandatory_settings_edge() {
    assert!(host_plugin().inject().is_empty());
}

#[tokio::test]
async fn host_plugin_without_settings_stays_nonblocking_and_disposable() {
    let context = Context::new();
    let fiber = context.plugin(host_plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    fiber.dispose().await.unwrap();
}
