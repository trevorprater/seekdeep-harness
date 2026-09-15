//! Source-oracle coverage for frozen archive triplets and immutable manifests.

use indexmap::IndexMap;
use seekdeep_repository_tools::{
    agent_note_tree::is_archived_agent_note_path,
    archived_agent_notes::{
        ArchiveManifest, extend_archive_manifest, git_blob_hash, parse_archive_manifest,
        render_archive_manifest, validate_archive_artifacts, validate_archive_manifest_extension,
        verify_archived_agent_notes,
    },
};

fn fixture() -> IndexMap<String, Vec<u8>> {
    let base = "2026-07-26-example";
    let source = format!(
        "# Agent Note: Example\n\nStatus: implemented\nArchived: 2026-07-26\n\nEnglish | [中文]({base}.zh.md)\n\n## Problem\n\nExample.\n"
    )
    .into_bytes();
    let chinese = format!(
        "# Agent Note: 示例\n\nStatus: implemented\nArchived: 2026-07-26\n\n[English]({base}.md) | 中文\n\n## 问题\n\n示例。\n"
    )
    .into_bytes();
    let metadata = format!(
        "{base}.md: {}\n{base}.zh.md: {}\n",
        git_blob_hash(&source),
        git_blob_hash(&chinese)
    )
    .into_bytes();
    IndexMap::from([
        (format!("process/{base}.md"), source),
        (format!("process/{base}.zh.md"), chinese),
        (format!("process/{base}.i18n.yaml"), metadata),
    ])
}

#[test]
fn recognizes_archived_paths_with_posix_and_windows_separators() {
    assert!(is_archived_agent_note_path(
        ".agents/notes/archived/process/example.md"
    ));
    assert!(is_archived_agent_note_path(
        ".agents\\notes\\archived\\process\\example.md"
    ));
    assert!(!is_archived_agent_note_path(
        ".agents/notes/implemented/process/example.md"
    ));
}

#[test]
fn accepts_complete_implemented_triplet_with_matching_metadata() {
    assert!(validate_archive_artifacts(&fixture()).is_empty());
}

#[test]
fn rejects_incomplete_triplets_and_invalid_archive_headers() {
    let mut artifacts = fixture();
    artifacts.shift_remove("process/2026-07-26-example.i18n.yaml");
    artifacts.insert(
        "process/2026-07-26-example.md".into(),
        b"# Agent Note: Example\n\nStatus: proposed\nArchived: yesterday\n".to_vec(),
    );
    assert!(
        validate_archive_artifacts(&artifacts)
            .join("\n")
            .contains("incomplete archived triplet")
    );
}

#[test]
fn extension_never_permits_a_sealed_change_or_removal() {
    let artifacts = fixture();
    let empty = ArchiveManifest {
        version: 1,
        files: IndexMap::new(),
    };
    let first = extend_archive_manifest(&empty, &artifacts);
    assert!(first.errors.is_empty());
    assert_eq!(first.added.len(), 3);

    let sealed = ArchiveManifest {
        version: 1,
        files: first.files,
    };
    let mut changed = artifacts.clone();
    changed.insert("process/2026-07-26-example.md".into(), b"changed".to_vec());
    assert_eq!(
        extend_archive_manifest(&sealed, &changed).errors,
        ["process/2026-07-26-example.md: sealed content hash changed"]
    );
    changed.shift_remove("process/2026-07-26-example.zh.md");
    assert!(
        extend_archive_manifest(&sealed, &changed)
            .errors
            .contains(&"process/2026-07-26-example.zh.md: sealed artifact is missing".into())
    );
}

#[test]
fn replacing_manifest_seals_cannot_hide_changed_archive_content() {
    let artifacts = fixture();
    let empty = ArchiveManifest {
        version: 1,
        files: IndexMap::new(),
    };
    let initial = extend_archive_manifest(&empty, &artifacts);
    let baseline = ArchiveManifest {
        version: 1,
        files: initial.files,
    };
    let path = "process/2026-07-26-example.md";
    let mut changed = artifacts.clone();
    changed.insert(path.into(), b"changed".to_vec());
    let replacement = extend_archive_manifest(&empty, &changed);
    let current = ArchiveManifest {
        version: 1,
        files: replacement.files,
    };
    assert!(
        extend_archive_manifest(&current, &changed)
            .errors
            .is_empty()
    );
    assert_eq!(
        validate_archive_manifest_extension(&baseline, &current),
        [format!("{path}: sealed manifest hash changed")]
    );
    let mut removed = current.clone();
    removed.files.shift_remove(path);
    assert!(
        validate_archive_manifest_extension(&baseline, &removed)
            .contains(&format!("{path}: sealed manifest entry is missing"))
    );
}

#[test]
fn deterministic_manifest_schema_round_trips() {
    let files = IndexMap::from([("process/z.md".into(), format!("sha256:{}", "a".repeat(64)))]);
    let content = render_archive_manifest(&files).unwrap();
    assert_eq!(
        parse_archive_manifest(&content).unwrap(),
        ArchiveManifest { version: 1, files }
    );
}

#[test]
fn write_mode_appends_only_new_seals_and_refuses_a_later_content_change() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(".agents/notes/archived");
    for kind in [
        "architecture",
        "bug-fix",
        "feature",
        "process",
        "simplification",
        "testing",
    ] {
        std::fs::create_dir_all(archive.join(kind)).unwrap();
    }
    std::fs::write(archive.join("AGENTS.md"), "archive instructions\n").unwrap();
    for (relative, content) in fixture() {
        std::fs::write(archive.join(relative), content).unwrap();
    }
    git(root.path(), &["init", "--initial-branch=master"]);
    git(
        root.path(),
        &["config", "user.email", "archive@example.com"],
    );
    git(root.path(), &["config", "user.name", "Archive Tests"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    git(root.path(), &["add", ".agents/notes/archived/AGENTS.md"]);
    git(root.path(), &["commit", "-m", "baseline"]);

    let written = verify_archived_agent_notes(root.path(), true, "HEAD").unwrap();
    assert!(written.errors.is_empty());
    assert_eq!(written.added, 3);
    let manifest_path = archive.join("manifest.json");
    let sealed = std::fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(parse_archive_manifest(&sealed).unwrap().files.len(), 3);
    let checked = verify_archived_agent_notes(root.path(), false, "HEAD").unwrap();
    assert!(checked.errors.is_empty());
    assert_eq!((checked.artifacts, checked.kinds), (3, 6));
    let repeated = verify_archived_agent_notes(root.path(), true, "HEAD").unwrap();
    assert!(repeated.errors.is_empty());
    assert_eq!(repeated.added, 0);
    assert_eq!(std::fs::read_to_string(&manifest_path).unwrap(), sealed);

    std::fs::write(archive.join("process/2026-07-26-example.md"), "changed\n").unwrap();
    let rejected = verify_archived_agent_notes(root.path(), true, "HEAD").unwrap();
    assert!(
        rejected
            .errors
            .iter()
            .any(|error| error.contains("sealed content hash changed"))
    );
    assert_eq!(std::fs::read_to_string(manifest_path).unwrap(), sealed);
}

fn git(root: &std::path::Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
