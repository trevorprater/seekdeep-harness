//! Real local tarballs, installed entry calls, payload refusals, and confinement process seams.

mod release_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use release_support::{
    ENTRY_NAME, Fixture, RecordingRunner, assert_source_success, make_executable, output,
    write_json,
};
use seekdeep_landlock_tools::{
    process::{NativeRunner, Runner},
    release::{
        PackOptions, PackedInstallOptions, pack_release, tarball_name, verify_packed_install,
    },
    repo::host_platform,
};
use serde_json::{Value, json};

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn archive_packages(fixture: &Fixture, mutate: impl Fn(&str, &mut Value)) -> PathBuf {
    let output = fixture.temporary.path().join("packed");
    fs::create_dir_all(&output).unwrap();
    for directory in fixture.repository.package_dirs().unwrap() {
        let stage = tempfile::tempdir().unwrap();
        let package = stage.path().join("package");
        copy_tree(&fixture.root().join(&directory), &package);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(package.join("package.json")).unwrap()).unwrap();
        if let Some(optional) = manifest["optionalDependencies"].as_object_mut() {
            for version in optional.values_mut() {
                *version = Value::String("0.1.1".to_owned());
            }
        }
        mutate(&directory.to_string_lossy(), &mut manifest);
        write_json(&package.join("package.json"), &manifest);
        let result = Command::new("tar")
            .arg("-czf")
            .arg(output.join(tarball_name(&manifest).unwrap()))
            .arg("-C")
            .arg(stage.path())
            .arg("package")
            .output()
            .unwrap();
        assert_source_success(&result);
    }
    output
}

fn options(directory: &Path, platform: &str) -> PackedInstallOptions {
    PackedInstallOptions {
        tarball_dir: directory.to_owned(),
        current_platform_only: false,
        host_platform: platform.to_owned(),
        require_landlock: false,
    }
}

