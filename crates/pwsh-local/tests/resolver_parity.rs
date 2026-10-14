//! Pure executable-resolution and `PowerShell` argv parity.

use std::{collections::BTreeMap, fs, path::Path};

use seekdeep_cordis::Context;
use seekdeep_pwsh_local::{
    Config, ENCODING_PREAMBLE, LocalPwshExecutor, PwshPlatform, apply,
    assert_serviceable_pwsh_config, candidate_pwsh_paths, resolve_pwsh_path,
};
use seekdeep_shell::{ShellExecRequest, ShellExecutor};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

fn environment(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn explicit_and_non_windows_resolution_match_the_source() {
    let env = environment(&[("PATH", "P:\\Store")]);
    assert_eq!(
        resolve_pwsh_path(Some("C:\\custom\\pwsh.exe"), &env, PwshPlatform::Windows),
        "C:\\custom\\pwsh.exe"
    );
    assert_eq!(
        resolve_pwsh_path(Some("pwsh"), &env, PwshPlatform::Windows),
        "pwsh"
    );
    assert_eq!(
        resolve_pwsh_path(Some(""), &env, PwshPlatform::Other),
        "pwsh"
    );
    assert_eq!(resolve_pwsh_path(None, &env, PwshPlatform::Other), "pwsh");
}

#[test]
fn windows_candidates_are_ordered_and_strip_path_quotes() {
    let env = environment(&[
        ("ProgramFiles", "P:\\Program Files"),
        ("SystemRoot", "S:\\Windows"),
        ("PATH", ";\"Q:\\quoted store\";;"),
    ]);
    assert_eq!(
        candidate_pwsh_paths(&env),
        vec![
            Path::new("P:\\Program Files")
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
            Path::new("Q:\\quoted store").join("pwsh.exe"),
            Path::new("S:\\Windows")
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        ]
    );
    assert_eq!(candidate_pwsh_paths(&BTreeMap::new()).len(), 2);
}

#[cfg(unix)]
#[test]
fn windows_probe_accepts_files_and_links_but_not_directories() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let missing = root.path().join("missing");
    let store = root.path().join("store");
    fs::create_dir_all(&store).expect("store");
    let executable = store.join("pwsh.exe");
    fs::write(&executable, "").expect("candidate");
    let env = environment(&[
        ("ProgramFiles", &missing.to_string_lossy()),
        ("SystemRoot", &missing.to_string_lossy()),
        ("PATH", &store.to_string_lossy()),
    ]);
    assert_eq!(
        resolve_pwsh_path(None, &env, PwshPlatform::Windows),
        executable.to_string_lossy()
    );

    fs::remove_file(&executable).expect("remove file");
    symlink(root.path().join("no-target.exe"), &executable).expect("link");
    assert_eq!(
        resolve_pwsh_path(None, &env, PwshPlatform::Windows),
        executable.to_string_lossy()
    );

    fs::remove_file(&executable).expect("remove link");
    fs::create_dir(&executable).expect("directory candidate");
    assert_eq!(resolve_pwsh_path(None, &env, PwshPlatform::Windows), "pwsh");
}

#[test]
fn configuration_validation_names_each_unserviceable_field() {
    for (config, field) in [
        (
            Config {
                timeout_ms: f64::NAN,
                ..Config::default()
            },
            "timeoutMs",
        ),
        (
            Config {
                max_timeout_ms: 0.0,
                ..Config::default()
            },
            "maxTimeoutMs",
        ),
        (
            Config {
                max_output_bytes: -1.0,
                ..Config::default()
            },
            "maxOutputBytes",
        ),
        (
            Config {
                max_spill_bytes: 0.0,
                ..Config::default()
            },
            "maxSpillBytes",
        ),
        (
            Config {
                grace_ms: seekdeep_util::timeout::MAX_TIMER_DELAY_MS + 1.0,
                ..Config::default()
            },
            "graceMs",
        ),
    ] {
        let error = assert_serviceable_pwsh_config(&config).unwrap_err();
        assert!(error.to_string().contains(field), "{error:#}");
    }
}

