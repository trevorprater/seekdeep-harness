//! Executable composition and patch lifecycle parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_loader::{ConfigTree, LOADER, LoaderError, Patch, PluginCatalog};
use serde_json::json;

fn recording_plugin(name: &'static str, events: Arc<Mutex<Vec<String>>>) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, config| {
        let events = events.clone();
        Box::pin(async move {
            events.lock().push(format!("start:{name}:{config}"));
            context.own(EffectHandle::synchronous(name, move || {
                events.lock().push(format!("stop:{name}"));
                Ok(())
            }))?;
            Ok(())
        })
    })
}

#[tokio::test]
async fn yaml_list_mounts_enabled_entries_and_nested_children_then_disposes_in_reverse() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("alpha", recording_plugin("alpha", events.clone()))
        .unwrap();
    catalog
        .register_named("child", recording_plugin("child", events.clone()))
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: alpha-entry\n",
                "  name: alpha\n",
                "  config:\n",
                "    value: 7\n",
                "  children:\n",
                "    - id: child-entry\n",
                "      name: child\n",
                "- id: skipped\n",
                "  name: alpha\n",
                "  disabled: true\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 2);
    assert_eq!(
        *events.lock(),
        vec![
            "start:alpha:{\"value\":7}".to_owned(),
            "start:child:{}".to_owned(),
        ]
    );
    composition.dispose().await.unwrap();
    assert_eq!(
        *events.lock(),
        vec![
            "start:alpha:{\"value\":7}".to_owned(),
            "start:child:{}".to_owned(),
            "stop:child".to_owned(),
            "stop:alpha".to_owned(),
        ]
    );
}

#[tokio::test]
async fn unknown_or_failed_later_entry_rolls_back_every_prior_mount() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("alpha", recording_plugin("alpha", events.clone()))
        .unwrap();
    let context = Context::new();
    let error = catalog
        .load_yaml(
            &context,
            "- id: first\n  name: alpha\n- id: missing\n  name: absent\n",
        )
        .await
        .unwrap_err();
    assert!(matches!(error, LoaderError::UnknownPlugin(name) if name == "absent"));
    assert_eq!(
        *events.lock(),
        vec!["start:alpha:{}".to_owned(), "stop:alpha".to_owned()]
    );
}

#[test]
fn patch_replaces_nested_rows_wholesale_and_appends_unknown_ids() {
    let mut tree: ConfigTree = serde_json::from_value(json!({
        "entries": [{
            "id": "parent",
            "name": "one",
            "config": { "keep": true },
            "children": [{ "id": "nested", "name": "two" }]
        }]
    }))
    .unwrap();
    let patch: Patch = serde_json::from_value(json!({
        "nested": { "id": "ignored", "name": "replacement", "config": { "next": 1 } },
        "new": { "id": "ignored-too", "name": "three" }
    }))
    .unwrap();
    tree.apply_patch(patch);
    assert_eq!(tree.entries[0].children[0].id.as_str(), "nested");
    assert_eq!(tree.entries[0].children[0].plugin.as_str(), "replacement");
    assert_eq!(tree.entries[0].children[0].config, json!({ "next": 1 }));
    assert!(tree.entries[0].children[0].children.is_empty());
    assert_eq!(tree.entries[1].id.as_str(), "new");
    assert_eq!(tree.entries[1].config, json!({}));
}

#[test]
fn catalog_rejects_duplicate_and_empty_names() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "same",
            Plugin::new("one", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
        )
        .unwrap();
    assert!(matches!(
        catalog.register_named(
            "same",
            Plugin::new("two", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            })
        ),
        Err(LoaderError::DuplicatePlugin(name)) if name == "same"
    ));
    assert!(matches!(
        catalog.register_named(
            " ",
            Plugin::new("empty", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            })
        ),
        Err(LoaderError::InvalidPluginSpecifier)
    ));
}

#[tokio::test]
async fn exact_generation_settlement_waits_for_later_siblings() {
    let catalog = PluginCatalog::new();
    let (settled_sender, settled_receiver) = tokio::sync::oneshot::channel();
    let settled_sender = Arc::new(Mutex::new(Some(settled_sender)));
    catalog
        .register_named(
            "waiter",
            Plugin::new("waiter", ["loader"], move |context, _| {
                let settlement = context.get(LOADER).expect("loader settlement");
                let sender = settled_sender.clone();
                Box::pin(async move {
                    tokio::spawn(async move {
                        let result = settlement.wait().await.map_err(|error| error.to_string());
                        if let Some(sender) = sender.lock().take() {
                            let _ = sender.send(result);
                        }
                    });
                    Ok(())
                })
            }),
        )
        .unwrap();
    let blocker_started = Arc::new(tokio::sync::Notify::new());
    let blocker_release = Arc::new(tokio::sync::Notify::new());
    let started = blocker_started.clone();
    let release = blocker_release.clone();
    catalog
        .register_named(
            "blocker",
            Plugin::new("blocker", std::iter::empty::<&str>(), move |_, _| {
                let started = started.clone();
                let release = release.clone();
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let loading = tokio::spawn({
        let catalog = catalog.clone();
        let context = context.clone();
        async move {
            catalog
                .load_yaml(
                    &context,
                    "- id: waiter\n  name: waiter\n- id: blocker\n  name: blocker\n",
                )
                .await
        }
    });
    blocker_started.notified().await;
    let mut settled_receiver = settled_receiver;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut settled_receiver)
            .await
            .is_err(),
        "waiter settled before its later sibling"
    );
    blocker_release.notify_one();
    let composition = loading.await.unwrap().unwrap();
    assert_eq!(settled_receiver.await.unwrap(), Ok(()));
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn failed_generation_wakes_waiters_only_after_rollback() {
    let catalog = PluginCatalog::new();
    let rolled_back = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rollback_flag = rolled_back.clone();
    let (settled_sender, settled_receiver) = tokio::sync::oneshot::channel();
    let settled_sender = Arc::new(Mutex::new(Some(settled_sender)));
    catalog
        .register_named(
            "waiter",
            Plugin::new("waiter", ["loader"], move |context, _| {
                let settlement = context.get(LOADER).expect("loader settlement");
                let sender = settled_sender.clone();
                let observed_rollback = rollback_flag.clone();
                context
                    .own(EffectHandle::synchronous("waiter rollback", {
                        let rollback_flag = rollback_flag.clone();
                        move || {
                            rollback_flag.store(true, std::sync::atomic::Ordering::Release);
                            Ok(())
                        }
                    }))
                    .expect("rollback effect");
                Box::pin(async move {
                    tokio::spawn(async move {
                        let result = settlement.wait().await.map_err(|error| error.to_string());
                        if let Some(sender) = sender.lock().take() {
                            let _ = sender.send((
                                result,
                                observed_rollback.load(std::sync::atomic::Ordering::Acquire),
                            ));
                        }
                    });
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "failure",
            Plugin::new("failure", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { anyhow::bail!("later sibling failed") })
            }),
        )
        .unwrap();
    let context = Context::new();
    let error = catalog
        .load_yaml(
            &context,
            "- id: waiter\n  name: waiter\n- id: failure\n  name: failure\n",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("later sibling failed"));
    assert!(rolled_back.load(std::sync::atomic::Ordering::Acquire));
    let (settlement, rollback_was_visible) = settled_receiver.await.unwrap();
    assert!(settlement.is_err());
    assert!(rollback_was_visible);
    context.fiber().dispose().await.unwrap();
}
