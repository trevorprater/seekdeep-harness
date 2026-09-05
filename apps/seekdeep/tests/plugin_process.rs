//! Compiled plugin-management process behavior and path anchoring.

use std::{path::Path, process::Command};

#[cfg(unix)]
fn write_fake_pnpm(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run(
    home: &Path,
    cwd: &Path,
    path: &Path,
    args: &[&str],
    record: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .args(args)
        .current_dir(cwd)
        .env("SEEKDEEP_HOME", home)
        .env("PATH", path);
    if let Some(record) = record {
        command.env("SEEKDEEP_PLUGIN_TEST_RECORD", record);
    }
    command.output().unwrap()
}

#[test]
#[cfg(not(windows))]
fn missing_pnpm_initializes_the_profile_and_returns_127() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("work");
    let empty_path = root.path().join("empty-bin");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&empty_path).unwrap();
    let output = run(
        &home,
        &cwd,
        &empty_path,
        &["plugin", "--profile", "custom", "add", "plain-package"],
        None,
    );
    assert_eq!(output.status.code(), Some(127));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("initialized profile custom"), "{stderr}");
    assert!(stderr.contains("pnpm not found on PATH"), "{stderr}");
    assert!(home.join("profiles/custom/package.json").is_file());
}

#[cfg(unix)]
#[test]
fn forwards_to_profile_pnpm_and_anchors_relative_specs_at_the_invoking_directory() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("work/plugin");
    let bin = root.path().join("bin");
    let record = root.path().join("record.txt");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(root.path().join("work/shared")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let pnpm = bin.join("pnpm");
    write_fake_pnpm(
        &pnpm,
        concat!(
            "#!/bin/sh\n",
            "{ printf '%s\\n' \"$PWD\"; printf '%s\\n' \"$@\"; } > \"$SEEKDEEP_PLUGIN_TEST_RECORD\"\n",
            "exit 0\n",
        ),
    );
    let output = run(
        &home,
        &cwd,
        &bin,
        &[
            "plugin",
            "--profile",
            "custom",
            "add",
            ".",
            "file:../shared",
            "--reporter=silent",
        ],
        Some(&record),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let canonical_root = std::fs::canonicalize(root.path()).unwrap();
    assert_eq!(
        lines[0],
        canonical_root
            .join("home/profiles/custom")
            .to_string_lossy()
    );
    assert_eq!(lines[1], "add");
    assert_eq!(
        lines[2],
        canonical_root.join("work/plugin").to_string_lossy()
    );
    assert_eq!(
        lines[3],
        format!("file:{}", canonical_root.join("work/shared").display())
    );
    assert_eq!(lines[4], "--reporter=silent");
}

#[cfg(unix)]
#[test]
fn successful_plugin_command_activates_an_existing_dependency_that_now_exports_a_bundle() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("work");
    let bin = root.path().join("bin");
    let profile = home.join("profiles/up");
    let installed = profile.join("node_modules/late-bundle");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        profile.join("package.json"),
        concat!(
            "{\n",
            "  \"name\": \"seekdeep-profile-up\",\n",
            "  \"private\": true,\n",
            "  \"dependencies\": { \"late-bundle\": \"file:./late-bundle\" },\n",
            "  \"seekdeep\": { \"profile\": { \"bundles\": [\"@seekdeep-ai/seekdeep-base\"] } }\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(profile.join("cordis.patch.yml"), "[]\n").unwrap();
    std::fs::write(
        installed.join("package.json"),
        concat!(
            "{\n",
            "  \"name\": \"late-bundle\",\n",
            "  \"version\": \"2.0.0\",\n",
            "  \"seekdeep\": { \"bundle\": { \"patch\": \"./cordis.patch.yml\" } }\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(installed.join("cordis.patch.yml"), "[]\n").unwrap();
    write_fake_pnpm(&bin.join("pnpm"), "#!/bin/sh\nexit 0\n");

    let output = run(
        &home,
        &cwd,
        &bin,
        &["plugin", "--profile", "up", "root"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["seekdeep"]["profile"]["bundles"],
        serde_json::json!(["@seekdeep-ai/seekdeep-base", "late-bundle"])
    );
}

#[cfg(unix)]
#[test]
fn failed_git_install_preserves_the_exit_code_and_names_the_profile_allowlist() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cwd = root.path().join("work");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_fake_pnpm(&bin.join("pnpm"), "#!/bin/sh\nexit 7\n");

    let output = run(
        &home,
        &cwd,
        &bin,
        &[
            "plugin",
            "--profile",
            "custom",
            "add",
            "github:owner/repository",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(7));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("pnpm failed in profile directory"),
        "{stderr}"
    );
    assert!(
        stderr.contains("git-hosted plugins build on install"),
        "{stderr}"
    );
    let workspace = home.join("profiles/custom/pnpm-workspace.yaml");
    assert!(
        stderr.contains(workspace.to_string_lossy().as_ref()),
        "{stderr}"
    );
}
