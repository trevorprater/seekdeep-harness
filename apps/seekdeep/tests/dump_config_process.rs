//! Compiled launcher config-dump parity without booting profile plugins.

use std::{path::Path, process::Command};

fn run(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_seekdeep"))
        .args(args)
        .current_dir(cwd)
        .env("SEEKDEEP_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn default_dump_prints_the_web_profile_bundle_layers_without_a_user_layer() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let output = run(
        home.path(),
        cwd.path(),
        &["--profile", "web", "--dump-default-config"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("name: '@seekdeep-ai/seekdeep-agent-loop'"));
    assert!(stdout.contains("agents: []"));
    assert!(stdout.contains("# == @seekdeep-ai/seekdeep-base"));
    assert!(stdout.contains("name: '@seekdeep-ai/seekdeep-host-webserver'"));
}

#[test]
fn default_dump_initializes_and_prints_headless_without_host_or_browser_layers() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let output = run(
        home.path(),
        cwd.path(),
        &["--profile", "headless", "--dump-default-config"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("# == @seekdeep-ai/seekdeep-base"));
    assert!(stdout.contains("# == @seekdeep-ai/seekdeep-headless"));
    assert!(stdout.contains("name: '@seekdeep-ai/seekdeep-headless'"));
    assert!(!stdout.contains("name: '@seekdeep-ai/seekdeep-host-"));
    assert!(!stdout.contains("name: '@seekdeep-ai/seekdeep-web-app'"));
    assert!(!stdout.contains("name: '@seekdeep-ai/seekdeep-client-"));
    let profile = home.path().join("profiles/headless");
    assert!(profile.join("package.json").is_file());
    assert!(
        home.path()
            .join("profiles/.seekdeep-installation/package.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(profile.join("cordis.yml")).unwrap(),
        concat!(
            "# seekdeep profile root — an empty entry list. The tree is composed as patches:\n",
            "# each bundle in package.json's seekdeep.profile.bundles, then cordis.patch.yml, then any\n",
            "# --patch overlays. Edit cordis.patch.yml, not this file.\n",
            "[]\n",
        )
    );
}

#[test]
fn full_dump_composes_profile_home_and_relative_overlay_in_order() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let initial = run(
        home.path(),
        cwd.path(),
        &["--profile", "headless", "--dump-default-config"],
    );
    assert!(initial.status.success());
    let profile = home.path().join("profiles/headless");
    std::fs::write(
        profile.join("cordis.patch.yml"),
        concat!(
            "- id: agent-loop\n",
            "  config:\n",
            "    agents:\n",
            "      - id: personal\n",
            "        provider: personal-provider\n",
            "        model: personal-model\n",
            "- id: absent-row\n",
            "  config:\n",
            "    x: 1\n",
        ),
    )
    .unwrap();
    std::fs::write(
        home.path().join("cordis.patch.yml"),
        concat!(
            "- id: agent-loop\n",
            "  config:\n",
            "    agents:\n",
            "      - id: home\n",
            "        provider: home-provider\n",
            "        model: home-model\n",
        ),
    )
    .unwrap();
    std::fs::write(
        cwd.path().join("overlay.yml"),
        concat!(
            "- id: agent-loop\n",
            "  config:\n",
            "    agents:\n",
            "      - id: configured\n",
            "        provider: configured-provider\n",
            "        model: configured-model\n",
        ),
    )
    .unwrap();
    let output = run(
        home.path(),
        cwd.path(),
        &[
            "--profile",
            "headless",
            "--dump-config",
            "--patch",
            "overlay.yml",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("provider: configured-provider"));
    assert!(!stdout.contains("personal-provider"));
    assert!(!stdout.contains("home-provider"));
    let profile_patch = profile.join("cordis.patch.yml");
    let home_patch = home.path().join("cordis.patch.yml");
    let overlay = std::fs::canonicalize(cwd.path())
        .unwrap()
        .join("overlay.yml");
    assert!(stdout.contains(&format!(
        "patched by {}, {}, {}",
        profile_patch.display(),
        home_patch.display(),
        overlay.display()
    )));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("patch: entry \"absent-row\" not found"));
}

#[test]
fn default_dump_skips_a_broken_user_layer_while_full_dump_fails_loud() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let initial = run(
        home.path(),
        cwd.path(),
        &["--profile", "headless", "--dump-default-config"],
    );
    assert!(initial.status.success());
    std::fs::write(
        home.path().join("profiles/headless/cordis.patch.yml"),
        "invalid: [unclosed\n",
    )
    .unwrap();
    let recovery = run(
        home.path(),
        cwd.path(),
        &["--profile", "headless", "--dump-default-config"],
    );
    assert!(recovery.status.success());
    let failure = run(
        home.path(),
        cwd.path(),
        &["--profile", "headless", "--dump-config"],
    );
    assert_eq!(failure.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failure.stderr).contains("failed to parse patch"));
}
