//! Serialized whole-tree replacement and last-good rollback parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_app_boot::ReloadableComposition;
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_loader::PluginCatalog;
use serde_json::Value;

const CURRENT: ServiceKey<Value> = ServiceKey::new("current");

fn source(name: &str, value: &str, fail: bool) -> String {
    format!("- id: current\n  name: {name}\n  config:\n    value: {value}\n    fail: {fail}\n")
}

#[tokio::test]
async fn parse_import_and_activation_failures_keep_or_restore_last_good_tree() -> anyhow::Result<()>
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "value",
        Plugin::new("value", std::iter::empty::<&str>(), {
            let events = events.clone();
            move |context, config| {
                let events = events.clone();
                Box::pin(async move {
                    events.lock().push(format!("attempt:{}", config["value"]));
                    anyhow::ensure!(config["fail"] != true, "candidate config failed");
                    context.provide(CURRENT, Arc::new(config.clone()))?;
                    context.own(EffectHandle::synchronous("value stop", move || {
                        events.lock().push(format!("stop:{}", config["value"]));
                        Ok(())
                    }))?;
                    Ok(())
                })
            }
        }),
    )?;
    let context = Context::new();
    let old = source("value", "old", false);
    let reload = ReloadableComposition::open(context.clone(), catalog, old.clone()).await?;
    assert_eq!(context.get(CURRENT).expect("old")["value"], "old");

    let parse = reload.replace("invalid: [unclosed\n").await.unwrap_err();
    assert!(parse.to_string().contains("invalid composition"));
    assert_eq!(context.get(CURRENT).expect("still old")["value"], "old");
    assert_eq!(&*events.lock(), &["attempt:\"old\""]);

    let missing = reload
        .replace(source("missing", "missing", false))
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("missing"));
    assert_eq!(context.get(CURRENT).expect("still old")["value"], "old");
    assert_eq!(&*events.lock(), &["attempt:\"old\""]);

    let failed = reload
        .replace(source("value", "bad", true))
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("candidate config failed"));
    assert_eq!(reload.source().await, old);
    assert_eq!(context.get(CURRENT).expect("restored")["value"], "old");
    assert_eq!(
        &*events.lock(),
        &[
            "attempt:\"old\"",
            "stop:\"old\"",
            "attempt:\"bad\"",
            "attempt:\"old\"",
        ]
    );

    let next = source("value", "new", false);
    reload.replace(next.clone()).await?;
    assert_eq!(reload.source().await, next);
    assert_eq!(context.get(CURRENT).expect("new")["value"], "new");
    reload.dispose().await?;
    assert!(context.get(CURRENT).is_none());
    assert!(
        reload
            .replace(source("value", "later", false))
            .await
            .is_err()
    );
    context.fiber().dispose().await
}

fn event_plugin(name: &'static str, events: Arc<Mutex<Vec<String>>>, fail: bool) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, config| {
        let events = events.clone();
        Box::pin(async move {
            events
                .lock()
                .push(format!("start:{name}:{}", config["value"]));
            anyhow::ensure!(!fail, "{name} candidate failed");
            context.own(EffectHandle::synchronous(name, move || {
                events
                    .lock()
                    .push(format!("stop:{name}:{}", config["value"]));
                Ok(())
            }))?;
            Ok(())
        })
    })
}