#[tokio::test]
async fn argv_keeps_the_command_in_one_utf8_prefixed_element() {
    let context = Context::new();
    LocalSubprocessRuntime::install(&context).expect("subprocess");
    let executable = if cfg!(windows) {
        "C:\\custom\\pwsh.exe"
    } else {
        "/opt/custom/pwsh"
    };
    let executor: std::sync::Arc<LocalPwshExecutor> = apply(
        &context,
        Config {
            pwsh_path: Some(executable.to_owned()),
            ..Config::default()
        },
    )
    .await
    .expect("executor");
    let spec = executor
        .resolve(ShellExecRequest::new("Write-Output 你好; Get-Date"))
        .expect("spec");
    assert_eq!(
        executor.argv(&spec),
        vec![
            executable,
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("{ENCODING_PREAMBLE}Write-Output 你好; Get-Date"),
        ]
    );
    assert!(ENCODING_PREAMBLE.contains("[Console]::OutputEncoding"));
    assert!(ENCODING_PREAMBLE.contains("$OutputEncoding"));
}

#[tokio::test]
async fn loader_validation_and_missing_dependency_fail_before_publishing_shell() {
    use seekdeep_pwsh_local::{plugin, resolve_config_value};
    use serde_json::json;
    assert!(resolve_config_value(&json!({"cwd": 42})).is_err());
    assert!(resolve_config_value(&json!({"graceMs": 0})).is_err());
    let empty = Context::new();
    assert!(
        apply(&empty, Config::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("requires subprocess")
    );
    let context = Context::new();
    LocalSubprocessRuntime::install(&context).unwrap();
    let mounted = context
        .plugin(plugin(), json!({"pwshPath": "fixture-pwsh"}))
        .unwrap();
    mounted.await_settled().await.unwrap();
    let shell = context.get(seekdeep_shell::SHELL).unwrap();
    assert!(shell.resolve(ShellExecRequest::new("probe")).is_ok());
    mounted.dispose().await.unwrap();
    assert!(context.get(seekdeep_shell::SHELL).is_none());
}

#[tokio::test]
async fn typed_builder_rejects_invalid_configuration_before_installing_settings() {
    let context = Context::new();
    let error = seekdeep_pwsh_local::build(
        &context,
        Config {
            grace_ms: 0.0,
            ..Config::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("graceMs"));
    assert!(context.get(seekdeep_shell::SHELL).is_none());
}

#[tokio::test]
async fn duplicate_shell_registration_keeps_the_original_provider() {
    let context = Context::new();
    LocalSubprocessRuntime::install(&context).unwrap();
    apply(&context, Config::default()).await.unwrap();
    let original = context.get(seekdeep_shell::SHELL).unwrap();
    let error = apply(&context, Config::default()).await.unwrap_err();
    assert!(error.to_string().contains("shell"));
    let installed = context.get(seekdeep_shell::SHELL).unwrap();
    assert!(std::sync::Arc::ptr_eq(&original, &installed));
}

#[tokio::test]
async fn loader_rejects_invalid_configuration_before_startup() {
    let context = Context::new();
    LocalSubprocessRuntime::install(&context).unwrap();
    let mounted = context
        .plugin(
            seekdeep_pwsh_local::plugin(),
            serde_json::json!({"graceMs": 0}),
        )
        .unwrap();
    let error = mounted.await_settled().await.unwrap_err();
    assert!(error.to_string().contains("graceMs"));
    assert!(context.get(seekdeep_shell::SHELL).is_none());
}

#[tokio::test]
async fn disposed_owner_cannot_install_settings_or_publish_a_shell() {
    let root = Context::new();
    LocalSubprocessRuntime::install(&root).unwrap();
    let owner = seekdeep_cordis::Fiber::active_child("disposed-pwsh-owner");
    owner.dispose().await.unwrap();
    let context = root.with_fiber(owner);
    let error = seekdeep_pwsh_local::build(&context, Config::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("inactive context"));
    assert!(root.get(seekdeep_shell::SHELL).is_none());
}

#[tokio::test]
async fn duplicate_plugin_mount_preserves_the_active_provider_and_its_cleanup() {
    let context = Context::new();
    LocalSubprocessRuntime::install(&context).unwrap();
    let first = context
        .plugin(seekdeep_pwsh_local::plugin(), serde_json::json!({}))
        .unwrap();
    first.await_settled().await.unwrap();
    let original = context.get(seekdeep_shell::SHELL).unwrap();
    let second = context
        .plugin(seekdeep_pwsh_local::plugin(), serde_json::json!({}))
        .unwrap();
    let error = second.await_settled().await.unwrap_err();
    assert!(error.to_string().contains("shell"));
    assert!(std::sync::Arc::ptr_eq(
        &original,
        &context.get(seekdeep_shell::SHELL).unwrap()
    ));
    second.dispose().await.unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &original,
        &context.get(seekdeep_shell::SHELL).unwrap()
    ));
    first.dispose().await.unwrap();
    assert!(context.get(seekdeep_shell::SHELL).is_none());
}
