//! File round-trip, locking, and watcher parity tests.

use std::{path::Path, sync::Arc, time::Duration};

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SETTINGS, SettingsRegisterOptions, settings_namespace};
use seekdeep_settings_file::invariant::{INVARIANT_NAME, PACKAGE_NAME, register_invariant};
use seekdeep_settings_file::{
    FileSettingsConfig, SETTINGS_FILENAME, SettingsFormat, install, resolve_spec,
};
use serde_json::json;
use tempfile::TempDir;

struct Harness {
    context: Context,
    fiber: Arc<seekdeep_cordis::PluginFiber>,
}

#[tokio::test]
async fn invariant_reserves_renamed_identity_and_releases_for_replacement() {
    assert_eq!(INVARIANT_NAME, "settings-file-invariant");
    assert_eq!(PACKAGE_NAME, "@deepseek-ai/seekdeep-settings-file");
    let context = Context::new();
    let registry = Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(
        register_invariant(&registry)
            .unwrap_err()
            .to_string()
            .contains(PACKAGE_NAME)
    );
    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}

impl Harness {
    async fn boot(path: &Path, watch: bool) -> Self {
        let context = Context::new();
        let fiber = install(
            &context,
            FileSettingsConfig {
                path: Some(path.to_path_buf()),
                watch,
                debounce_ms: 5.0,
                ..FileSettingsConfig::default()
            },
        )
        .unwrap();
        fiber.await_settled().await.unwrap();
        Self { context, fiber }
    }
}

fn theme_schema() -> Schema {
    Schema::object([
        ("theme", Schema::string().with_default("dark")),
        ("fontSize", Schema::number().with_default(14)),
        ("tags", Schema::array(Schema::string())),
    ])
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn spec_defaults_under_seekdeep_home_and_rejects_unknown_extensions() {
    let home = TempDir::new().unwrap();
    let spec = resolve_spec(&FileSettingsConfig {
        seekdeep_home: Some(home.path().to_path_buf()),
        ..FileSettingsConfig::default()
    })
    .unwrap();
    assert_eq!(spec.filename, home.path().join(SETTINGS_FILENAME));
    assert_eq!(spec.format, SettingsFormat::Yaml);
    assert!(spec.watch);
    assert!((spec.debounce_ms - 100.0).abs() < f64::EPSILON);
    let error = resolve_spec(&FileSettingsConfig {
        path: Some(home.path().join("settings.toml")),
        ..FileSettingsConfig::default()
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("extension \".toml\" is not supported")
    );
}

#[tokio::test]
async fn absent_document_resolves_defaults_and_prepare_is_owner_only_and_idempotent() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("nested/settings.yaml");
    let harness = Harness::boot(&path, false).await;
    let settings = harness.context.get(SETTINGS).unwrap();
    let scope = settings
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(scope.get()["theme"], "dark");
    assert_eq!(settings.document_path().as_deref(), Some(path.as_path()));
    assert_eq!(
        settings.prepare_document().await.unwrap().as_deref(),
        Some(path.as_path())
    );
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "");
    tokio::fs::write(&path, "ui-theme:\n  theme: light\n")
        .await
        .unwrap();
    settings.prepare_document().await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        "ui-theme:\n  theme: light\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            tokio::fs::metadata(&path)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn boot_reads_yaml_and_json_and_invalid_roots_fail_loud() {
    let directory = TempDir::new().unwrap();
    let yaml = directory.path().join("settings.yaml");
    tokio::fs::write(&yaml, "ui-theme:\n  theme: light\n")
        .await
        .unwrap();
    let harness = Harness::boot(&yaml, false).await;
    let settings = harness.context.get(SETTINGS).unwrap();
    let scope = settings
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(scope.get()["theme"], "light");

    let json_path = directory.path().join("settings.json");
    tokio::fs::write(&json_path, r#"{"ui-theme":{"fontSize":20}}"#)
        .await
        .unwrap();
    let json_harness = Harness::boot(&json_path, false).await;
    let json_scope = json_harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &json_harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(json_scope.get()["fontSize"], 20);

    let invalid = directory.path().join("invalid.yaml");
    tokio::fs::write(&invalid, "[not, a, map]\n").await.unwrap();
    let context = Context::new();
    let fiber = install(
        &context,
        FileSettingsConfig {
            path: Some(invalid),
            watch: false,
            ..FileSettingsConfig::default()
        },
    )
    .unwrap();
    assert!(
        fiber
            .await_settled()
            .await
            .unwrap_err()
            .to_string()
            .contains("must be a map")
    );
}

#[tokio::test]
async fn yaml_updates_preserve_unregistered_sections_and_nested_comments() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(
        &path,
        concat!(
            "# document comment\n",
            "ui-theme:\n",
            "  # theme comment\n",
            "  theme: light # own-line\n",
            "  fontSize: 16 # keep sibling\n",
            "  tags: # unchanged array\n",
            "    - one # item comment\n",
            "other-plugin:\n",
            "  keep: yes # untouched\n",
        ),
    )
    .await
    .unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    scope.update(json!({ "theme": "dark" })).await.unwrap();
    let output = tokio::fs::read_to_string(&path).await.unwrap();
    for retained in [
        "# document comment",
        "# theme comment",
        "theme: dark # own-line",
        "fontSize: 16 # keep sibling",
        "tags: # unchanged array",
        "- one # item comment",
        "other-plugin:",
        "keep: yes # untouched",
    ] {
        assert!(
            output.contains(retained),
            "missing {retained:?} in:\n{output}"
        );
    }
    scope
        .replace(json!({ "theme": "dark", "tags": ["one"] }))
        .await
        .unwrap();
    let output = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(!output.contains("fontSize:"), "{output}");
    assert!(output.contains("# theme comment"));
    assert!(output.contains("other-plugin:"));
}

