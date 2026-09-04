//! Source executable-target parsing and native artifact identity contracts.

use seekdeep_python_release::executable::{
    Arch, CliOutcome, Host, Platform, Target, build_executables, parse_cli,
    validate_native_artifact,
};
use serde_json::json;

fn host() -> Host {
    Host {
        platform: "darwin".to_owned(),
        arch: "arm64".to_owned(),
    }
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn flags_preserve_host_defaults_help_last_value_and_collisions() {
    let CliOutcome::Build(default) = parse_cli(&[], &host()).unwrap() else {
        panic!("build expected")
    };
    assert_eq!(default.targets[0].spec(), "node24-macos-arm64");
    assert!(!default.skip_build && !default.dry_run);
    let CliOutcome::Build(options) = parse_cli(
        &args(&[
            "--targets=invalid",
            "--targets",
            "node24-linux-x64, node24-macos-arm64",
            "--dry-run",
            "--skip-build",
            "--dry-run",
        ]),
        &host(),
    )
    .unwrap() else {
        panic!("build expected")
    };
    assert_eq!(options.targets.len(), 2);
    assert!(options.skip_build && options.dry_run);
    assert_eq!(
        parse_cli(&args(&["--targets=invalid", "--help"]), &host()).unwrap(),
        CliOutcome::Help
    );
    let duplicate = parse_cli(
        &args(&["--targets=node22-linux-x64,node24-linux-x64"]),
        &host(),
    )
    .unwrap_err();
    assert!(
        duplicate
            .message
            .contains("duplicate platform-arch linux-x64")
    );
    assert!(!duplicate.show_usage);
    assert!(
        parse_cli(&args(&["--targets= , "]), &host())
            .unwrap_err()
            .message
            .ends_with("--targets is empty.")
    );
    let mut invalid = default;
    invalid.targets.clear();
    assert!(invalid.validate().is_err());
}

#[test]
fn malformed_option_errors_keep_the_source_diagnostics() {
    for (values, expected) in [
        (
            vec!["--targets"],
            "Option '--targets <value>' argument missing",
        ),
        (
            vec!["--dry-run=false"],
            "Option '--dry-run' does not take an argument",
        ),
        (vec!["-abc"], "Unknown option '-a'"),
        (vec!["--wat=1"], "Unknown option '--wat'"),
        (
            vec!["--", "value"],
            "Unexpected argument 'value'. This command does not take positional arguments",
        ),
    ] {
        let error = parse_cli(&args(&values), &host()).unwrap_err();
        assert_eq!(
            error.message,
            format!("build-exe-for-python-sdk: {expected}")
        );
        assert!(error.show_usage);
    }
}

#[test]
fn every_source_platform_arch_pair_has_a_closed_native_target() {
    for (spec, platform, arch, rust) in [
        (
            "node24-linux-x64",
            Platform::Linux,
            Arch::X64,
            "x86_64-unknown-linux-gnu",
        ),
        (
            "node24-linux-arm64",
            Platform::Linux,
            Arch::Arm64,
            "aarch64-unknown-linux-gnu",
        ),
        (
            "node24-macos-x64",
            Platform::Macos,
            Arch::X64,
            "x86_64-apple-darwin",
        ),
        (
            "node24-macos-arm64",
            Platform::Macos,
            Arch::Arm64,
            "aarch64-apple-darwin",
        ),
    ] {
        let target = Target::parse(spec).unwrap();
        assert_eq!(target.platform(), platform);
        assert_eq!(target.arch(), arch);
        assert_eq!(target.rust_target(), rust);
    }
    for spec in [
        "linux-x64",
        "node24-linux",
        "node24-windows-x64",
        "node24-linux-ia32",
        "latest-linux-x64",
    ] {
        assert!(Target::parse(spec).is_err());
    }
}

#[test]
fn dry_run_does_not_create_outputs_or_launch_cargo() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temporary.path().join("python/sdk-runtime")).unwrap();
    std::fs::write(
        temporary.path().join("python/sdk-runtime/package.json"),
        json!({"name":"seekdeep-jsonrpc-agent-pkg","dependencies":{"entry":"workspace:^"}})
            .to_string(),
    )
    .unwrap();
    let CliOutcome::Build(options) = parse_cli(
        &args(&["--dry-run", "--targets=node24-linux-x64,node24-macos-arm64"]),
        &host(),
    )
    .unwrap() else {
        panic!("build expected")
    };
    let report = build_executables(temporary.path(), &options, &host()).unwrap();
    assert_eq!(report.products.len(), 3);
    assert_eq!(report.binding_libraries.len(), 2);
    assert!(!temporary.path().join("dist-exe").exists());
    assert!(!report.node_carrier.exists());
}

#[test]
fn native_artifact_check_rejects_the_wrong_format_architecture_and_mode() {
    let temporary = tempfile::tempdir().unwrap();
    let binary = temporary.path().join("runtime");
    let mut header = [0_u8; 32];
    header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    header[4..8].copy_from_slice(&0x0100_000c_u32.to_le_bytes());
    header[12..16].copy_from_slice(&2_u32.to_le_bytes());
    std::fs::write(&binary, header).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    validate_native_artifact(&binary, &Target::parse("node24-macos-arm64").unwrap()).unwrap();
    assert!(
        validate_native_artifact(&binary, &Target::parse("node24-macos-x64").unwrap()).is_err()
    );
    assert!(
        validate_native_artifact(&binary, &Target::parse("node24-linux-arm64").unwrap()).is_err()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            validate_native_artifact(&binary, &Target::parse("node24-macos-arm64").unwrap())
                .is_err()
        );
    }
}
