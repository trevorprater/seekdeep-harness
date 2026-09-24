//! Removed TypeScript implementations retain their pinned declaration and `JSDoc` checks.

use std::{path::Path, process::Command};

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, source_oracle::SourceOracle,
    ts_project::locate_repository_library,
};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Oracle fixture",
            "-c",
            "user.email=oracle@example.invalid",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn verify(root: &Path) -> (bool, String) {
    run(env!("CARGO_BIN_EXE_verify-type-equiv"), root)
}

fn run(binary: &str, root: &Path) -> (bool, String) {
    let library = locate_repository_library(compiled_repository_root()).unwrap();
    let output = Command::new(binary)
        .arg("--root")
        .arg(root)
        .env("SEEKDEEP_TYPESCRIPT_LIBRARY", library)
        .env_remove("SEEKDEEP_PARITY_SOURCE")
        .output()
        .unwrap();
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn package_paths_accept_pinned_specifications_but_reject_untracked_or_misspelled_files() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("seekdeep-harness");
    let source = temporary.path().join("deepseek-harness");
    let original = "packages/subagent/subagent-dsh-sdk/src/index.ts";
    let tracked = "packages/subagent/subagent-seekdeep-sdk/src/index.ts";
    let absent = "packages/subagent/subagent-seekdeep-sdk/src/absent.ts";
    write(&source, original, "export {};\n");
    git(&source, &["init", "--quiet"]);
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "Pinned source paths"]);
    let revision = git(&source, &["rev-parse", "HEAD"]);
    write(
        &root,
        "SOURCE_SNAPSHOT",
        &format!("repository={}\ncommit={revision}\n", source.display()),
    );
    write(
        &root,
        "packages/subagent/subagent-seekdeep-sdk/package.json",
        "{}\n",
    );
    write(&root, "README.md", &format!("{tracked}\n{absent}\n"));
    let binary = env!("CARGO_BIN_EXE_verify-package-paths");
    let (passed, report) = run(binary, &root);
    assert!(!passed, "{report}");
    assert!(report.contains(absent), "{report}");
    assert!(!report.contains(tracked), "{report}");

    write(
        &source,
        &absent.replace("seekdeep-", "dsh-"),
        "export {};\n",
    );
    let oracle = SourceOracle::open_with_root(&root, Some(&source)).unwrap();
    let paths = oracle.paths().unwrap();
    assert!(paths.contains(original));
    assert!(paths.contains("packages/subagent/subagent-dsh-sdk"));
    assert!(!paths.contains(&absent.replace("seekdeep-", "dsh-")));
    let (passed, report) = run(binary, &root);
    assert!(!passed, "{report}");
    assert!(report.contains(absent), "{report}");

    write(&root, absent, "export {};\n");
    let (passed, report) = run(binary, &root);
    assert!(passed, "{report}");
}

#[test]
fn pinned_objects_preserve_strict_declarations_and_reject_document_or_revision_drift() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("seekdeep-harness");
    let source = temporary.path().join("deepseek-harness");
    let source_path = "packages/subagent/subagent-dsh-sdk/src/index.ts";
    let original = "/** DeepSeek Harness config. */\nexport interface Config { env: 'DSH_HOME'; value: string; }\nexport class Api {\n  /** Read. */\n  read(): string { return 'value'; }\n  private hidden = 1;\n}\n";
    write(&source, source_path, original);
    git(&source, &["init", "--quiet"]);
    git(&source, &["add", "."]);
    git(&source, &["commit", "--quiet", "-m", "Pinned declarations"]);
    let revision = git(&source, &["rev-parse", "HEAD"]);
    write(
        &root,
        "SOURCE_SNAPSHOT",
        &format!("repository={}\ncommit={revision}\n", source.display()),
    );
    write(&root, "package.json", "{\"private\":true}\n");
    let document = "```ts type-equiv\n/** SeekDeep Harness config. */\ninterface Config { env: 'SEEKDEEP_HOME'; value: string; }\n```\n\n```ts public-api\ndeclare class Api {\n  /** Read. */\n  read(): string;\n}\n```\n";
    write(&root, "README.md", document);
    write(
        &root,
        "scripts/type-equiv.manifest.json",
        &serde_json::json!({"entries":[
            {"doc":"README.md", "source":"packages/subagent/subagent-seekdeep-sdk/src/index.ts", "symbol":"Config"},
            {"doc":"README.md", "source":"packages/subagent/subagent-seekdeep-sdk/src/index.ts", "symbol":"Api", "projection":"public-api"}
        ]}).to_string(),
    );
    let (passed, report) = verify(&root);
    assert!(passed, "{report}");
    assert!(report.contains("2 type-equiv block(s) match"), "{report}");

    let library = locate_repository_library(compiled_repository_root()).unwrap();
    let inherited = Command::new(env!("CARGO_BIN_EXE_verify-type-equiv"))
        .arg("--root")
        .arg(&root)
        .env("SEEKDEEP_TYPESCRIPT_LIBRARY", library)
        .env("SEEKDEEP_PARITY_SOURCE", &source)
        .env("GIT_DIR", root.join("absent-git-directory"))
        .env("GIT_WORK_TREE", &root)
        .env("GIT_INDEX_FILE", root.join("absent-index"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.bare")
        .env("GIT_CONFIG_VALUE_0", "true")
        .output()
        .unwrap();
    assert!(
        inherited.status.success(),
        "{}{}",
        String::from_utf8_lossy(&inherited.stdout),
        String::from_utf8_lossy(&inherited.stderr)
    );

    write(
        &source,
        source_path,
        "export interface Config { wrong: boolean; }\n",
    );
    let oracle = SourceOracle::open_with_root(&root, Some(&source)).unwrap();
    assert_eq!(oracle.read(source_path).unwrap(), original);
    assert!(oracle.read("../outside.ts").is_err());
    assert!(oracle.read("absent.ts").is_err());
    let (passed, report) = verify(&root);
    assert!(passed, "{report}");

    for drift in [
        document.replace("value: string", "value: number"),
        document.replace("/** Read. */", "/** Changed. */"),
    ] {
        write(&root, "README.md", &drift);
        let (passed, report) = verify(&root);
        assert!(!passed, "{report}");
        assert!(report.contains("DRIFT:"), "{report}");
    }
    write(&root, "README.md", document);
    git(&source, &["add", "."]);
    git(
        &source,
        &["commit", "--quiet", "-m", "Unpinned declarations"],
    );
    let (passed, report) = verify(&root);
    assert!(!passed, "{report}");
    assert!(
        report.contains("expected") && report.contains(&revision),
        "{report}"
    );
}
