//! Source differential checks for native metadata, build selection, and prepack gates.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Result, bail};
use seekdeep_landlock_tools::{
    build::{BuildHost, build_native, build_targets},
    matrix::{MatrixKind, github_matrix},
    process::{CommandOutput, CommandSpec, Runner},
    repo::{Repository, verify_entry_lib, verify_platform_binaries},
};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    repo: Repository,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("native/landlock-run");
        fs::create_dir_all(root.join("packages")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        let source = source_root().join("native/landlock-run/scripts");
        for script in [
            "repo.mjs",
            "github-matrix.mjs",
            "verify-entry-lib.mjs",
            "verify-launcher-binary.mjs",
            "build.ts",
        ] {
            fs::copy(source.join(script), root.join("scripts").join(script)).unwrap();
        }
        Self {
            directory,
            repo: Repository::new(root),
        }
    }

    fn package(&self, directory: &str, cpu: Option<&str>, platform: Option<&str>) -> PathBuf {
        let package = self.repo.root.join("packages").join(directory);
        fs::create_dir_all(&package).unwrap();
        let mut manifest = json!({ "name": format!("@fixture/{directory}"), "version": "1.2.3" });
        if let Some(cpu) = cpu {
            manifest["cpu"] = json!([cpu]);
        }
        write_json(&package.join("package.json"), &manifest);
        if let Some(platform) = platform {
            write_json(
                &package.join("prebuilds.json"),
                &json!({
                    "platform": platform,
                    "binaries": [{"tool":"landlock-run", "kind":"static-musl", "path":"bin/landlock-run"}],
                }),
            );
        }
        package
    }

    fn source_repo(&self, expression: &str, argument: Option<&Path>) -> Value {
        let code = format!(
            "import {{ pathToFileURL }} from 'node:url'; const repo = await import(pathToFileURL(process.argv[1] + '/scripts/repo.mjs')); try {{ const value = {expression}; process.stdout.write(JSON.stringify({{ok:true,value}})); }} catch(error) {{ process.stdout.write(JSON.stringify({{ok:false,error:error.message}})); }}"
        );
        let output = Command::new("node")
            .args(["--input-type=module", "-e", &code])
            .arg(&self.repo.root)
            .arg(argument.unwrap_or(&self.repo.root))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn source_root() -> PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || {
            PathBuf::from(
                include_str!("../../../SOURCE_SNAPSHOT")
                    .lines()
                    .find_map(|line| line.strip_prefix("repository="))
                    .unwrap(),
            )
        },
        PathBuf::from,
    )
}

fn elf(path: &Path, machine: u16) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = vec![0_u8; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    fs::write(path, bytes).unwrap();
    executable(path, true);
}

#[cfg(unix)]
fn executable(path: &Path, allowed: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if allowed { 0o755 } else { 0o644 }),
    )
    .unwrap();
}

#[cfg(not(unix))]
fn executable(_: &Path, _: bool) {}

fn verification_result(package: &Path) -> Value {
    match verify_platform_binaries(package) {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    }
}

#[test]
fn package_discovery_preserves_metadata_classification_sort_and_publish_order() {
    let fixture = Fixture::new();
    fixture.package("z-entry", None, None);
    fixture.package("a-entry", None, None);
    fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    fixture.package("linux-arm64", Some("arm64"), Some("linux-arm64"));
    fs::create_dir_all(fixture.repo.root.join("packages/ignored")).unwrap();
    for (function, actual) in [
        ("platformDirs", fixture.repo.platform_dirs().unwrap()),
        ("entryDirs", fixture.repo.entry_dirs().unwrap()),
        ("packageDirs", fixture.repo.package_dirs().unwrap()),
    ] {
        let expected = fixture.source_repo(&format!("repo.{function}()"), None);
        assert_eq!(expected, json!({"ok":true,"value":actual}));
    }
    assert_eq!(
        fixture.repo.package_dirs().unwrap(),
        [
            "packages/linux-arm64",
            "packages/linux-x64",
            "packages/a-entry",
            "packages/z-entry"
        ]
        .map(PathBuf::from)
    );
}

