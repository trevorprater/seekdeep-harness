//! Corpus, named, write, list, structural, and staged-index command fixtures.

use std::process::Command;

use seekdeep_repository_tools::{
    translation_pairing_command::run_translation_pairing,
    translation_pairing_git::git_blob_hash,
    translation_pairing_record::{
        TranslationPairingRecord, render_translation_pairing_record, translation_pair_paths,
    },
};
use tempfile::TempDir;

fn repository() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "scripts/translation-pairing.manifest.json",
        "{\"excluded\":[]}\n",
    );
    root
}

fn write(root: &TempDir, relative: &str, content: &str) {
    let path = root.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn valid_pair(root: &TempDir, stem: &str, record: bool) {
    let source = format!("# Title\n\nEnglish | [中文]({stem}.zh.md)\n\nBody.\n");
    let zh = format!("# 标题\n\n[English]({stem}.md) | 中文\n\n正文。\n");
    let source_path = format!("docs/{stem}.md");
    let zh_path = format!("docs/{stem}.zh.md");
    write(root, &source_path, &source);
    write(root, &zh_path, &zh);
    if record {
        let paths = translation_pair_paths(&source_path).unwrap();
        let metadata = render_translation_pairing_record(
            &paths,
            &TranslationPairingRecord {
                source_hash: git_blob_hash(source.as_bytes()),
                zh_hash: git_blob_hash(zh.as_bytes()),
            },
        );
        write(root, &paths.metadata, &metadata);
    }
}

fn init_git(root: &TempDir) {
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(root.path())
            .env("GIT_DEFAULT_HASH", "sha1")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn complete_corpus_and_named_pair_checks_succeed() {
    let root = repository();
    valid_pair(&root, "one", true);
    let corpus = run_translation_pairing(root.path(), &[]).unwrap();
    assert_eq!(corpus.exit_code, 0);
    assert_eq!(
        corpus.stdout,
        "verify-translation-pairing: 1 pair(s) checked across all in-scope documentation, all consistent.\n"
    );
    let named = run_translation_pairing(root.path(), &["docs/one.zh.md".to_owned()]).unwrap();
    assert_eq!(named.exit_code, 0);
    assert!(named.stdout.contains("1 named pair(s) consistent"));
}

#[test]
fn missing_counterpart_fails_and_list_remains_nonfailing() {
    let root = repository();
    write(&root, "docs/missing.md", "# Missing\n");
    let check = run_translation_pairing(root.path(), &[]).unwrap();
    assert_eq!(check.exit_code, 1);
    assert!(check.stderr.contains("must merge bilingual"));
    let list = run_translation_pairing(root.path(), &["--list".to_owned()]).unwrap();
    assert_eq!(list.exit_code, 0);
    assert!(
        list.stdout
            .contains("missing     docs/missing.md  (required)")
    );
    assert!(list.stdout.contains("0 ok, 0 out-of-sync, 1 missing"));
}

#[test]
fn scoped_write_records_exact_blobs_then_check_succeeds() {
    let root = repository();
    init_git(&root);
    valid_pair(&root, "write", false);
    let write_output = run_translation_pairing(
        root.path(),
        &["--write".to_owned(), "docs/write.md".to_owned()],
    )
    .unwrap();
    assert_eq!(write_output.exit_code, 0);
    assert!(
        write_output
            .stdout
            .contains("recorded docs/write.i18n.yaml")
    );
    assert!(root.path().join("docs/write.i18n.yaml").is_file());
    let check = run_translation_pairing(root.path(), &["docs/write.md".to_owned()]).unwrap();
    assert_eq!(check.exit_code, 0, "{}", check.stderr);
}

#[test]
fn content_drift_is_out_of_sync_and_precedes_missing_in_list_order() {
    let root = repository();
    valid_pair(&root, "drift", true);
    write(&root, "docs/drift.md", "# changed\n");
    write(&root, "docs/missing.md", "# Missing\n");
    let list = run_translation_pairing(root.path(), &["--list".to_owned()]).unwrap();
    let drift = list.stdout.find("out-of-sync docs/drift.md").unwrap();
    let missing = list.stdout.find("missing     docs/missing.md").unwrap();
    assert!(drift < missing);
    assert!(list.stdout.contains("0 ok, 1 out-of-sync, 1 missing"));
}

#[test]
fn cached_named_check_reads_staged_blobs_not_worktree_drift() {
    let root = repository();
    init_git(&root);
    valid_pair(&root, "cached", true);
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(["add", "."])
            .status()
            .unwrap()
            .success()
    );
    write(&root, "docs/cached.md", "# worktree drift\n");
    let output = run_translation_pairing(
        root.path(),
        &["--cached".to_owned(), "docs/cached.md".to_owned()],
    )
    .unwrap();
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(output.stdout.contains("1 named staged pair(s) consistent"));
}

#[test]
fn structural_and_switcher_divergence_fails_after_hash_confirmation() {
    let root = repository();
    let source = "# Title\n\n- one\n- two\n";
    let zh = "# 标题\n\n- 一\n";
    write(&root, "docs/bad.md", source);
    write(&root, "docs/bad.zh.md", zh);
    let paths = translation_pair_paths("docs/bad.md").unwrap();
    write(
        &root,
        &paths.metadata,
        &render_translation_pairing_record(
            &paths,
            &TranslationPairingRecord {
                source_hash: git_blob_hash(source.as_bytes()),
                zh_hash: git_blob_hash(zh.as_bytes()),
            },
        ),
    );
    let output = run_translation_pairing(root.path(), &[]).unwrap();
    assert_eq!(output.exit_code, 1);
    assert!(output.stderr.contains("missing language switcher"));
    assert!(output.stderr.contains("list (kind, start, item count)"));
}
