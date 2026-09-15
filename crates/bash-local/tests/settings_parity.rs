//! Dynamic `shell` settings-section parity for the local Bash provider.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_bash_local::{Config, LocalBashExecutor, apply};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_settings::{SettingsDocument, SettingsService, SettingsStorage};
use seekdeep_shell::{ShellExecRequest, ShellExecutor, shell_settings_namespace};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Map, Value, json};

#[derive(Debug, Default)]
struct MemoryStorage {
    document: Mutex<SettingsDocument>,
}

#[async_trait]
impl SettingsStorage for MemoryStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &seekdeep_settings::SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.document.lock().insert(
            namespace.as_str().to_owned(),
            Value::Object(section.clone()),
        );
        Ok(())
    }
}

struct Bench {
    root: Context,
    settings_fiber: Arc<Fiber>,
    executor_fiber: Arc<Fiber>,
    settings: Arc<SettingsService>,
    bash: Arc<LocalBashExecutor>,
}

async fn boot() -> Bench {
    let root = Context::new();
    LocalSubprocessRuntime::install(&root).expect("subprocess");
    let settings_fiber = Fiber::active_child("settings-provider");
    let settings_context = root.with_fiber(settings_fiber.clone());
    let settings = SettingsService::install(&settings_context, Arc::new(MemoryStorage::default()))
        .await
        .expect("settings");
    let executor_fiber = Fiber::active_child("bash-provider");
    let executor_context = root.with_fiber(executor_fiber.clone());
    let bash = apply(
        &executor_context,
        Config {
            timeout_ms: 60_000.0,
            ..Config::default()
        },
    )
    .await
    .expect("bash");
    Bench {
        root,
        settings_fiber,
        executor_fiber,
        settings,
        bash,
    }
}

fn assert_number(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "expected {expected}, got {actual}"
    );
}

#[tokio::test]
async fn user_layer_validates_updates_every_read_and_falls_back_on_provider_detach() {
    let bench = boot().await;
    let namespace = shell_settings_namespace();
    assert_number(bench.bash.config().timeout_ms, 60_000.0);

    bench
        .settings
        .update(&namespace, json!({"timeoutMs": 5_000}), None)
        .await
        .expect("update timeout");
    assert_number(bench.bash.config().timeout_ms, 5_000.0);

    for (patch, expected) in [
        (json!({"timeoutMs": 0}), "positive finite"),
        (
            json!({"graceMs": 9_007_199_254_740_991_u64}),
            "graceMs must be no greater than",
        ),
    ] {
        let error = bench
            .settings
            .update(&namespace, patch, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
    assert_number(bench.bash.config().timeout_ms, 5_000.0);

    bench
        .settings
        .update(
            &namespace,
            json!({"maxOutputBytes": 1_024, "cwd": "/tmp"}),
            None,
        )
        .await
        .expect("update execution settings");
    let spec = bench
        .bash
        .resolve(ShellExecRequest::new("true"))
        .expect("resolve");
    assert_number(spec.stdout_max_bytes, 1_024.0);
    assert_eq!(spec.workdir, Path::new("/tmp"));

    bench
        .settings_fiber
        .dispose()
        .await
        .expect("detach settings");
    assert_number(bench.bash.config().timeout_ms, 60_000.0);
    assert_number(bench.bash.config().max_output_bytes, 64_000.0);
}

#[tokio::test]
async fn no_provider_keeps_composition_entry_and_executor_unload_releases_namespace() {
    let root = Context::new();
    LocalSubprocessRuntime::install(&root).expect("subprocess");
    let no_settings_fiber = Fiber::active_child("bash-without-settings");
    let no_settings_context = root.with_fiber(no_settings_fiber.clone());
    let bash = apply(
        &no_settings_context,
        Config {
            timeout_ms: 1_234.0,
            ..Config::default()
        },
    )
    .await
    .expect("bash");
    assert_number(bash.config().timeout_ms, 1_234.0);
    no_settings_fiber.dispose().await.expect("dispose bash");

    let bench = boot().await;
    assert!(
        bench
            .settings
            .describe(false)
            .iter()
            .any(|row| row.ns.as_str() == "shell")
    );
    bench
        .executor_fiber
        .dispose()
        .await
        .expect("dispose executor");
    assert!(
        bench
            .settings
            .describe(false)
            .iter()
            .all(|row| row.ns.as_str() != "shell")
    );
    assert!(bench.root.get(seekdeep_shell::SHELL).is_none());
}
