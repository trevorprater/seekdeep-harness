//! Transactional root boot and activation-audit parity.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use seekdeep_app_boot::{BootOptions, boot};
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_loader::profile_patch::parse_patch_list_yaml;
use seekdeep_loader::{
    Entry, EntryId, EntryParent, ExpressionEnvironment, LOADER, PluginCatalog, PluginSpecifier,
};

const PREPARED: ServiceKey<String> = ServiceKey::new("prepared");
const OBSERVED: ServiceKey<String> = ServiceKey::new("observed");
const RELATIVE_LOADED: ServiceKey<serde_json::Value> = ServiceKey::new("relativePluginLoaded");
const ABSOLUTE_LOADED: ServiceKey<serde_json::Value> = ServiceKey::new("absolutePluginLoaded");
const HOST_LOADED: ServiceKey<serde_json::Value> = ServiceKey::new("harnessPluginLoaded");
const SHADOW_LOADED: ServiceKey<serde_json::Value> = ServiceKey::new("shadowPluginLoaded");

fn config(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("cordis.yml");
    std::fs::write(&path, source).unwrap();
    (temporary, path)
}

#[tokio::test]
async fn boots_composed_rows_after_host_preparation_and_disposes_as_one_tree() -> anyhow::Result<()>
{
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "host",
        Plugin::new("host", std::iter::empty::<&str>(), |_, _| {
            Box::pin(async { Ok(()) })
        }),
    )?;
    catalog.register_named(
        "reader",
        Plugin::new("reader", ["prepared"], |context, config| {
            Box::pin(async move {
                let prepared = context
                    .get(PREPARED)
                    .ok_or_else(|| anyhow::anyhow!("prepared missing"))?;
                context.provide(
                    OBSERVED,
                    Arc::new(format!("{prepared}:{}", config["value"])),
                )?;
                Ok(())
            })
        }),
    )?;
    let (_temporary, path) = config("- id: reader\n  name: reader\n  config:\n    value: base\n");
    let prepared = Arc::new(AtomicBool::new(false));
    let disposed = Arc::new(AtomicBool::new(false));
    let app = boot(
        "seekdeep-test-bin",
        &path,
        &catalog,
        BootOptions {
            patches: parse_patch_list_yaml("- id: reader\n  config:\n    value: patched\n")?,
            prepare: Some(Arc::new({
                let prepared = prepared.clone();
                let disposed = disposed.clone();
                move |context: Context| {
                    let prepared = prepared.clone();
                    let disposed = disposed.clone();
                    Box::pin(async move {
                        let loader = context.get(LOADER).expect("loader before prepare");
                        assert!(loader.entries()?.is_empty());
                        loader
                            .create_entry(
                                Entry::new(EntryId::new("host")?, PluginSpecifier::new("host")?),
                                EntryParent::Root,
                                None,
                            )
                            .await?;
                        context.provide(PREPARED, Arc::new("host".to_owned()))?;
                        context.own(EffectHandle::synchronous("prepare cleanup", move || {
                            disposed.store(true, Ordering::Release);
                            Ok(())
                        }))?;
                        prepared.store(true, Ordering::Release);
                        Ok(())
                    })
                }
            })),
            warn: None,
        },
    )
    .await?;
    assert!(prepared.load(Ordering::Acquire));
    assert_eq!(
        app.context().get(OBSERVED).as_deref().map(String::as_str),
        Some("host:\"patched\"")
    );
    assert_eq!(app.composition().expect("composition").fibers().len(), 2);
    assert_eq!(
        app.context()
            .get(LOADER)
            .expect("loader")
            .entries()?
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["host", "include", "reader"]
    );
    app.dispose().await?;
    assert!(disposed.load(Ordering::Acquire));
    Ok(())
}

