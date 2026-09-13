//! Behavioral mirror of `packages/settings/settings/tests/invariant.spec.ts`.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, EventArgs, Fiber};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{
    SettingsDocument, SettingsNamespace, SettingsRegisterOptions, SettingsService, SettingsStorage,
    register_invariant, settings_namespace,
};
use serde_json::{Map, Value, json};

struct EmptyStorage;

#[async_trait]
impl SettingsStorage for EmptyStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(Map::new())
    }

    async fn persist(&self, _: &SettingsNamespace, _: &Map<String, Value>) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn setup(with_provider: bool) -> (Context, Option<Arc<SettingsService>>) {
    let context = Context::new();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    let settings = if with_provider {
        let fiber = Fiber::active_child("settings-provider");
        Some(
            SettingsService::install(&context.with_fiber(fiber), Arc::new(EmptyStorage))
                .await
                .unwrap(),
        )
    } else {
        None
    };
    (context, settings)
}

fn event_args(ns: SettingsNamespace, next: Value, previous: Value) -> EventArgs {
    EventArgs::from_values(vec![
        Arc::new(ns),
        Arc::new(next),
        Arc::new(previous),
        Arc::new(seekdeep_settings::SettingsUpdateSource::Provider),
    ])
}

#[tokio::test]
async fn fails_an_updated_event_without_a_live_settings_service() {
    let (context, _) = setup(false).await;
    let error = context
        .events()
        .emit(
            &context,
            "settings/updated",
            &event_args(
                settings_namespace("ghost").unwrap(),
                json!({ "a": 1 }),
                json!({ "a": 2 }),
            ),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("without a live settings service")
    );
}

#[tokio::test]
async fn fails_an_updated_event_for_an_unregistered_namespace() {
    let (context, _) = setup(true).await;
    let error = context
        .events()
        .emit(
            &context,
            "settings/updated",
            &event_args(
                settings_namespace("ghost").unwrap(),
                json!({ "a": 1 }),
                json!({ "a": 2 }),
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("unregistered"));
}

async fn registered_theme() -> (Context, Arc<SettingsService>) {
    let (context, settings) = setup(true).await;
    let settings = settings.unwrap();
    settings
        .register(
            &context,
            &settings_namespace("ui-theme").unwrap(),
            Schema::object([("theme", Schema::string().with_default("dark"))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    (context, settings)
}

#[tokio::test]
async fn fails_an_updated_event_without_a_resolved_value_change() {
    let (context, _settings) = registered_theme().await;
    let error = context
        .events()
        .emit(
            &context,
            "settings/updated",
            &event_args(
                settings_namespace("ui-theme").unwrap(),
                json!({ "theme": "dark" }),
                json!({ "theme": "dark" }),
            ),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("without a resolved-value change")
    );
}

#[tokio::test]
async fn fails_an_updated_event_diverging_from_authoritative_state() {
    let (context, _settings) = registered_theme().await;
    let error = context
        .events()
        .emit(
            &context,
            "settings/updated",
            &event_args(
                settings_namespace("ui-theme").unwrap(),
                json!({ "theme": "forged" }),
                json!({ "theme": "dark" }),
            ),
        )
        .unwrap_err();
    assert!(error.to_string().contains("authoritative resolved value"));
}
