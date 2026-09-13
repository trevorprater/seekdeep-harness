//! Real declarative-loader composition for settings provider and consumer.

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_loader::PluginCatalog;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{
    SETTINGS, SettingsSectionSource, install_settings_section, settings_namespace,
};
use seekdeep_settings_file::plugin as settings_file_plugin;
use serde_json::{Value, json};

#[derive(Default)]
struct ConsumerState {
    source: Option<SettingsSectionSource>,
    applied: Option<Value>,
    seen: Vec<Value>,
}

fn theme_schema() -> Schema {
    Schema::object([
        (
            "theme",
            Schema::union([Schema::constant("dark"), Schema::constant("light")])
                .with_default("dark"),
        ),
        ("fontSize", Schema::number().with_default(14)),
    ])
}

fn consumer_plugin(state: Arc<Mutex<ConsumerState>>) -> Plugin {
    Plugin::new(
        "settings-consumer",
        std::iter::empty::<&str>(),
        move |context, config| {
            let state = state.clone();
            Box::pin(async move {
                let schema = theme_schema();
                state.lock().applied = Some(schema.resolve(&config)?);
                let source_slot = Arc::new(Mutex::new(None::<SettingsSectionSource>));
                let on_change = {
                    let state = state.clone();
                    let source_slot = source_slot.clone();
                    Arc::new(move || {
                        if let Some(source) = source_slot.lock().clone() {
                            let value = source.get();
                            let mut state = state.lock();
                            state.applied = Some(value.clone());
                            state.seen.push(value);
                        }
                        Ok(())
                    })
                };
                let installed = install_settings_section(
                    &context,
                    &settings_namespace("ui-theme")?,
                    schema,
                    config,
                    None,
                    on_change,
                )?;
                *source_slot.lock() = Some(installed.source.clone());
                state.lock().source = Some(installed.source);
                installed.fiber.await_settled().await?;
                Ok(())
            })
        },
    )
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cordis_yaml_boots_provider_and_hot_publishes_to_optional_consumer() {
    let directory = tempfile::tempdir().unwrap();
    let settings_path = directory.path().join("settings.yaml");
    tokio::fs::write(&settings_path, "ui-theme:\n  theme: light\n")
        .await
        .unwrap();
    let state = Arc::new(Mutex::new(ConsumerState::default()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("seekdeep-settings-file", settings_file_plugin())
        .unwrap();
    catalog
        .register_named("test-settings-consumer", consumer_plugin(state.clone()))
        .unwrap();
    let source = format!(
        concat!(
            "- id: settings\n",
            "  name: seekdeep-settings-file\n",
            "  config:\n",
            "    path: {}\n",
            "    debounceMs: 5\n",
            "- id: consumer\n",
            "  name: test-settings-consumer\n",
            "  config:\n",
            "    fontSize: 16\n",
        ),
        serde_json::to_string(&settings_path).unwrap()
    );
    let context = Context::new();
    let composition = catalog.load_yaml(&context, &source).await.unwrap();
    wait_until(|| state.lock().applied == Some(json!({ "theme": "light", "fontSize": 16 }))).await;
    assert_eq!(
        context.get(SETTINGS).unwrap().describe(false)[0]
            .ns
            .as_str(),
        "ui-theme"
    );

    tokio::fs::write(&settings_path, "ui-theme:\n  theme: dark\n  fontSize: 20\n")
        .await
        .unwrap();
    wait_until(|| state.lock().applied == Some(json!({ "theme": "dark", "fontSize": 20 }))).await;
    assert_eq!(
        state.lock().seen.last(),
        Some(&json!({ "theme": "dark", "fontSize": 20 }))
    );
    composition.dispose().await.unwrap();
    assert!(context.get(SETTINGS).is_none());
}

#[tokio::test]
async fn same_consumer_without_settings_keeps_entry_config_resolution() {
    let state = Arc::new(Mutex::new(ConsumerState::default()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("test-settings-consumer", consumer_plugin(state.clone()))
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: consumer\n",
                "  name: test-settings-consumer\n",
                "  config:\n",
                "    fontSize: 16\n",
            ),
        )
        .await
        .unwrap();
    assert!(context.get(SETTINGS).is_none());
    assert_eq!(
        state.lock().applied,
        Some(json!({ "theme": "dark", "fontSize": 16 }))
    );
    assert!(state.lock().seen.is_empty());
    composition.dispose().await.unwrap();
}