#[tokio::test]
async fn preparation_failure_is_labelled_and_rolls_back_partial_host_state() {
    let catalog = PluginCatalog::new();
    let (_temporary, path) = config("[]\n");
    let disposed = Arc::new(AtomicBool::new(false));
    let error = boot(
        "seekdeep-test-bin",
        &path,
        &catalog,
        BootOptions {
            patches: Vec::new(),
            prepare: Some(Arc::new({
                let disposed = disposed.clone();
                move |context: Context| {
                    let disposed = disposed.clone();
                    Box::pin(async move {
                        context.own(EffectHandle::synchronous("partial host", move || {
                            disposed.store(true, Ordering::Release);
                            Ok(())
                        }))?;
                        anyhow::bail!("42")
                    })
                }
            })),
            warn: None,
        },
    )
    .await
    .expect_err("prepare failure");
    assert_eq!(
        error.to_string(),
        "seekdeep-test-bin: host preparation failed: 42"
    );
    assert!(disposed.load(Ordering::Acquire));
}

#[tokio::test]
async fn missing_failed_and_pending_plugins_never_return_a_half_boot() -> anyhow::Result<()> {
    let (_missing_temp, missing_path) = config("- id: ghost\n  name: absent\n");
    let missing = boot(
        "seekdeep-test-bin",
        &missing_path,
        &PluginCatalog::new(),
        BootOptions::default(),
    )
    .await
    .expect_err("missing plugin");
    assert!(missing.to_string().contains("plugin tree failed to load"));
    assert!(missing.to_string().contains("absent"));

    let failed_catalog = PluginCatalog::new();
    failed_catalog.register_named(
        "failure",
        Plugin::new("failure", std::iter::empty::<&str>(), |_, _| {
            Box::pin(async { anyhow::bail!("pinned activation failure") })
        }),
    )?;
    let (_failed_temp, failed_path) = config("- id: failing\n  name: failure\n");
    let failed = boot(
        "seekdeep-test-bin",
        &failed_path,
        &failed_catalog,
        BootOptions::default(),
    )
    .await
    .expect_err("failed plugin");
    assert!(failed.to_string().contains("pinned activation failure"));

    let pending_catalog = PluginCatalog::new();
    pending_catalog.register_named(
        "waiting",
        Plugin::new("waiting", ["neverProvided"], |_, _| {
            Box::pin(async { Ok(()) })
        }),
    )?;
    let (_pending_temp, pending_path) = config("- id: waiting\n  name: waiting\n");
    let pending = boot(
        "seekdeep-test-bin",
        &pending_path,
        &pending_catalog,
        BootOptions::default(),
    )
    .await
    .expect_err("pending plugin");
    assert!(pending.to_string().contains("1 entry did not activate"));
    assert!(
        pending
            .to_string()
            .contains("waiting for service: neverProvided")
    );
    Ok(())
}

#[tokio::test]
async fn consumer_before_provider_is_active_at_the_final_audit() -> anyhow::Result<()> {
    let catalog = PluginCatalog::new();
    catalog.register_named(
        "consumer",
        Plugin::new("consumer", ["prepared"], |context, _| {
            Box::pin(async move {
                let value = context
                    .get(PREPARED)
                    .ok_or_else(|| anyhow::anyhow!("prepared missing"))?;
                context.provide(OBSERVED, value)?;
                Ok(())
            })
        }),
    )?;
    catalog.register_named(
        "provider",
        Plugin::new("provider", std::iter::empty::<&str>(), |context, _| {
            Box::pin(async move {
                context.provide(PREPARED, Arc::new("ready".to_owned()))?;
                Ok(())
            })
        }),
    )?;
    let (_temporary, path) =
        config("- id: consumer\n  name: consumer\n- id: provider\n  name: provider\n");
    let app = boot("seekdeep-test-bin", &path, &catalog, BootOptions::default()).await?;
    assert_eq!(
        app.context().get(OBSERVED).as_deref().map(String::as_str),
        Some("ready")
    );
    app.dispose().await
}

