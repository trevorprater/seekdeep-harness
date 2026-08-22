//! User patch add/failure/recovery/removal through transactional reload.

use std::{path::Path, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_app_boot::{
    BootOptions, BootUserPatchWatchOptions, ConfigDumpLayer, ConfigWatchRegistry, PatchComposer,
    ReloadableComposition, UserPatchWatchOptions, boot, render_config_dump,
    watch_boot_user_patches, watch_user_patches,
};
use seekdeep_cordis::{Context, Fiber, Plugin, ServiceKey};
use seekdeep_loader::profile_patch::parse_patch_list_yaml;
use seekdeep_loader::{Entry, EntryId, EntryParent, LOADER, PluginCatalog};
use serde_json::Value;

const CURRENT: ServiceKey<Value> = ServiceKey::new("current");

async fn eventually(mut test: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[tokio::test]
async fn add_failure_recovery_and_removal_preserve_last_good_generation() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let base = temporary.path().join("cordis.yml");
    let user = temporary.path().join("user.patch.yml");
    std::fs::write(
        &base,
        "- id: current\n  name: value\n  config:\n    value: base\n",
    )?;
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "value",
        Plugin::new("value", std::iter::empty::<&str>(), |context, config| {
            Box::pin(async move {
                anyhow::ensure!(config["fail"] != true, "candidate config failed");
                context.provide(CURRENT, Arc::new(config))?;
                Ok(())
            })
        }),
    )?;
    let app_patch = parse_patch_list_yaml("- id: current\n  config:\n    value: generated\n")?;
    let initial = render_config_dump(
        "seekdeep-test-bin",
        &base,
        &[ConfigDumpLayer {
            label: "generated".to_owned(),
            patches: app_patch.clone(),
        }],
        |_| {},
    )?;
    let context = Context::new();
    let reload = Arc::new(ReloadableComposition::open(context.clone(), catalog, initial).await?);
    assert_eq!(context.get(CURRENT).expect("initial")["value"], "generated");

    let failures = Arc::new(Mutex::new(Vec::new()));
    let failure_filename = user.clone();
    let failure: seekdeep_app_boot::ConfigRefreshFailure = Arc::new({
        let failures = failures.clone();
        move |path: &Path, error| {
            assert_eq!(path, failure_filename);
            failures.lock().push(error.to_string());
        }
    });
    let compose: PatchComposer =
        Arc::new(move |user| app_patch.iter().cloned().chain(user).collect());
    let watcher = watch_user_patches(UserPatchWatchOptions {
        bin_name: "seekdeep-test-bin".to_owned(),
        base_config: base,
        filename: user.clone(),
        compose,
        reload: reload.clone(),
        registry: ConfigWatchRegistry::new(),
        failure,
    })?;

    std::fs::write(&user, "- id: current\n  config:\n    value: live\n")?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "live")
        },
        "user patch addition",
    )
    .await;

    std::fs::write(&user, "- id: current\n  config:\n    fail: true\n")?;
    eventually(|| failures.lock().len() == 1, "candidate failure").await;
    assert!(failures.lock()[0].contains("candidate config failed"));
    assert_eq!(context.get(CURRENT).expect("last good")["value"], "live");

    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(&user, "invalid: [unclosed\n")?;
    eventually(|| failures.lock().len() == 2, "parse failure").await;
    assert!(failures.lock()[1].contains("failed to parse patches"));
    assert_eq!(context.get(CURRENT).expect("still live")["value"], "live");

    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(&user, "- id: current\n  config:\n    value: recovered\n")?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "recovered")
        },
        "valid recovery",
    )
    .await;

    std::fs::remove_file(&user)?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "generated")
        },
        "patch removal",
    )
    .await;
    assert_eq!(failures.lock().len(), 2);

    watcher.dispose().await?;
    reload.dispose().await?;
    context.fiber().dispose().await
}

