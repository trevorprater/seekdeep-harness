//! A running gate can rebuild the workspace that produced its executable.

use std::{path::Path, process::Command};

const PROCESSOR: &str = r#"
use std::{env, fs, io::Write as _, process::{self, Command}};

fn main() {
    let root = env::current_dir().unwrap();
    fs::write(root.join("observed-executable"), env::current_exe().unwrap().canonicalize().unwrap().to_str().unwrap()).unwrap();
    fs::write(root.join("observed-arguments"), env::args().skip(1).collect::<Vec<_>>().join("\0")).unwrap();
    fs::OpenOptions::new().append(true).open(root.join("crates/repository-tools/src/main.rs")).unwrap().write_all(b"\n// Rebuild the active processor.\n").unwrap();
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--quiet", "--workspace", "--all-features"])
        .status().unwrap();
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
    println!("workspace rebuilt");
    process::exit(17);
}
"#;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn workspace(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/repository-runner\", \"crates/repository-tools\"]\n",
    );
    write(
        root,
        "crates/repository-runner/Cargo.toml",
        "[package]\nname = \"seekdeep-repository-runner\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    write(
        root,
        "crates/repository-runner/src/main.rs",
        include_str!("../src/main.rs"),
    );
    write(
        root,
        "crates/repository-tools/Cargo.toml",
        "[package]\nname = \"seekdeep-repository-tools\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[[bin]]\nname = \"run-gates\"\npath = \"src/main.rs\"\n",
    );
    write(root, "crates/repository-tools/src/main.rs", PROCESSOR);
}

#[test]
fn workspace_rebuild_preserves_arguments_exit_status_and_temporary_runner_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace with 中文 and spaces");
    workspace(&root);
    let target = root.join("compiler outputs 中文");
    let output = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "--package",
            "seekdeep-repository-runner",
            "--",
            "ci-windows-complete",
            "two words",
            "中文",
            "",
        ])
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_INCREMENTAL", "0")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(17),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("workspace rebuilt"));
    assert_eq!(
        std::fs::read_to_string(root.join("observed-arguments")).unwrap(),
        "ci-windows-complete\0two words\0中文\0"
    );
    let executable = std::fs::read_to_string(root.join("observed-executable")).unwrap();
    let executable = Path::new(&executable);
    assert!(
        !executable.starts_with(target.canonicalize().unwrap()),
        "the running gate still occupies Cargo's executable output"
    );
    assert!(!executable.parent().unwrap().exists());
    assert!(
        target
            .join("debug")
            .join(format!("run-gates{}", std::env::consts::EXE_SUFFIX))
            .is_file()
    );
}
