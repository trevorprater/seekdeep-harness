//! Source release-version, staging, native hook, and wheel-envelope contracts.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use seekdeep_python_release::{
    Package, RuntimePlatform, hook, load_platforms, pep440_version, repository_version,
    runtime_suffixes, staging, validate_release_tag, wheel,
};
use serde_json::json;
use zip::write::SimpleFileOptions;

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("package.json"),
        "{\"version\":\"1.2.3-rc.1\"}",
    )
    .unwrap();
    fs::write(root.path().join("LICENSE"), "MIT license\n").unwrap();
    fs::write(root.path().join("THIRD_PARTY_NOTICES.md"), "Notices\n").unwrap();
    for (package, namespace, distribution) in [
        ("sdk", "deepseek_harness", "seekdeep-harness-sdk"),
        (
            "sdk-runtime",
            "deepseek_harness_runtime",
            "seekdeep-harness-runtime-bin",
        ),
    ] {
        let directory = root.path().join("python").join(package);
        fs::create_dir_all(directory.join("src").join(namespace)).unwrap();
        fs::write(
            directory.join("src").join(namespace).join("__init__.py"),
            "MARKER = True\n",
        )
        .unwrap();
        let dependencies = if package == "sdk" {
            "dependencies = [\"seekdeep-harness-runtime-bin==0.0.0.dev0\"]\n"
        } else {
            ""
        };
        let hook = if package == "sdk-runtime" {
            "[tool.hatch.build.targets.wheel.hooks.custom]\n"
        } else {
            ""
        };
        fs::write(directory.join("pyproject.toml"), format!("[build-system]\nrequires = [\"hatchling==1.30.1\"]\nbuild-backend = \"hatchling.build\"\n[project]\nname = \"{distribution}\"\nversion = \"0.0.0.dev0\"\nrequires-python = \">=3.10\"\nlicense = \"MIT\"\n{dependencies}[tool.hatch.build.targets.wheel]\npackages = [\"src/{namespace}\"]\n{hook}")).unwrap();
    }
    fs::write(
        root.path().join("python/sdk-runtime/platforms.json"),
        serde_json::to_vec(&platforms()).unwrap(),
    )
    .unwrap();
    root
}

fn platforms() -> serde_json::Value {
    json!({"linux-x64":{"tag":"manylinux_2_28_x86_64","executable":"seekdeep-jsonrpc-agent-pkg-linux-x64"},"macos-arm64":{"tag":"macosx_14_0_arm64","executable":"seekdeep-jsonrpc-agent-pkg-macos-arm64"}})
}