#[tokio::test]
async fn unchanged_arrays_keep_comments_and_changed_arrays_replace_wholesale() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(
        &path,
        concat!(
            "workspace:\n",
            "  tags:\n",
            "    # pinned by hand\n",
            "    - alpha\n",
            "  label: draft\n",
        ),
    )
    .await
    .unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("workspace").unwrap(),
            Schema::object([
                ("tags", Schema::array(Schema::string())),
                ("label", Schema::string().with_default("")),
            ]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    scope.update(json!({ "label": "final" })).await.unwrap();
    let untouched = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(untouched.contains("# pinned by hand"), "{untouched}");
    assert!(untouched.contains("label: final"), "{untouched}");
    let untouched_value: serde_json::Value =
        serde_yml::from_str(&untouched).unwrap_or_else(|error| panic!("{error}:\n{untouched}"));
    assert_eq!(untouched_value["workspace"]["label"], "final");

    scope.update(json!({ "tags": ["beta"] })).await.unwrap();
    let replaced = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(!replaced.contains("# pinned by hand"), "{replaced}");
    assert!(replaced.contains("- beta"), "{replaced}");
    let parsed: serde_json::Value =
        serde_yml::from_str(&replaced).unwrap_or_else(|error| panic!("{error}:\n{replaced}"));
    assert_eq!(parsed["workspace"]["tags"], json!(["beta"]));
    assert_eq!(parsed["workspace"]["label"], "final");
}

#[tokio::test]
async fn json_writes_are_pretty_and_cross_namespace_operations_do_not_drop_sections() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.json");
    let harness = Harness::boot(&path, false).await;
    let settings = harness.context.get(SETTINGS).unwrap();
    let alpha = settings
        .register(
            &harness.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let beta = settings
        .register(
            &harness.context,
            &settings_namespace("beta").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    tokio::try_join!(
        alpha.update(json!({ "value": 1 })),
        beta.update(json!({ "value": 2 }))
    )
    .unwrap();
    let text = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        json!({ "alpha": { "value": 1 }, "beta": { "value": 2 } })
    );
}

#[tokio::test]
async fn two_provider_instances_coordinate_with_the_writer_lock() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    let first = Harness::boot(&path, false).await;
    let second = Harness::boot(&path, false).await;
    let alpha = first
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &first.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let beta = second
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &second.context,
            &settings_namespace("beta").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    tokio::try_join!(
        async {
            for value in 1..=5 {
                alpha.update(json!({ "value": value })).await?;
            }
            Ok::<_, anyhow::Error>(())
        },
        async {
            for value in 1..=5 {
                beta.update(json!({ "value": value })).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
    )
    .unwrap();
    let text = tokio::fs::read_to_string(&path).await.unwrap();
    let document: serde_json::Value = serde_yml::from_str(&text).unwrap();
    assert_eq!(document["alpha"]["value"], 5, "{text}");
    assert_eq!(document["beta"]["value"], 5, "{text}");
    assert!(!path.with_extension("yaml.lock").exists());

    let third = Harness::boot(&path, false).await;
    let settings = third.context.get(SETTINGS).unwrap();
    let third_alpha = settings
        .register(
            &third.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let third_beta = settings
        .register(
            &third.context,
            &settings_namespace("beta").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    assert_eq!(third_alpha.get()["value"], 5);
    assert_eq!(third_beta.get()["value"], 5);
}

fn writer_lock_path(path: &Path) -> std::path::PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    lock.into()
}

#[tokio::test]
async fn busy_writer_lock_is_waited_for_and_an_old_lock_is_never_stolen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(&path, "alpha:\n  value: 4\n")
        .await
        .unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let lock = writer_lock_path(&path);
    tokio::fs::write(&lock, "holder\n").await.unwrap();
    let release = {
        let lock = lock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            tokio::fs::remove_file(lock).await.unwrap();
        })
    };
    let started = tokio::time::Instant::now();
    scope.update(json!({ "value": 7 })).await.unwrap();
    release.await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(
        tokio::fs::read_to_string(&path)
            .await
            .unwrap()
            .contains("value: 7")
    );

    tokio::fs::write(&lock, "slow-holder\n").await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), scope.update(json!({ "value": 9 })))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("timed out waiting for the writer lock")
    );
    assert_eq!(
        tokio::fs::read_to_string(&lock).await.unwrap(),
        "slow-holder\n"
    );
    assert_eq!(scope.get()["value"], 7);
    assert!(
        tokio::fs::read_to_string(&path)
            .await
            .unwrap()
            .contains("value: 7")
    );
}

