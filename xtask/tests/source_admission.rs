//! Source-dependent commands must reject checkout drift before reading or writing artifacts.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const CATALOG: &str = "crates/cordis-client-runner/data/slot-catalog.json";

struct Fixture {
    directory: tempfile::TempDir,
    source: PathBuf,
    revision: String,
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Oracle admission fixture",
            "-c",
            "user.email=oracle@example.invalid",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("original source 中文");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "--quiet"]);
        git(
            &source,
            &["commit", "--quiet", "--allow-empty", "-m", "Pinned source"],
        );
        let revision = git(&source, &["rev-parse", "HEAD"]);
        std::fs::write(
            directory.path().join("SOURCE_SNAPSHOT"),
            format!("repository={}\ncommit={revision}\n", source.display()),
        )
        .unwrap();
        Self {
            directory,
            source,
            revision,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .current_dir(self.directory.path())
            .env_remove("SEEKDEEP_PARITY_SOURCE")
            .arg("client-catalog");
        command
    }

    fn catalog(&self, check: bool) -> Output {
        let mut command = self.command();
        command.arg("--source").arg(&self.source);
        if check {
            command.arg("--check");
        }
        command.output().unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn unchanged_source_files_at_a_different_commit_cannot_validate_a_catalog() {
    let fixture = Fixture::new();
    let generated = fixture.catalog(false);
    assert!(generated.status.success(), "{}", text(&generated));
    let checked = fixture.catalog(true);
    assert!(checked.status.success(), "{}", text(&checked));

    git(
        &fixture.source,
        &["commit", "--quiet", "--allow-empty", "-m", "Source drift"],
    );
    let drifted = git(&fixture.source, &["rev-parse", "HEAD"]);
    let rejected = fixture.catalog(true);
    assert!(!rejected.status.success(), "{}", text(&rejected));
    let diagnostic = text(&rejected);
    assert!(diagnostic.contains(&fixture.revision), "{diagnostic}");
    assert!(diagnostic.contains(&drifted), "{diagnostic}");
}

#[test]
fn missing_source_cannot_generate_an_empty_catalog() {
    let fixture = Fixture::new();
    let rejected = fixture
        .command()
        .arg("--source")
        .arg(fixture.directory.path().join("missing source"))
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "{}", text(&rejected));
    assert!(text(&rejected).contains("Resolve source oracle"));
    assert!(!fixture.directory.path().join(CATALOG).exists());
}

#[test]
fn explicit_source_precedes_environment_and_environment_is_validated() {
    let fixture = Fixture::new();
    let missing = fixture.directory.path().join("missing environment source");
    let accepted = fixture
        .command()
        .env("SEEKDEEP_PARITY_SOURCE", &missing)
        .arg("--source")
        .arg(&fixture.source)
        .output()
        .unwrap();
    assert!(accepted.status.success(), "{}", text(&accepted));

    let rejected = fixture
        .command()
        .env("SEEKDEEP_PARITY_SOURCE", &missing)
        .arg("--check")
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "{}", text(&rejected));
    assert!(text(&rejected).contains("Resolve source oracle"));
}