#[test]
fn real_pack_preserves_executable_modes_and_converts_workspace_dependencies() {
    let fixture = Fixture::new();
    let npm_cache = fixture.temporary.path().join("npm-cache");
    let install = Command::new("pnpm")
        .args(["install", "--offline", "--ignore-scripts"])
        .current_dir(fixture.temporary.path())
        .env("CI", "true")
        .env("npm_config_cache", &npm_cache)
        .output()
        .unwrap();
    assert_source_success(&install);
    let mut runner = RecordingRunner::new(move |spec| {
        let mut spec = spec.clone();
        spec.env.insert(
            "npm_config_cache".to_owned(),
            npm_cache.to_string_lossy().into_owned(),
        );
        spec.env
            .insert("npm_config_offline".to_owned(), "true".to_owned());
        NativeRunner.run(&spec)
    });
    let destination = fixture.temporary.path().join("actual-npm-pnpm-pack");
    let order = pack_release(
        &fixture.repository,
        &PackOptions {
            destination: destination.clone(),
            current_platform_only: false,
            host_platform: host_platform(),
        },
        &mut runner,
    )
    .unwrap();
    assert_eq!(order.len(), 3);
    assert!(order[0].contains("linux-arm64"));
    assert!(order[1].contains("linux-x64"));
    assert!(!order[2].contains("linux-"));
    let report = verify_packed_install(
        &fixture.repository,
        &options(&destination, &host_platform()),
        &mut runner,
    )
    .unwrap();
    assert_eq!(report.checked_packages, 3);
    assert_eq!(report.enforcement, "unusable");
    assert!(!report.confinement_proved);

    let extraction = tempfile::tempdir().unwrap();
    assert_source_success(
        &Command::new("tar")
            .arg("-xzf")
            .arg(destination.join(&order[0]))
            .arg("-C")
            .arg(extraction.path())
            .output()
            .unwrap(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_ne!(
            fs::metadata(extraction.path().join("package/bin/landlock-run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    let manifest = seekdeep_landlock_tools::release::read_packed_manifest(
        &destination.join(&order[2]),
        fixture.root(),
        &mut NativeRunner,
    )
    .unwrap();
    assert!(
        manifest["optionalDependencies"]
            .as_object()
            .unwrap()
            .values()
            .all(|version| version == "0.1.1")
    );
    let source = fixture.source(
        "verify-packed-install.mjs",
        &[destination.to_string_lossy().into_owned()],
        &[],
        None,
    );
    assert_source_success(&source);
    assert!(
        String::from_utf8_lossy(&source.stdout).contains("Packed install verification passed.")
    );
}

#[test]
fn packed_payload_refusals_match_source_before_installation() {
    for (field, script) in [
        ("scripts", "preinstall"),
        ("scripts", "install"),
        ("scripts", "postinstall"),
        ("scripts", "prepare"),
        ("dependencies", "runtime"),
        ("optionalDependencies", "platform"),
        ("peerDependencies", "peer"),
    ] {
        let fixture = Fixture::new();
        let directory = archive_packages(&fixture, |dir, manifest| {
            if dir != "packages/linux-arm64" {
                return;
            }
            manifest[field] =
                json!({script:if field=="scripts" {"node build.js"} else {"workspace:^"}});
        });
        let result = verify_packed_install(
            &fixture.repository,
            &options(&directory, &host_platform()),
            &mut NativeRunner,
        )
        .unwrap_err();
        let source = fixture.source(
            "verify-packed-install.mjs",
            &[directory.to_string_lossy().into_owned()],
            &[],
            None,
        );
        assert!(!source.status.success());
        assert!(
            String::from_utf8_lossy(&source.stderr).contains(&result.to_string()),
            "Rust {result}\nsource {}",
            String::from_utf8_lossy(&source.stderr)
        );
    }
    let fixture = Fixture::new();
    let directory = archive_packages(&fixture, |dir, manifest| {
        if dir == "packages/entry" {
            manifest["optionalDependencies"] = json!({"unrelated":"1.0.0"});
        }
    });
    let error = verify_packed_install(
        &fixture.repository,
        &options(&directory, &host_platform()),
        &mut NativeRunner,
    )
    .unwrap_err();
    let source = fixture.source(
        "verify-packed-install.mjs",
        &[directory.to_string_lossy().into_owned()],
        &[],
        None,
    );
    assert!(String::from_utf8_lossy(&source.stderr).contains(&error.to_string()));
}

#[test]
fn all_platform_and_current_platform_tarball_coverage_are_distinct() {
    let fixture = Fixture::new();
    let directory = archive_packages(&fixture, |_, _| {});
    fs::remove_file(
        directory.join(tarball_name(&fixture.manifest("packages/linux-arm64")).unwrap()),
    )
    .unwrap();
    let full = options(&directory, "darwin-arm64");
    assert!(
        verify_packed_install(&fixture.repository, &full, &mut NativeRunner)
            .unwrap_err()
            .to_string()
            .contains("missing packed tarball:")
    );
    let mut current = full;
    current.current_platform_only = true;
    let mut runner = installed_runner("darwin-arm64", "unusable", Failure::None);
    let report = verify_packed_install(&fixture.repository, &current, &mut runner).unwrap();
    assert_eq!(report.checked_packages, 1);
    assert_eq!(report.byte_pinned_binaries, 0);
    let mut unsupported = options(&directory, "linux-riscv64");
    unsupported.current_platform_only = true;
    assert!(
        verify_packed_install(&fixture.repository, &unsupported, &mut NativeRunner)
            .unwrap_err()
            .to_string()
            .contains("linux host without a platform package in the matrix: linux-riscv64")
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    RelativePath,
    WrongPackage,
    DeniedSuccess,
    DeniedFile,
    GrantedExit,
    GrantedContent,
}

fn installed_runner(platform: &str, enforcement: &str, failure: Failure) -> RecordingRunner {
    let platform = platform.to_owned();
    let enforcement = enforcement.to_owned();
    RecordingRunner::new(move |spec| {
        if spec.program == "tar" {
            return NativeRunner.run(spec);
        }
        if spec.program == "node" {
            let result = match spec.args[1].as_str() {
                "launcherPath" => {
                    let resolved = if failure == Failure::RelativePath {
                        PathBuf::from("relative/landlock-run")
                    } else if failure == Failure::WrongPackage {
                        spec.cwd.join("unrelated/landlock-run")
                    } else {
                        spec.cwd
                            .join("node_modules")
                            .join(format!("{ENTRY_NAME}-{platform}"))
                            .join("bin/landlock-run")
                    };
                    json!(resolved)
                }
                "probe" => json!(enforcement),
                "grantArgs" => {
                    let parameters: Value = serde_json::from_str(&spec.args[2])?;
                    let grants = &parameters[0];
                    let mut argv = Vec::new();
                    for (field, flag) in [("readOnly", "--ro"), ("readWrite", "--rw")] {
                        if let Some(paths) = grants[field].as_array() {
                            for path in paths {
                                argv.extend([Value::String(flag.into()), path.clone()]);
                            }
                        }
                    }
                    Value::Array(argv)
                }
                other => panic!("unexpected installed entry call {other}"),
            };
            return Ok(output(0, &result.to_string(), ""));
        }
        let shell = spec.args.last().unwrap();
        if shell.starts_with("echo x > ") {
            if failure == Failure::DeniedFile {
                fs::write(shell.trim_start_matches("echo x > "), "x")?;
            }
            return Ok(output(
                i32::from(failure != Failure::DeniedSuccess),
                "",
                "denied",
            ));
        }
        assert!(shell.starts_with("echo ok > "));
        if failure == Failure::GrantedExit {
            return Ok(output(8, "", "granted path rejected"));
        }
        fs::write(
            shell.trim_start_matches("echo ok > "),
            if failure == Failure::GrantedContent {
                "wrong"
            } else {
                "ok\n"
            },
        )?;
        Ok(output(0, "", ""))
    })
}

#[test]
fn installed_linux_path_probe_and_confinement_operations_are_in_source_order() {
    for enforcement in ["full", "partial", "unusable"] {
        let fixture = Fixture::new();
        let directory = archive_packages(&fixture, |_, _| {});
        let mut runner = installed_runner("linux-x64", enforcement, Failure::None);
        let report = verify_packed_install(
            &fixture.repository,
            &options(&directory, "linux-x64"),
            &mut runner,
        )
        .unwrap();
        assert_eq!(report.byte_pinned_binaries, 1);
        assert_eq!(report.confinement_proved, enforcement != "unusable");
        let actions = runner
            .calls
            .iter()
            .filter(|call| call.program != "tar")
            .map(|call| {
                if call.program == "node" {
                    call.args[1].as_str()
                } else if call.args.last().unwrap().starts_with("echo x") {
                    "denied write"
                } else {
                    "granted write"
                }
            })
            .collect::<Vec<_>>();
        let expected = if enforcement == "unusable" {
            vec!["launcherPath", "probe"]
        } else {
            vec![
                "launcherPath",
                "probe",
                "grantArgs",
                "denied write",
                "grantArgs",
                "granted write",
            ]
        };
        assert_eq!(actions, expected);
    }
}

#[test]
fn installed_probe_and_world_proof_failures_stop_at_the_responsible_boundary() {
    for (failure, message) in [
        (Failure::RelativePath, "launcherPath must be absolute"),
        (
            Failure::WrongPackage,
            "launcherPath must point into the platform package:",
        ),
        (Failure::DeniedSuccess, "write outside the grants must fail"),
        (Failure::DeniedFile, "denied write must not land on disk"),
        (Failure::GrantedExit, "granted write must succeed:"),
        (Failure::GrantedContent, "granted write content mismatch"),
    ] {
        let fixture = Fixture::new();
        let directory = archive_packages(&fixture, |_, _| {});
        let mut runner = installed_runner("linux-x64", "full", failure);
        let error = verify_packed_install(
            &fixture.repository,
            &options(&directory, "linux-x64"),
            &mut runner,
        )
        .unwrap_err();
        assert!(error.to_string().starts_with(message), "{error}");
        if matches!(failure, Failure::RelativePath | Failure::WrongPackage) {
            assert!(
                !runner
                    .calls
                    .iter()
                    .any(|call| call.args.get(1).is_some_and(|arg| arg == "probe"))
            );
        }
    }
    let fixture = Fixture::new();
    let directory = archive_packages(&fixture, |_, _| {});
    let mut required = options(&directory, "linux-x64");
    required.require_landlock = true;
    let error = verify_packed_install(
        &fixture.repository,
        &required,
        &mut installed_runner("linux-x64", "unusable", Failure::None),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "NALR_REQUIRE_LANDLOCK=1 but the probe reports unusable"
    );
}

#[test]
fn installed_binary_bytes_and_mode_bits_are_checked_before_probe() {
    for mode_only in [false, true] {
        let fixture = Fixture::new();
        let binary = fixture.root().join("packages/linux-x64/bin/landlock-run");
        if mode_only {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let directory = archive_packages(&fixture, |_, _| {});
        if !mode_only {
            fs::write(&binary, b"modified after pack").unwrap();
            make_executable(&binary);
        }
        let mut runner = installed_runner("linux-x64", "full", Failure::None);
        let error = verify_packed_install(
            &fixture.repository,
            &options(&directory, "linux-x64"),
            &mut runner,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(if mode_only {
                "not executable"
            } else {
                "differs from the workspace build"
            }),
            "{error}"
        );
        assert!(
            !runner
                .calls
                .iter()
                .any(|call| call.args.get(1).is_some_and(|arg| arg == "probe"))
        );
    }
}

#[test]
fn captured_tar_and_installed_node_failures_preserve_child_status_and_diagnostics() {
    let fixture = Fixture::new();
    let mut runner = RecordingRunner::new(|spec| {
        assert_eq!(spec.program, "tar");
        assert!(spec.capture);
        assert_eq!(spec.max_buffer, 64 * 1024 * 1024);
        assert!(!spec.inherit_stdin);
        Ok(output(17, "", "tar: archive is unreadable\n"))
    });
    let error = seekdeep_landlock_tools::release::read_packed_manifest(
        &fixture.temporary.path().join("broken.tgz"),
        fixture.root(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(seekdeep_landlock_tools::process::exit_code(&error), 17);
    assert_eq!(error.to_string(), "tar: archive is unreadable");

    let directory = archive_packages(&fixture, |_, _| {});
    let mut runner = RecordingRunner::new(|spec| {
        if spec.program == "node" {
            assert_eq!(spec.max_buffer, 64 * 1024 * 1024);
            return Ok(output(19, "", "Error: installed binding cannot load\n"));
        }
        NativeRunner.run(spec)
    });
    let error = verify_packed_install(
        &fixture.repository,
        &options(&directory, "darwin-arm64"),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(seekdeep_landlock_tools::process::exit_code(&error), 19);
    assert_eq!(error.to_string(), "Error: installed binding cannot load");

    let mut runner = RecordingRunner::new(|_| {
        let mut result = output(0, "not a manifest", "");
        result.spawn_error = Some("spawnSync tar ENOBUFS".to_owned());
        Ok(result)
    });
    let error = seekdeep_landlock_tools::release::read_packed_manifest(
        &fixture.temporary.path().join("oversized.tgz"),
        fixture.root(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "spawnSync tar ENOBUFS");
    assert_eq!(seekdeep_landlock_tools::process::exit_code(&error), 1);
}