#[tokio::test]
async fn invalid_unobserved_document_fails_write_without_overwriting_or_committing() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(&path, "ui-theme:\n  theme: light\n")
        .await
        .unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    let broken = "ui-theme: [unclosed\n  flow: {\n";
    tokio::fs::write(&path, broken).await.unwrap();
    let error = scope
        .update(json!({ "theme": "darker" }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid document"));
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), broken);
    assert_eq!(scope.get()["theme"], "light");
    assert!(!writer_lock_path(&path).exists());
}

#[tokio::test]
async fn comment_only_document_and_directory_collision_recover_without_data_loss() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(&path, "# reserved for future settings\n")
        .await
        .unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    scope.update(json!({ "theme": "light" })).await.unwrap();
    let text = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(text.contains("# reserved for future settings"), "{text}");
    assert!(text.contains("theme: light"), "{text}");

    tokio::fs::remove_file(&path).await.unwrap();
    tokio::fs::create_dir(&path).await.unwrap();
    assert!(scope.update(json!({ "theme": "blocked" })).await.is_err());
    assert_eq!(scope.get()["theme"], "light");
    assert!(!writer_lock_path(&path).exists());
    tokio::fs::remove_dir(&path).await.unwrap();
    scope.update(json!({ "theme": "recovered" })).await.unwrap();
    assert_eq!(scope.get()["theme"], "recovered");
    assert!(
        tokio::fs::read_to_string(&path)
            .await
            .unwrap()
            .contains("theme: recovered")
    );
}

#[tokio::test]
async fn an_unobserved_external_edit_is_folded_into_the_next_write() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(&path, "alpha:\n  value: 1\n")
        .await
        .unwrap();
    let harness = Harness::boot(&path, false).await;
    let settings = harness.context.get(SETTINGS).unwrap();
    let alpha = settings
        .register(
            &harness.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    tokio::fs::write(&path, "alpha:\n  value: 1\nbeta:\n  external: true\n")
        .await
        .unwrap();
    alpha.update(json!({ "value": 3 })).await.unwrap();
    let document: serde_json::Value =
        serde_yml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
    assert_eq!(document["alpha"]["value"], 3);
    assert_eq!(document["beta"]["external"], true);
}

#[tokio::test]
async fn watcher_keeps_last_good_over_invalid_edit_then_recovers_and_handles_removal() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    tokio::fs::write(&path, "ui-theme:\n  theme: light\n")
        .await
        .unwrap();
    let harness = Harness::boot(&path, true).await;
    let settings = harness.context.get(SETTINGS).unwrap();
    let scope = settings
        .register(
            &harness.context,
            &settings_namespace("ui-theme").unwrap(),
            theme_schema(),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    tokio::fs::write(&path, "ui-theme: [unclosed\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(scope.get()["theme"], "light");
    tokio::fs::write(&path, "ui-theme:\n  theme: dark\n")
        .await
        .unwrap();
    wait_until(|| scope.get()["theme"] == "dark").await;
    tokio::fs::remove_file(&path).await.unwrap();
    wait_until(|| settings.describe(false)[0].user.is_none()).await;
    assert_eq!(
        scope.get(),
        json!({ "theme": "dark", "fontSize": 14, "tags": [] })
    );
    // The theme schema default is dark, so removal resolves through defaults.
    harness.fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn atomic_replacement_does_not_follow_a_target_symlink() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("settings.yaml");
    let victim = directory.path().join("victim.txt");
    tokio::fs::write(&victim, "{}\n").await.unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&victim, &path).unwrap();
    let harness = Harness::boot(&path, false).await;
    let scope = harness
        .context
        .get(SETTINGS)
        .unwrap()
        .register(
            &harness.context,
            &settings_namespace("alpha").unwrap(),
            Schema::object([("value", Schema::number().with_default(0))]),
            SettingsRegisterOptions::default(),
        )
        .unwrap();
    scope.update(json!({ "value": 9 })).await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&victim).await.unwrap(), "{}\n");
    assert!(
        !tokio::fs::symlink_metadata(&path)
            .await
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
