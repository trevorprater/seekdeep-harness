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
