//! Provider-table resolution, atomic registration, and setup cancellation parity.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{
    LSP_CONFLICT, LSP_UNAVAILABLE, Lsp, LspError, LspOperation, LspPosition, LspQueryRequest,
};
use seekdeep_lsp_stdio::{Config, apply, plugin};
use seekdeep_subprocess::{
    SubprocessHandleRef, SubprocessLookupEnvironment, SubprocessRuntime, SubprocessService,
    SubprocessSpawnSpec, SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec,
};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Value, json};

fn local_context(cwd: &Path) -> (Context, Arc<Lsp>) {
    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    LocalFileSystem::install(
        &context,
        FsConfig {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            diff_basis_max_bytes: None,
        },
    )
    .unwrap();
    (context, lsp)
}

fn request(workspace: &Path) -> LspQueryRequest {
    LspQueryRequest {
        operation: LspOperation::GoToDefinition,
        file_path: "a.ts".to_owned(),
        position: LspPosition {
            line: 0.0,
            character: 0.0,
        },
        workspace_root: workspace.to_string_lossy().into_owned(),
    }
}

fn config(value: Value) -> Config {
    serde_json::from_value(value).unwrap()
}

fn error_code(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<LspError>().map(LspError::code)
}

#[cfg(unix)]
#[tokio::test]
async fn bare_path_resolution_missing_commands_and_executable_kinds_fail_at_load() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("ws");
    let bin = root.path().join("bin");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::create_dir(&bin).await.unwrap();
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\n")
        .await
        .unwrap();
    let executable = bin.join("fake-lsp");
    tokio::fs::write(&executable, "#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    let mut permissions = tokio::fs::metadata(&executable)
        .await
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&executable, permissions)
        .await
        .unwrap();

    let (context, lsp) = local_context(root.path());
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {"onpath": {
                "command": "fake-lsp",
                "env": {"PATH": bin},
                "extensionToLanguage": {".ts": "typescript"}
            }}}),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    let error = lsp.query(request(&workspace), None).await.unwrap_err();
    assert_ne!(error_code(&error), Some(LSP_UNAVAILABLE));
    fiber.dispose().await.unwrap();
    assert_eq!(
        error_code(&lsp.query(request(&workspace), None).await.unwrap_err()),
        Some(LSP_UNAVAILABLE)
    );
    context.fiber().restart().await.unwrap();

    for (command, expected) in [
        (
            "definitely-not-a-real-lsp-binary-xyz".to_owned(),
            "was not found on PATH",
        ),
        (
            workspace.to_string_lossy().into_owned(),
            "is not an executable file",
        ),
    ] {
        let (context, _) = local_context(root.path());
        let fiber = context
            .plugin(
                plugin(),
                json!({"servers": {"bad": {
                    "command": command,
                    "env": {"PATH": "::"},
                    "extensionToLanguage": {".ts": "typescript"}
                }}}),
            )
            .unwrap();
        let error = fiber.await_settled().await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        context.fiber().restart().await.unwrap();
    }

    let not_executable = root.path().join("not-executable.txt");
    tokio::fs::write(&not_executable, "plain text")
        .await
        .unwrap();
    let (context, _) = local_context(root.path());
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {"bad": {
                "command": not_executable,
                "extensionToLanguage": {".ts": "typescript"}
            }}}),
        )
        .unwrap();
    assert!(
        fiber
            .await_settled()
            .await
            .unwrap_err()
            .to_string()
            .contains("is not an executable file")
    );
    context.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn every_numeric_and_table_validation_failure_is_early_and_exact() {
    let root = tempfile::tempdir().unwrap();
    let (context, _) = local_context(root.path());
    for (value, expected) in [
        (
            json!({"servers": {}}),
            "servers must contain at least one server",
        ),
        (
            json!({"servers": {"": {
                "command": "x", "extensionToLanguage": {".ts": "typescript"}
            }}}),
            "server ids must be non-empty strings",
        ),
        (
            json!({"servers": {"bad-budget": {
                "command": "x", "extensionToLanguage": {".ts": "typescript"}, "killGraceMs": 0
            }}}),
            "servers.bad-budget.killGraceMs must be a positive integer",
        ),
        (
            json!({"servers": {"bad-cap": {
                "command": "x", "extensionToLanguage": {".ts": "typescript"}, "maxDocumentBytes": 0
            }}}),
            "servers.bad-cap.maxDocumentBytes must be a positive integer",
        ),
        (
            json!({"servers": {"bad-timer": {
                "command": "x", "extensionToLanguage": {".ts": "typescript"},
                "shutdownTimeoutMs": 2_147_483_648_f64
            }}}),
            "servers.bad-timer.shutdownTimeoutMs",
        ),
        (
            json!({"servers": {"bad-timer": {
                "command": "x", "extensionToLanguage": {".ts": "typescript"},
                "killGraceMs": 2_147_483_648_f64
            }}}),
            "servers.bad-timer.killGraceMs",
        ),
    ] {
        let fiber = context.plugin(plugin(), value).unwrap();
        let error = fiber.await_settled().await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        fiber.dispose().await.unwrap();
    }
    context.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn all_lookups_precede_publication_and_registration_conflicts_roll_back() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("a.ts"), "x").await.unwrap();
    let executable = env!("CARGO_BIN_EXE_seekdeep-lsp-stdio-fixture");
    let (context, lsp) = local_context(root.path());
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {
                "valid": {"command": executable, "extensionToLanguage": {".ts": "typescript"}},
                "missing": {"command": "definitely-not-a-real-lsp-binary-xyz", "extensionToLanguage": {".py": "python"}}
            }}),
        )
        .unwrap();
    assert!(
        fiber
            .await_settled()
            .await
            .unwrap_err()
            .to_string()
            .contains("was not found on PATH")
    );
    assert_eq!(
        error_code(&lsp.query(request(&workspace), None).await.unwrap_err()),
        Some(LSP_UNAVAILABLE)
    );
    fiber.dispose().await.unwrap();

    let error = apply(
        &context,
        config(json!({"servers": {
            "first": {"command": executable, "extensionToLanguage": {".ts": "typescript"}},
            "second": {"command": executable, "extensionToLanguage": {".ts": "typescript"}}
        }})),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&error), Some(LSP_CONFLICT));
    assert_eq!(
        error_code(&lsp.query(request(&workspace), None).await.unwrap_err()),
        Some(LSP_UNAVAILABLE)
    );
    context.fiber().restart().await.unwrap();
}