#[tokio::test]
async fn later_failure_rolls_back_earlier_updates_and_additions_without_restarting_unchanged_rows()
-> anyhow::Result<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog.register_named("value", event_plugin("value", events.clone(), false))?;
    catalog.register_named("stable", event_plugin("stable", events.clone(), false))?;
    catalog.register_named("bad", event_plugin("bad", events.clone(), true))?;
    let initial = concat!(
        "- id: existing\n",
        "  name: value\n",
        "  config: { value: old }\n",
        "- id: stable\n",
        "  name: stable\n",
        "  config: { value: pinned }\n",
    );
    let context = Context::new();
    let reload = ReloadableComposition::open(context.clone(), catalog, initial).await?;
    let candidate = concat!(
        "- id: existing\n",
        "  name: value\n",
        "  config: { value: candidate }\n",
        "- id: stable\n",
        "  name: stable\n",
        "  config: { value: pinned }\n",
        "- id: added\n",
        "  name: value\n",
        "  config: { value: added }\n",
        "- id: bad\n",
        "  name: bad\n",
        "  config: { value: rejected }\n",
    );
    let error = reload.replace(candidate).await.unwrap_err();
    assert!(error.to_string().contains("bad candidate failed"));
    assert_eq!(reload.source().await, initial);
    assert_eq!(
        &*events.lock(),
        &[
            "start:value:\"old\"",
            "start:stable:\"pinned\"",
            "stop:value:\"old\"",
            "start:value:\"candidate\"",
            "start:value:\"added\"",
            "start:bad:\"rejected\"",
            "stop:value:\"candidate\"",
            "start:value:\"old\"",
            "stop:value:\"added\"",
        ]
    );
    reload.dispose().await?;
    assert_eq!(
        events
            .lock()
            .iter()
            .filter(|event| event.as_str() == "start:stable:\"pinned\"")
            .count(),
        1
    );
    assert_eq!(
        events
            .lock()
            .iter()
            .filter(|event| event.as_str() == "stop:stable:\"pinned\"")
            .count(),
        1
    );
    context.fiber().dispose().await
}

#[tokio::test]
async fn changed_name_is_preflighted_then_replaced_with_old_plugin_reconstruction_on_failure()
-> anyhow::Result<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog.register_named("old", event_plugin("old", events.clone(), false))?;
    catalog.register_named("new", event_plugin("new", events.clone(), false))?;
    catalog.register_named("bad", event_plugin("bad", events.clone(), true))?;
    let context = Context::new();
    let reload = ReloadableComposition::open(
        context.clone(),
        catalog,
        "- id: target\n  name: old\n  config: { value: old }\n",
    )
    .await?;

    let missing = reload
        .replace("- id: target\n  name: missing\n  config: { value: missing }\n")
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("missing"));
    assert_eq!(&*events.lock(), &["start:old:\"old\""]);

    let failed = reload
        .replace("- id: target\n  name: bad\n  config: { value: bad }\n")
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("bad candidate failed"));
    assert_eq!(
        &*events.lock(),
        &[
            "start:old:\"old\"",
            "stop:old:\"old\"",
            "start:bad:\"bad\"",
            "start:old:\"old\"",
        ]
    );

    reload
        .replace("- id: target\n  name: new\n  config: { value: committed }\n")
        .await?;
    assert_eq!(
        &*events.lock(),
        &[
            "start:old:\"old\"",
            "stop:old:\"old\"",
            "start:bad:\"bad\"",
            "start:old:\"old\"",
            "stop:old:\"old\"",
            "start:new:\"committed\"",
        ]
    );
    reload.dispose().await?;
    context.fiber().dispose().await
}

#[tokio::test]
async fn disabling_and_reenabling_a_group_stops_and_restores_its_descendants() -> anyhow::Result<()>
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog.register_named("child", event_plugin("child", events.clone(), false))?;
    let tree = |disabled: bool| {
        format!(
            concat!(
                "- id: group\n",
                "  name: cordis:group\n",
                "  group: true\n",
                "  disabled: {}\n",
                "  config:\n",
                "    - id: child\n",
                "      name: child\n",
                "      config: {{ value: child }}\n",
            ),
            disabled
        )
    };
    let context = Context::new();
    let reload = ReloadableComposition::open(context.clone(), catalog, tree(false)).await?;
    reload.replace(tree(true)).await?;
    reload.replace(tree(false)).await?;
    assert_eq!(
        &*events.lock(),
        &[
            "start:child:\"child\"",
            "stop:child:\"child\"",
            "start:child:\"child\"",
        ]
    );
    reload.dispose().await?;
    context.fiber().dispose().await
}
