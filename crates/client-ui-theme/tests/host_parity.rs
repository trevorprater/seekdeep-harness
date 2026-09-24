//! Host settings schema, current-preference bootstrap, optional edges, and teardown parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use seekdeep_client_ui_theme::{
    THEME_SETTINGS_NAMESPACE, ThemePreference, host_plugin, read_preference,
};
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_settings::{
    SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage, settings_namespace,
};
use serde_json::{Map, Value, json};

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

async fn wait_for(condition: impl Fn() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(condition(), "condition did not settle");
}

#[tokio::test]
async fn optional_host_edges_register_live_settings_and_index_transform_then_unwind() {
    let context = Context::new();
    let settings = SettingsService::install(&context, Arc::new(MemoryStorage))
        .await
        .unwrap();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    let fiber = context.plugin(host_plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    let namespace = settings_namespace(THEME_SETTINGS_NAMESPACE).unwrap();
    wait_for(|| settings.get(&namespace).is_some()).await;
    assert_eq!(
        settings.get(&namespace),
        Some(json!({"preference": "system"}))
    );
    assert_eq!(read_preference(&context), ThemePreference::System);
    let system = server.apply_index_taps("<html><body><main>shell</main></body></html>");
    assert!(system.contains("const preference = \"system\""));
    settings
        .update(&namespace, json!({"preference": "dark"}), None)
        .await
        .unwrap();
    assert_eq!(read_preference(&context), ThemePreference::Dark);
    assert!(
        server
            .apply_index_taps("<body></body>")
            .contains("const preference = \"dark\"")
    );
    assert!(
        settings
            .update(&namespace, json!({"preference": "sepia"}), None)
            .await
            .is_err()
    );
    fiber.dispose().await.unwrap();
    wait_for(|| settings.get(&namespace).is_none()).await;
    assert_eq!(server.apply_index_taps("<body></body>"), "<body></body>");
}

#[tokio::test]
async fn either_optional_service_can_arrive_alone_and_the_root_has_no_hard_edges() {
    let bare = Context::new();
    let plugin = host_plugin();
    assert!(plugin.inject().is_empty());
    let fiber = bare.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    fiber.dispose().await.unwrap();

    let web_only = Context::new();
    let server = WebServer::install(
        &web_only,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    let fiber = web_only.plugin(host_plugin(), Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert!(
        server
            .apply_index_taps("<body></body>")
            .contains("const preference = \"system\"")
    );
    fiber.dispose().await.unwrap();
}