fn executable(path: &Path) {
    fs::write(path, b"native payload").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn repository_and_wheel_versions_keep_distinct_release_spellings() {
    let root = fixture();
    assert_eq!(repository_version(root.path()).unwrap(), "1.2.3-rc.1");
    for (source, expected) in [
        ("1.2.3", "1.2.3"),
        ("1.2.3-rc.1", "1.2.3rc1"),
        ("1.2.3-alpha.2", "1.2.3a2"),
        ("1.2.3-beta.10", "1.2.3b10"),
        ("1.2.3-preview9", "1.2.3rc9"),
        ("1.2.3-c1", "1.2.3rc1"),
    ] {
        assert_eq!(pep440_version(source).unwrap(), expected);
    }
    assert!(
        pep440_version("1.2.3-nightly")
            .unwrap_err()
            .to_string()
            .contains("no PEP 440 spelling")
    );
    validate_release_tag(None, "1.2.3").unwrap();
    validate_release_tag(Some("python-v1.2.3"), "1.2.3").unwrap();
    assert!(
        validate_release_tag(Some("python-v1.2.4"), "1.2.3")
            .unwrap_err()
            .to_string()
            .contains("expected 'python-v1.2.3'")
    );
    fs::write(root.path().join("package.json"), "{\"version\":\"v1.2\"}").unwrap();
    assert!(
        repository_version(root.path())
            .unwrap_err()
            .to_string()
            .contains("must be X.Y.Z")
    );
}

#[test]
fn platform_manifest_is_nonempty_and_has_exact_string_fields() {
    let root = fixture();
    let path = root.path().join("python/sdk-runtime/platforms.json");
    let manifest = load_platforms(&path).unwrap();
    assert_eq!(manifest["macos-arm64"].tag, "macosx_14_0_arm64");
    for invalid in [
        json!({}),
        json!([]),
        json!({"macos-arm64":{"tag":"macosx_14_0_arm64"}}),
        json!({"x":{"tag":"t","executable":"e","extra":true}}),
    ] {
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(load_platforms(&path).is_err());
    }
}

#[test]
fn staging_preserves_namespaces_legal_files_modes_and_exact_pins() {
    let root = fixture();
    let sdk_source = root.path().join("python/sdk");
    for ignored in [".venv", ".pytest_cache", "node_modules", "__pycache__"] {
        fs::create_dir(sdk_source.join(ignored)).unwrap();
        fs::write(sdk_source.join(ignored).join("discard"), "x").unwrap();
    }
    let staged_sdk = root.path().join("staged-sdk");
    staging::stage_sdk(root.path(), &staged_sdk, "1.2.3").unwrap();
    let project = fs::read_to_string(staged_sdk.join("pyproject.toml")).unwrap();
    assert!(project.contains("version = \"1.2.3\""));
    assert!(project.contains("seekdeep-harness-runtime-bin==1.2.3"));
    assert!(project.contains("license-files = [\"LICENSE\"]"));
    assert!(staged_sdk.join("src/deepseek_harness/__init__.py").exists());
    assert!(!staged_sdk.join(".venv").exists());
    assert!(
        fs::read_to_string(sdk_source.join("pyproject.toml"))
            .unwrap()
            .contains("0.0.0.dev0")
    );
    for platform in load_platforms(&root.path().join("python/sdk-runtime/platforms.json"))
        .unwrap()
        .values()
    {
        let binary = root.path().join(&platform.executable);
        executable(&binary);
        for suffix in runtime_suffixes(&platform.executable).iter().skip(1) {
            executable(&root.path().join(format!("{}{suffix}", platform.executable)));
        }
        let staged = root.path().join(&platform.tag);
        staging::stage_runtime(root.path(), &staged, "1.2.3", &binary, &platform.executable)
            .unwrap();
        assert_eq!(
            fs::read(staged.join("LICENSE")).unwrap(),
            fs::read(root.path().join("LICENSE")).unwrap()
        );
        assert_eq!(
            fs::read(staged.join("THIRD_PARTY_NOTICES.md")).unwrap(),
            fs::read(root.path().join("THIRD_PARTY_NOTICES.md")).unwrap()
        );
        let result = hook::initialize(
            &staged,
            "standard",
            "wheel",
            Some(&platform.tag),
            "Linux",
            "x86_64",
        )
        .unwrap();
        assert_eq!(result["tag"], format!("py3-none-{}", platform.tag));
    }
}

#[test]
fn hook_rejects_sdists_mixed_payloads_missing_helpers_and_unexecutable_files() {
    let root = fixture();
    let staged = root.path().join("python/sdk-runtime");
    assert_eq!(
        hook::initialize(&staged, "editable", "sdist", None, "Windows", "unknown").unwrap(),
        json!({})
    );
    assert!(
        hook::initialize(&staged, "standard", "sdist", None, "Linux", "x86_64")
            .unwrap_err()
            .to_string()
            .contains("wheel-only")
    );
    let runtime = staged.join("src/deepseek_harness_runtime/runtime");
    fs::create_dir(&runtime).unwrap();
    let binary = runtime.join("seekdeep-jsonrpc-agent-pkg-linux-x64");
    executable(&binary);
    assert!(hook::initialize(&staged, "standard", "wheel", None, "Linux", "AMD64").is_ok());
    assert!(
        hook::initialize(
            &staged,
            "standard",
            "wheel",
            Some("invalid"),
            "Linux",
            "x86_64"
        )
        .is_err()
    );
    executable(&runtime.join("seekdeep-jsonrpc-agent-pkg-macos-arm64"));
    assert!(hook::initialize(&staged, "standard", "wheel", None, "Linux", "x86_64").is_err());
    fs::remove_file(runtime.join("seekdeep-jsonrpc-agent-pkg-macos-arm64")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(binary, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            hook::initialize(&staged, "standard", "wheel", None, "Linux", "x86_64")
                .unwrap_err()
                .to_string()
                .contains("not executable")
        );
    }
}

fn wheel_file(
    root: &Path,
    package: Package,
    tag: &str,
    executable: Option<(&str, u32)>,
    metadata_override: Option<&str>,
) -> PathBuf {
    let path = root.join("fixture.whl");
    let mut archive = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    archive
        .start_file("fixture.dist-info/WHEEL", options)
        .unwrap();
    write!(archive, "Wheel-Version: 1.0\nTag: {tag}\n").unwrap();
    archive
        .start_file("fixture.dist-info/METADATA", options)
        .unwrap();
    let licenses = if package == Package::Sdk {
        "License-File: LICENSE\n"
    } else {
        "License-File: LICENSE\nLicense-File: THIRD_PARTY_NOTICES.md\n"
    };
    let metadata = metadata_override.map_or_else(|| format!("Name: {}\nVersion: 1.2.3\nLicense-Expression: MIT\n{licenses}Requires-Dist: seekdeep-harness-runtime-bin==1.2.3\n", package.distribution()), str::to_owned);
    archive.write_all(metadata.as_bytes()).unwrap();
    if let Some((name, mode)) = executable {
        archive
            .start_file(
                format!("deepseek_harness_runtime/runtime/{name}"),
                options.unix_permissions(mode),
            )
            .unwrap();
        archive.write_all(b"runtime").unwrap();
    }
    archive.finish().unwrap();
    path
}

#[test]
fn wheel_verification_checks_metadata_and_executable_payloads() {
    let root = tempfile::tempdir().unwrap();
    let sdk = wheel_file(root.path(), Package::Sdk, "py3-none-any", None, None);
    wheel::verify_wheel(&sdk, Package::Sdk, "1.2.3", None).unwrap();
    assert!(
        wheel::verify_wheel(&sdk, Package::Sdk, "1.2.4", None)
            .unwrap_err()
            .to_string()
            .contains("has version")
    );
    let platform = RuntimePlatform {
        tag: "manylinux_2_28_x86_64".to_owned(),
        executable: "seekdeep-jsonrpc-agent-pkg-linux-x64".to_owned(),
    };
    let runtime = wheel_file(
        root.path(),
        Package::Runtime,
        "py3-none-manylinux_2_28_x86_64",
        Some((&platform.executable, 0o755)),
        None,
    );
    wheel::verify_wheel(&runtime, Package::Runtime, "1.2.3", Some(&platform)).unwrap();
    let runtime = wheel_file(
        root.path(),
        Package::Runtime,
        "py3-none-manylinux_2_28_x86_64",
        Some((&platform.executable, 0o644)),
        None,
    );
    assert!(
        wheel::verify_wheel(&runtime, Package::Runtime, "1.2.3", Some(&platform))
            .unwrap_err()
            .to_string()
            .contains("lost its executable bit")
    );
}

#[test]
fn native_cli_builds_sdk_and_runtime_fixture_wheels_through_the_rust_hook() {
    let root = fixture();
    let output = root.path().join("wheels");
    let tool = env!("CARGO_BIN_EXE_seekdeep-python-release");
    let version = std::process::Command::new(tool)
        .args(["--root"])
        .arg(root.path())
        .args(["version", "--github-output"])
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        "repository-version=1.2.3-rc.1\nversion=1.2.3rc1\n"
    );
    let sdk = std::process::Command::new(tool)
        .args(["--root"])
        .arg(root.path())
        .args(["build", "--package", "sdk", "--output-dir"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        sdk.status.success(),
        "{}",
        String::from_utf8_lossy(&sdk.stderr)
    );
    let platform = load_platforms(&root.path().join("python/sdk-runtime/platforms.json")).unwrap()
        ["linux-x64"]
        .clone();
    let executable_path = root.path().join(&platform.executable);
    executable(&executable_path);
    let runtime = std::process::Command::new(tool)
        .args(["--root"])
        .arg(root.path())
        .args([
            "build",
            "--package",
            "runtime",
            "--platform",
            "linux-x64",
            "--runtime-exe",
        ])
        .arg(executable_path)
        .arg("--output-dir")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        runtime.status.success(),
        "{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    let sdk = output.join("seekdeep_harness_sdk-1.2.3rc1-py3-none-any.whl");
    let runtime =
        output.join("seekdeep_harness_runtime_bin-1.2.3rc1-py3-none-manylinux_2_28_x86_64.whl");
    wheel::verify_wheel(&sdk, Package::Sdk, "1.2.3rc1", None).unwrap();
    wheel::verify_wheel(&runtime, Package::Runtime, "1.2.3rc1", Some(&platform)).unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(sdk).unwrap()).unwrap();
    assert!(archive.by_name("deepseek_harness/__init__.py").is_ok());
    assert!(archive.by_name("hatch_build.py").is_err());
}

#[test]
fn release_build_refuses_a_missing_binding_entry_instead_of_a_metadata_only_wheel() {
    let root = fixture();
    fs::remove_file(
        root.path()
            .join("python/sdk/src/deepseek_harness/__init__.py"),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-python-release"))
        .arg("--root")
        .arg(root.path())
        .args(["build", "--package", "sdk", "--output-dir"])
        .arg(root.path().join("wheels"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("refusing to build an empty carrier"));
}