#[tokio::test]
async fn boot_root_include_recomposes_user_patches_without_dropping_app_layers()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let base = temporary.path().join("cordis.yml");
    let user = temporary.path().join("cordis.patch.yml");
    std::fs::write(
        &base,
        "- id: current\n  name: value\n  config: { value: base }\n",
    )?;
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "value",
        Plugin::new("value", std::iter::empty::<&str>(), |context, config| {
            Box::pin(async move {
                anyhow::ensure!(config["fail"] != true, "candidate config failed");
                context.provide(CURRENT, Arc::new(config))?;
                Ok(())
            })
        }),
    )?;
    let app_patch = parse_patch_list_yaml("- id: current\n  config: { value: generated }\n")?;
    let app = boot(
        "seekdeep-test-bin",
        &base,
        &catalog,
        BootOptions {
            patches: app_patch.clone(),
            ..BootOptions::default()
        },
    )
    .await?;
    let context = app.context().clone();
    assert_eq!(context.get(CURRENT).expect("initial")["value"], "generated");

    let failures = Arc::new(Mutex::new(Vec::new()));
    let failure: seekdeep_app_boot::ConfigRefreshFailure = Arc::new({
        let failures = failures.clone();
        move |_, error| failures.lock().push(error.to_string())
    });
    let compose: PatchComposer =
        Arc::new(move |user| app_patch.iter().cloned().chain(user).collect());
    let watcher = watch_boot_user_patches(BootUserPatchWatchOptions {
        bin_name: "seekdeep-test-bin".to_owned(),
        filename: user.clone(),
        compose,
        context: context.clone(),
        registry: ConfigWatchRegistry::new(),
        failure,
    })
    .await?;

    std::fs::write(&user, "- id: current\n  config: { value: live }\n")?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "live")
        },
        "boot user patch addition",
    )
    .await;
    std::fs::write(&user, "- id: current\n  config: { fail: true }\n")?;
    eventually(|| failures.lock().len() == 1, "boot user patch failure").await;
    assert_eq!(context.get(CURRENT).expect("last good")["value"], "live");
    std::fs::remove_file(&user)?;
    eventually(
        || {
            context
                .get(CURRENT)
                .is_some_and(|value| value["value"] == "generated")
        },
        "boot user patch removal",
    )
    .await;
    assert!(!std::fs::read_to_string(&base)?.contains("generated"));

    app.dispose().await?;
    watcher.dispose().await
}

#[tokio::test]
async fn boot_watcher_requires_the_live_loader_and_root_include() -> anyhow::Result<()> {
    fn options(context: Context, filename: &Path) -> BootUserPatchWatchOptions {
        BootUserPatchWatchOptions {
            bin_name: "seekdeep-test-bin".to_owned(),
            filename: filename.to_path_buf(),
            compose: Arc::new(|patches| patches),
            context,
            registry: ConfigWatchRegistry::new(),
            failure: Arc::new(|_, _| {}),
        }
    }

    let temporary = tempfile::tempdir()?;
    let context = Context::new();
    let error = watch_boot_user_patches(options(
        context.clone(),
        &temporary.path().join("missing.patch.yml"),
    ))
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires the Cordis Loader service")
    );

    let composition = PluginCatalog::new().load_yaml(&context, "[]\n").await?;
    let error = watch_boot_user_patches(options(
        context.clone(),
        &temporary.path().join("missing.patch.yml"),
    ))
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires the root Include entry")
    );
    composition.dispose().await?;
    context.fiber().dispose().await
}

#[tokio::test]
async fn boot_watcher_returns_a_noop_when_ownership_turns_inactive_during_registration()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let base = temporary.path().join("base.yml");
    let patch = temporary.path().join("cordis.patch.yml");
    std::fs::write(&base, "[]\n")?;
    let context = Context::new();
    let composition = PluginCatalog::new().load_yaml(&context, "[]\n").await?;
    context
        .get(LOADER)
        .unwrap()
        .create_entry(
            Entry::file_include(EntryId::new("include")?, base.to_string_lossy(), Vec::new())?,
            EntryParent::Root,
            None,
        )
        .await?;

    let dead_owner = Fiber::active_child("disposed watcher owner");
    dead_owner.dispose().await?;
    let dead_context = context.with_fiber(dead_owner);
    let registry = ConfigWatchRegistry::new();
    let watcher = watch_boot_user_patches(BootUserPatchWatchOptions {
        bin_name: "seekdeep-test-bin".to_owned(),
        filename: patch.clone(),
        compose: Arc::new(|patches| patches),
        context: dead_context,
        registry: registry.clone(),
        failure: Arc::new(|_, _| {}),
    })
    .await?;
    watcher.dispose().await?;

    let live = watch_boot_user_patches(BootUserPatchWatchOptions {
        bin_name: "seekdeep-test-bin".to_owned(),
        filename: patch,
        compose: Arc::new(|patches| patches),
        context: context.clone(),
        registry,
        failure: Arc::new(|_, _| {}),
    })
    .await?;
    live.dispose().await?;
    composition.dispose().await?;
    context.fiber().dispose().await
}