#[tokio::test]
async fn boot_exposes_seekdeep_home_and_config_directory_to_expressions() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let path = temporary.path().join("cordis.yml");
    std::fs::write(
        &path,
        concat!(
            "- id: capture\n",
            "  name: capture\n",
            "  config:\n",
            "    home: !!js seekdeepHomePath('sessions')\n",
            "    base: !!js baseUrl\n",
        ),
    )?;
    let catalog = PluginCatalog::new().with_expression_environment(ExpressionEnvironment::new(
        std::collections::BTreeMap::new(),
        temporary.path().to_path_buf(),
        "/bin/seekdeep".into(),
        "linux",
        "v22.0.0",
        home.clone(),
    ));
    catalog.register_named(
        "capture",
        Plugin::new("capture", std::iter::empty::<&str>(), |context, config| {
            Box::pin(async move {
                context.provide(
                    OBSERVED,
                    Arc::new(format!("{}|{}", config["home"], config["base"])),
                )?;
                Ok(())
            })
        }),
    )?;
    let app = boot("seekdeep-test-bin", &path, &catalog, BootOptions::default()).await?;
    let expected = format!(
        "\"{}\"|\"file://{}/\"",
        home.join("sessions").display(),
        temporary.path().display()
    );
    assert_eq!(
        app.context().get(OBSERVED).as_deref().map(String::as_str),
        Some(expected.as_str())
    );
    app.dispose().await
}

#[tokio::test]
async fn boot_resolves_relative_absolute_and_host_owned_bare_plugins() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("cordis.yml");
    let absolute = temporary.path().join("absolute.mjs");
    let harness = tempfile::tempdir()?;
    let shadow = temporary
        .path()
        .join("node_modules/@seekdeep-ai/seekdeep-system-prompt");
    let host = harness
        .path()
        .join("node_modules/@seekdeep-ai/seekdeep-system-prompt");
    std::fs::create_dir_all(&shadow)?;
    std::fs::create_dir_all(&host)?;
    for (directory, service) in [
        (&shadow, "shadowPluginLoaded"),
        (&host, "harnessPluginLoaded"),
    ] {
        std::fs::write(
            directory.join("package.json"),
            r#"{"type":"module","exports":"./index.mjs"}"#,
        )?;
        std::fs::write(
            directory.join("index.mjs"),
            format!("export function apply(ctx) {{ ctx.provide('{service}', true); }}\n"),
        )?;
    }
    std::fs::write(
        temporary.path().join("relative.mjs"),
        "export function apply(ctx) { ctx.provide('relativePluginLoaded', true); }\n",
    )?;
    std::fs::write(
        &absolute,
        "export function apply(ctx) { ctx.provide('absolutePluginLoaded', true); }\n",
    )?;
    std::fs::write(
        &path,
        format!(
            concat!(
                "- id: host\n",
                "  name: '@seekdeep-ai/seekdeep-system-prompt'\n",
                "- id: relative\n",
                "  name: ./relative.mjs\n",
                "- id: absolute\n",
                "  name: {}\n",
            ),
            serde_json::to_string(&absolute.to_string_lossy())?
        ),
    )?;
    let catalog = PluginCatalog::new().with_bare_module_base(harness.path().join("entry.mjs"));
    let app = boot("seekdeep-test-bin", &path, &catalog, BootOptions::default()).await?;
    assert_eq!(
        app.context().get(HOST_LOADED).as_deref(),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        app.context().get(RELATIVE_LOADED).as_deref(),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        app.context().get(ABSOLUTE_LOADED).as_deref(),
        Some(&serde_json::json!(true))
    );
    assert!(app.context().get(SHADOW_LOADED).is_none());
    app.dispose().await
}

#[tokio::test]
async fn javascript_surface_can_dispose_the_root_during_startup_without_a_half_boot()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("cordis.yml");
    std::fs::write(
        temporary.path().join("exit.mjs"),
        "export function apply(ctx) { void ctx.root.fiber.dispose(); }\n",
    )?;
    std::fs::write(&path, "- id: exit\n  name: ./exit.mjs\n")?;
    let app = boot(
        "seekdeep-test-bin",
        &path,
        &PluginCatalog::new(),
        BootOptions::default(),
    )
    .await?;
    assert!(app.composition().is_none());
    let context = app.context().clone();
    app.dispose().await?;
    assert!(context.get(LOADER).is_none());
    Ok(())
}