#[test]
fn platform_gate_matches_source_for_valid_missing_wrong_mode_and_undeclared_payloads() {
    let fixture = Fixture::new();
    let package = fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    let verify = || {
        assert_eq!(
            verification_result(&package),
            fixture.source_repo(
                "repo.verifyPlatformBinaries(process.argv[2])",
                Some(&package)
            )
        );
    };
    verify();
    elf(&package.join("bin/landlock-run"), 183);
    verify();
    elf(&package.join("bin/landlock-run"), 62);
    verify();
    #[cfg(unix)]
    {
        executable(&package.join("bin/landlock-run"), false);
        verify();
        executable(&package.join("bin/landlock-run"), true);
    }
    fs::write(package.join("bin/z-extra"), "unexpected").unwrap();
    fs::write(package.join("bin/a-extra"), "unexpected").unwrap();
    verify();
    fs::remove_file(package.join("bin/z-extra")).unwrap();
    fs::remove_file(package.join("bin/a-extra")).unwrap();
    for length in [0, 1, 2, 10, 19] {
        fs::write(package.join("bin/landlock-run"), vec![0; length]).unwrap();
        verify();
    }
}

#[test]
fn platform_gate_checks_cpu_before_payload_and_supports_each_declared_machine() {
    let fixture = Fixture::new();
    for (directory, cpu) in [
        ("missing", None),
        ("bad", Some("riscv64")),
        ("x64", Some("x64")),
        ("arm64", Some("arm64")),
    ] {
        let package = fixture.package(directory, cpu, Some("linux-test"));
        elf(
            &package.join("bin/landlock-run"),
            if cpu == Some("arm64") { 183 } else { 62 },
        );
        assert_eq!(
            verification_result(&package),
            fixture.source_repo(
                "repo.verifyPlatformBinaries(process.argv[2])",
                Some(&package)
            )
        );
    }
}

#[test]
fn ci_and_release_matrices_match_source_without_duplicate_ci_platforms() {
    let fixture = Fixture::new();
    fixture.package("linux-x64-extra", Some("x64"), Some("linux-x64"));
    fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    fixture.package("linux-arm64", Some("arm64"), Some("linux-arm64"));
    for (target, kind) in [
        ("ci", MatrixKind::Ci),
        ("release-prebuild", MatrixKind::ReleasePrebuild),
    ] {
        let output = Command::new("node")
            .arg(fixture.repo.root.join("scripts/github-matrix.mjs"))
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = github_matrix(&fixture.repo, kind).unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            serde_json::to_string(&actual).unwrap()
        );
    }
    fixture.package("linux-other", Some("riscv64"), Some("linux-riscv64"));
    for kind in [MatrixKind::Ci, MatrixKind::ReleasePrebuild] {
        assert_eq!(
            github_matrix(&fixture.repo, kind).unwrap_err().to_string(),
            "missing GitHub runner for platform: linux-riscv64"
        );
    }
}

#[test]
fn entry_gate_preserves_required_file_order_and_success_message() {
    let fixture = Fixture::new();
    let package = fixture.package("entry", None, None);
    for file in [None, Some("lib/index.js"), Some("lib/index.d.ts")] {
        if let Some(file) = file {
            fs::create_dir_all(package.join("lib")).unwrap();
            fs::write(package.join(file), "export {};\n").unwrap();
        }
        let output = Command::new("node")
            .arg(fixture.repo.root.join("scripts/verify-entry-lib.mjs"))
            .current_dir(&package)
            .output()
            .unwrap();
        match verify_entry_lib(&package) {
            Ok(message) => {
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!("{message}\n")
                );
            }
            Err(error) => {
                assert_eq!(output.status.code(), Some(1));
                assert_eq!(
                    String::from_utf8(output.stderr).unwrap(),
                    format!("{error}\n")
                );
            }
        }
    }
}

#[test]
fn compiled_entry_gate_rejects_missing_loader_and_missing_or_invalid_wasm() {
    use seekdeep_landlock_tools::entry::verify_entry_wasm;
    let fixture = Fixture::new();
    let package = fixture.package("entry", None, None);
    fs::create_dir_all(package.join("lib")).unwrap();
    fs::write(package.join("lib/index.js"), "export {};\n").unwrap();
    fs::write(package.join("lib/index.d.ts"), "export {};\n").unwrap();
    assert!(
        verify_entry_wasm(&package)
            .unwrap_err()
            .to_string()
            .contains("missing lib/seekdeep_landlock_entry.cjs")
    );
    fs::write(
        package.join("lib/seekdeep_landlock_entry.cjs"),
        "module.exports = {};\n",
    )
    .unwrap();
    assert!(
        verify_entry_wasm(&package)
            .unwrap_err()
            .to_string()
            .contains("missing lib/seekdeep_landlock_entry_bg.wasm")
    );
    fs::write(
        package.join("lib/seekdeep_landlock_entry_bg.wasm"),
        "invalid",
    )
    .unwrap();
    assert!(
        verify_entry_wasm(&package)
            .unwrap_err()
            .to_string()
            .contains("not a compiled WebAssembly module")
    );
    fs::write(
        package.join("lib/seekdeep_landlock_entry_bg.wasm"),
        b"\0asm\x01\0\0\0",
    )
    .unwrap();
    for file in [
        "lib/landlock-run.main.rs",
        "lib/landlock-run.lib.rs",
        "lib/landlock-run.Cargo.toml",
        "lib/seekdeep-workspace.Cargo.toml",
        "lib/seekdeep-workspace.Cargo.lock",
        "lib/seekdeep-workspace.rust-toolchain.toml",
        "lib/seekdeep-harness.LICENSE",
    ] {
        assert!(
            verify_entry_wasm(&package)
                .unwrap_err()
                .to_string()
                .contains(&format!("missing {file}"))
        );
        fs::write(package.join(file), "audit source\n").unwrap();
    }
    assert_eq!(
        verify_entry_wasm(&package).unwrap(),
        "verify-entry-lib: @fixture/entry built lib/ present."
    );
}

