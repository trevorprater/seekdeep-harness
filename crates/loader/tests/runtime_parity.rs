//! Executable composition and patch lifecycle parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_loader::{ConfigTree, LoaderError, Patch, PluginCatalog};
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