#[derive(Debug, Default)]
struct CoordinatedLookupRuntime {
    slow_started: AtomicBool,
    slow_aborted: AtomicBool,
    release_cleanup: AtomicBool,
    notify: tokio::sync::Notify,
    observed_signal: parking_lot::Mutex<Option<AbortSignal>>,
}

impl CoordinatedLookupRuntime {
    async fn wait_flag(&self, flag: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.notify.notified();
                if flag.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("coordinated lookup timed out");
    }

    fn release(&self) {
        self.release_cleanup.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

#[async_trait]
impl SubprocessRuntime for CoordinatedLookupRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        _env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        let signal = signal.ok_or_else(|| anyhow::anyhow!("missing setup signal"))?;
        *self.observed_signal.lock() = Some(signal.clone());
        if command == "slow-lsp" || command == "pending-lsp" {
            self.slow_started.store(true, Ordering::Release);
            self.notify.notify_waiters();
            signal.cancelled().await;
            self.slow_aborted.store(true, Ordering::Release);
            self.notify.notify_waiters();
            if command == "slow-lsp" {
                self.wait_flag(&self.release_cleanup).await;
            }
            anyhow::bail!(
                signal
                    .reason()
                    .and_then(|reason| reason.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "aborted".to_owned())
            );
        }
        self.wait_flag(&self.slow_started).await;
        anyhow::bail!("lookup failed")
    }

    fn spawn(&self, _spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        anyhow::bail!("test runtime does not spawn")
    }

    async fn spawn_terminal(
        &self,
        _spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        anyhow::bail!("test runtime does not spawn terminals")
    }
}

fn coordinated_context(runtime: Arc<CoordinatedLookupRuntime>) -> Context {
    let context = Context::new();
    Arc::new(Lsp::new()).provide(&context).unwrap();
    SubprocessService::new(runtime).provide(&context).unwrap();
    let filesystem = seekdeep_fs_local::LocalFileSystem::new(FsConfig::default()).unwrap();
    seekdeep_fs::FileSystemService::new(filesystem)
        .provide(&context)
        .unwrap();
    context
}

#[tokio::test]
async fn failed_setup_aborts_and_joins_sibling_lookups_before_rejecting() {
    let runtime = Arc::new(CoordinatedLookupRuntime::default());
    let context = coordinated_context(runtime.clone());
    let config = config(json!({"servers": {
        "slow": {"command": "slow-lsp", "extensionToLanguage": {".ts": "typescript"}},
        "failing": {"command": "failing-lsp", "extensionToLanguage": {".js": "javascript"}}
    }}));
    let apply_context = context.clone();
    let loading = tokio::spawn(async move { apply(&apply_context, config).await });
    runtime.wait_flag(&runtime.slow_aborted).await;
    assert!(!loading.is_finished());
    runtime.release();
    let error = loading.await.unwrap().unwrap_err();
    assert_eq!(error.to_string(), "lookup failed");
    context.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn disposing_the_loading_plugin_cancels_only_its_setup_lookup() {
    let runtime = Arc::new(CoordinatedLookupRuntime::default());
    let context = coordinated_context(runtime.clone());
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {"pending": {
                "command": "pending-lsp", "extensionToLanguage": {".ts": "typescript"}
            }}}),
        )
        .unwrap();
    runtime.wait_flag(&runtime.slow_started).await;
    let unrelated = context
        .plugin(
            Plugin::new("unrelated", std::iter::empty::<String>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
            Value::Null,
        )
        .unwrap();
    unrelated.await_settled().await.unwrap();
    unrelated.dispose().await.unwrap();
    assert!(
        !runtime
            .observed_signal
            .lock()
            .as_ref()
            .unwrap()
            .is_aborted()
    );

    let dispose_fiber = fiber.clone();
    let disposing = tokio::spawn(async move { dispose_fiber.dispose().await });
    runtime.wait_flag(&runtime.slow_aborted).await;
    let error = fiber.await_settled().await.unwrap_err();
    assert!(error.to_string().contains("lsp-stdio setup disposed"));
    disposing.await.unwrap().unwrap();
    assert!(
        runtime
            .observed_signal
            .lock()
            .as_ref()
            .unwrap()
            .is_aborted()
    );
    context.fiber().restart().await.unwrap();
}