#[derive(Default)]
struct Compiler {
    commands: Vec<CommandSpec>,
    logs: Vec<String>,
    failure: Option<i32>,
    spawn_error: bool,
}

impl Runner for Compiler {
    fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
        self.commands.push(command.clone());
        if self.spawn_error {
            bail!("compiler unavailable");
        }
        if let Some(status) = self.failure {
            return Ok(CommandOutput {
                status: Some(status),
                ..CommandOutput::default()
            });
        }
        let argument =
            |key| &command.args[command.args.iter().position(|arg| arg == key).unwrap() + 1];
        let target = argument("--target");
        let output = Path::new(argument("--target-dir"))
            .join(target)
            .join("release/landlock-run");
        elf(
            &output,
            if target.starts_with("aarch64") {
                183
            } else {
                62
            },
        );
        Ok(CommandOutput {
            status: Some(0),
            ..CommandOutput::default()
        })
    }
    fn sleep(&mut self, _: Duration) {
        panic!("builds do not retry");
    }
    fn log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }
}

#[test]
fn build_refuses_non_linux_and_undeclared_hosts_before_running_any_compiler() {
    let fixture = Fixture::new();
    fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    for (os, arch) in [("darwin", "arm64"), ("win32", "x64"), ("linux", "arm64")] {
        let host = BuildHost {
            os: os.to_owned(),
            arch: arch.to_owned(),
        };
        let source_code = format!(
            "Object.defineProperty(process, 'platform', {{ value: {} }}); Object.defineProperty(process, 'arch', {{ value: {} }}); await import(process.argv[1]);",
            json!(os),
            json!(arch)
        );
        let output = Command::new("node")
            .args(["--input-type=module", "-e", &source_code])
            .arg(fixture.repo.root.join("scripts/build.ts"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("{}\n", build_targets(&fixture.repo, &host).unwrap_err())
        );
        let mut compiler = Compiler::default();
        assert!(build_native(&fixture.repo, &host, &mut compiler).is_err());
        assert!(compiler.commands.is_empty());
    }
}

#[test]
fn rust_build_is_native_static_musl_and_preserves_metadata_output_order() {
    for (arch, machine) in [("x64", 62), ("arm64", 183)] {
        let fixture = Fixture::new();
        let package = fixture.package(
            &format!("linux-{arch}"),
            Some(arch),
            Some(&format!("linux-{arch}")),
        );
        fixture.package("other", Some("x64"), Some("linux-not-this-host"));
        let mut compiler = Compiler::default();
        let outputs = build_native(
            &fixture.repo,
            &BuildHost {
                os: "linux".to_owned(),
                arch: arch.to_owned(),
            },
            &mut compiler,
        )
        .unwrap();
        assert_eq!(outputs, [package.join("bin/landlock-run")]);
        assert_eq!(compiler.commands.len(), 1);
        let command = &compiler.commands[0];
        assert_eq!(command.program, "cargo");
        assert_eq!(command.args[0], "rustc");
        assert!(
            command
                .args
                .iter()
                .any(|arg| arg == "target-feature=+crt-static")
        );
        assert_eq!(command.env["CARGO_INCREMENTAL"], "0");
        assert!(
            command
                .env
                .iter()
                .any(|(name, value)| name.ends_with("_LINKER") && value == "musl-gcc")
        );
        assert_eq!(
            &fs::read(&outputs[0]).unwrap()[18..20],
            &u16::try_from(machine).unwrap().to_le_bytes()
        );
        assert_eq!(
            compiler.logs,
            [format!("build: built linux-{arch}/bin/landlock-run")]
        );
        assert!(fixture.directory.path().exists());
    }
}

#[test]
fn build_validates_tools_and_kind_before_execution_and_stops_at_compiler_failure() {
    let fixture = Fixture::new();
    let package = fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    let host = BuildHost {
        os: "linux".to_owned(),
        arch: "x64".to_owned(),
    };
    for (tool, kind, expected) in [
        (
            "unknown",
            "static-musl",
            "build: prebuilds.json names unknown tool \"unknown\" — add it to the TOOLS table in scripts/build.ts.",
        ),
        (
            "landlock-run",
            "dynamic",
            "build: unknown binary kind \"dynamic\" — the only toolchain here is static musl.",
        ),
    ] {
        write_json(
            &package.join("prebuilds.json"),
            &json!({"platform":"linux-x64","binaries":[{"tool":tool,"kind":kind,"path":"bin/landlock-run"}]}),
        );
        let mut compiler = Compiler::default();
        assert_eq!(
            build_native(&fixture.repo, &host, &mut compiler)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert!(compiler.commands.is_empty());
    }
    fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    for mut compiler in [
        Compiler {
            failure: Some(7),
            ..Compiler::default()
        },
        Compiler {
            spawn_error: true,
            ..Compiler::default()
        },
    ] {
        assert!(
            build_native(&fixture.repo, &host, &mut compiler)
                .unwrap_err()
                .to_string()
                .starts_with("build: Rust static-musl build failed")
        );
        assert_eq!(compiler.commands.len(), 1);
        assert!(!package.join("bin/landlock-run").exists());
        assert!(compiler.logs.is_empty());
    }
}

#[test]
fn actual_command_line_gates_match_success_failure_status_and_messages() {
    let fixture = Fixture::new();
    let package = fixture.package("linux-x64", Some("x64"), Some("linux-x64"));
    for present in [false, true] {
        if present {
            elf(&package.join("bin/landlock-run"), 62);
        }
        let expected = Command::new("node")
            .arg(fixture.repo.root.join("scripts/verify-launcher-binary.mjs"))
            .current_dir(&package)
            .output()
            .unwrap();
        let actual = Command::new(env!("CARGO_BIN_EXE_landlock-verify-launcher-binary"))
            .current_dir(&package)
            .output()
            .unwrap();
        assert_eq!(actual.status.code(), expected.status.code());
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);
        let explicit = Command::new(env!("CARGO_BIN_EXE_landlock-verify-launcher-binary"))
            .arg("--root")
            .arg(&fixture.repo.root)
            .arg("packages/linux-x64")
            .output()
            .unwrap();
        assert_eq!(explicit.status.code(), actual.status.code());
        assert_eq!(explicit.stdout, actual.stdout);
        assert_eq!(explicit.stderr, actual.stderr);
    }
    let matrix = Command::new(env!("CARGO_BIN_EXE_landlock-github-matrix"))
        .arg("--root")
        .arg(&fixture.repo.root)
        .arg("ci")
        .output()
        .unwrap();
    assert!(matrix.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&matrix.stdout).unwrap(),
        github_matrix(&fixture.repo, MatrixKind::Ci).unwrap()
    );
    let usage = Command::new(env!("CARGO_BIN_EXE_landlock-github-matrix"))
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(usage.stderr).unwrap(),
        "Usage: landlock-github-matrix <ci|release-prebuild>\n"
    );
}

#[test]
fn native_process_boundary_preserves_cwd_environment_capture_and_nonzero_exit() {
    use seekdeep_landlock_tools::process::{NativeRunner, exit_code, run_checked};
    let fixture = Fixture::new();
    let command = CommandSpec {
        program: "node".to_owned(),
        args: vec!["-e".to_owned(), "process.stdout.write(process.cwd() + ':' + process.env.NALR_TEST_VALUE); process.stderr.write('failure'); process.exit(23)".to_owned()],
        cwd: fixture.repo.root.clone(),
        env: BTreeMap::from([("NALR_TEST_VALUE".to_owned(), "sentinel".to_owned())]),
        capture: true,
        max_buffer: 1_048_576,
        inherit_stdin: false,
    };
    let output = NativeRunner.run(&command).unwrap();
    assert_eq!(output.status, Some(23));
    assert_eq!(
        output.stdout,
        format!(
            "{}:sentinel",
            fixture.repo.root.canonicalize().unwrap().display()
        )
    );
    assert_eq!(output.stderr, "failure");
    let error = run_checked(&mut NativeRunner, &command).unwrap_err();
    assert_eq!(exit_code(&error), 23);
}
