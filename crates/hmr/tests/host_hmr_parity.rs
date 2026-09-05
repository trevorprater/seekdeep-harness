//! Host watcher integration over real Loader module generations.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_hmr::{Config, HostHmrService};
use seekdeep_loader::{Entry, EntryId, EntryParent, LOADER, PluginCatalog};
use serde_json::Value;

const HMR_VALUE: ServiceKey<Value> = ServiceKey::new("hmrValue");
const CONFIG_VALUE: ServiceKey<Value> = ServiceKey::new("configValue");

async fn eventually(mut predicate: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[tokio::test]
async fn recursive_watcher_reloads_modules_requests_full_restart_and_joins_disposal()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let config_path = temporary.path().join("cordis.yml");
    let module_path = temporary.path().join("main.mjs");
    let dependency_path = temporary.path().join("dep.mjs");
    let external_path = temporary.path().join("launcher.mjs");
    std::fs::write(
        &module_path,
        concat!(
            "import { value } from './dep.mjs';\n",
            "export function apply(ctx) { ctx.provide('hmrValue', value); }\n",
        ),
    )?;
    std::fs::write(&dependency_path, "export const value = 'old';\n")?;
    std::fs::write(&external_path, "export const generation = 1;\n")?;
    let catalog = PluginCatalog::new().with_hmr_externals([external_path.clone()]);
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(&context, "- id: module\n  name: ./main.mjs\n", &config_path)
        .await?;
    assert_eq!(
        context.get(HMR_VALUE).as_deref(),
        Some(&serde_json::json!("old"))
    );

    let restarts = Arc::new(AtomicUsize::new(0));
    let restart: seekdeep_hmr::RestartHook = Arc::new({
        let restarts = restarts.clone();
        move || {
            let restarts = restarts.clone();
            Box::pin(async move {
                restarts.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
        }
    });
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).expect("loader"),
        Config {
            base: Some(temporary.path().to_path_buf()),
            root: vec![temporary.path().to_path_buf()],
            debounce: 20,
            ignored: Vec::new(),
        },
        restart,
    )?;

    std::fs::write(&dependency_path, "export const value = 'new';\n")?;
    eventually(
        || {
            context
                .get(HMR_VALUE)
                .is_some_and(|value| *value == serde_json::json!("new"))
        },
        "dependency change was not reloaded",
    )
    .await;
    std::fs::write(&external_path, "export const generation = 2;\n")?;
    eventually(
        || restarts.load(Ordering::Acquire) >= 1,
        "external change did not request restart",
    )
    .await;

    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn config_files_refresh_before_module_classification_and_failures_are_contained()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let base_path = temporary.path().join("base.yml");
    std::fs::write(
        &base_path,
        "- id: value\n  name: value\n  config: { value: old }\n",
    )?;
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "value",
        seekdeep_cordis::Plugin::new("value", std::iter::empty::<&str>(), |context, config| {
            Box::pin(async move {
                context.provide(CONFIG_VALUE, Arc::new(config))?;
                Ok(())
            })
        }),
    )?;
    let context = Context::new();
    let composition = catalog.load_yaml(&context, "[]\n").await?;
    context
        .get(LOADER)
        .unwrap()
        .create_entry(
            Entry::file_include(
                EntryId::new("include")?,
                base_path.to_string_lossy(),
                Vec::new(),
            )?,
            EntryParent::Root,
            None,
        )
        .await?;
    assert_eq!(context.get(CONFIG_VALUE).expect("old")["value"], "old");

    let failures = Arc::new(AtomicUsize::new(0));
    let changes = Arc::new(AtomicUsize::new(0));
    for (name, count) in [
        ("hmr/config-update-failed", failures.clone()),
        ("hmr/change", changes.clone()),
    ] {
        context.events().on(
            &context,
            name,
            move |_, _| {
                let count = count.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::AcqRel);
                    Ok(seekdeep_cordis::EventReply::Undefined)
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )?;
    }
    let service = HostHmrService::start(
        context.clone(),
        context.get(LOADER).unwrap(),
        Config {
            base: Some(temporary.path().to_path_buf()),
            root: vec![temporary.path().to_path_buf()],
            debounce: 20,
            ignored: Vec::new(),
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    )?;

    std::fs::write(
        &base_path,
        "- id: value\n  name: value\n  config: { value: live }\n",
    )?;
    eventually(
        || {
            context
                .get(CONFIG_VALUE)
                .is_some_and(|value| value["value"] == "live")
        },
        "config change was not refreshed",
    )
    .await;
    assert_eq!(changes.load(Ordering::Acquire), 0);

    std::fs::write(&base_path, "invalid: [unclosed\n")?;
    eventually(
        || failures.load(Ordering::Acquire) >= 1,
        "config failure was not emitted",
    )
    .await;
    assert_eq!(
        context.get(CONFIG_VALUE).expect("last good")["value"],
        "live"
    );
    assert_eq!(changes.load(Ordering::Acquire), 0);

    service.dispose().await?;
    composition.dispose().await?;
    Ok(())
}
