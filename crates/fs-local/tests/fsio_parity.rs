//! Cordis-free path, read, edit, and atomic-publication parity.

use seekdeep_fs::types::{FsErrorCode, FsKind};
use seekdeep_fs_local::fsio::{
    LineEndings, apply_literal_edit, list_directory, probe, read_diff_basis, read_whole_bytes,
    resolve_local_target, restore_line_endings, write_file_atomic,
};
use seekdeep_llm::AbortSignal;

#[tokio::test]
async fn resolution_uses_deepest_existing_identity_and_rejects_blank_or_file_ancestors() {
    let root = tempfile::tempdir().expect("root");
    let real = root.path().join("real");
    std::fs::create_dir(&real).expect("real");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let alias = root.path().join("alias");
        symlink(&real, &alias).expect("alias");
        let target = resolve_local_target(root.path().to_str().unwrap(), "alias/missing/deep.txt")
            .await
            .unwrap();
        assert_eq!(
            target.target_key.as_str(),
            real.canonicalize()
                .unwrap()
                .join("missing/deep.txt")
                .to_string_lossy()
        );
    }
    assert_eq!(
        resolve_local_target(root.path().to_str().unwrap(), "   ")
            .await
            .unwrap_err()
            .code,
        FsErrorCode::FsNotFound
    );
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, "file").expect("blocker");
    assert_eq!(
        resolve_local_target(root.path().to_str().unwrap(), "blocker/child")
            .await
            .unwrap_err()
            .code,
        FsErrorCode::FsNotFound
    );
}

#[tokio::test]
async fn probe_and_listing_preserve_stable_order_types_and_child_identity() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("z.txt"), "z").expect("z");
    std::fs::write(root.path().join("a.txt"), "a").expect("a");
    std::fs::create_dir(root.path().join("dir")).expect("dir");
    let root_target = resolve_local_target("/", root.path().to_str().unwrap())
        .await
        .unwrap();
    let entries = list_directory(&root_target, None).await.unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["a.txt", "dir", "z.txt"]
    );
    assert_eq!(entries[0].kind, FsKind::File);
    assert_eq!(entries[1].kind, FsKind::Directory);
    assert_eq!(
        entries[0].target.display_path,
        root.path().join("a.txt").to_string_lossy()
    );
    assert!(
        probe(root.path().join("missing").to_str().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        list_directory(
            &resolve_local_target("/", root.path().join("a.txt").to_str().unwrap())
                .await
                .unwrap(),
            None,
        )
        .await
        .unwrap_err()
        .code,
        FsErrorCode::FsNotDirectory
    );
}

#[tokio::test]
async fn bounded_reads_and_diff_basis_enforce_exclusive_limits_and_text_rules() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("file");
    std::fs::write(&path, "12345\r\n").expect("file");
    let target = resolve_local_target("/", path.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        read_whole_bytes(&target, None, 7).await.unwrap(),
        b"12345\r\n"
    );
    assert_eq!(
        read_whole_bytes(&target, None, 6).await.unwrap_err().code,
        FsErrorCode::FsTooLarge
    );
    assert_eq!(
        read_diff_basis(path.to_str().unwrap(), None, 8)
            .await
            .unwrap()
            .as_deref(),
        Some("12345\n")
    );
    assert_eq!(
        read_diff_basis(path.to_str().unwrap(), None, 7)
            .await
            .unwrap(),
        None
    );
    std::fs::write(&path, [0, 1, 2]).expect("binary");
    assert_eq!(
        read_diff_basis(path.to_str().unwrap(), None, 8)
            .await
            .unwrap(),
        None
    );
    std::fs::write(&path, [0xff, 0xfe]).expect("invalid utf8");
    assert_eq!(
        read_diff_basis(path.to_str().unwrap(), None, 8)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn atomic_write_creates_parents_preserves_mode_cleans_staging_and_guards_create() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("nested/file.txt");
    write_file_atomic(path.to_str().unwrap(), "first", None, None, None)
        .await
        .expect("create");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("mode");
        write_file_atomic(path.to_str().unwrap(), "second", Some(0o640), None, None)
            .await
            .expect("replace");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
    }
    assert!(
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmpdir"))
    );

    let guarded = root.path().join("guarded.txt");
    write_file_atomic(
        guarded.to_str().unwrap(),
        "winner",
        None,
        None,
        Some("unresolved/guarded.txt"),
    )
    .await
    .expect("guarded create");
    let error = write_file_atomic(
        guarded.to_str().unwrap(),
        "loser",
        None,
        None,
        Some("unresolved/guarded.txt"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, FsErrorCode::FsNotObserved);
    assert!(error.message.contains("unresolved/guarded.txt"));
    assert_eq!(std::fs::read_to_string(&guarded).unwrap(), "winner");

    let aborted = AbortSignal::default();
    aborted.abort();
    let aborted_path = root.path().join("aborted.txt");
    assert_eq!(
        write_file_atomic(
            aborted_path.to_str().unwrap(),
            "nope",
            None,
            Some(&aborted),
            None,
        )
        .await
        .unwrap_err()
        .code,
        FsErrorCode::FsAborted
    );
    assert!(!aborted_path.exists());
}

#[test]
fn literal_edit_and_line_ending_helpers_preserve_exact_codes_and_style() {
    assert_eq!(
        apply_literal_edit("one two", "two", "three", false, "file").unwrap(),
        ("one three".to_owned(), 1)
    );
    assert_eq!(
        apply_literal_edit("x x", "x", "y", false, "file")
            .unwrap_err()
            .code,
        FsErrorCode::FsAmbiguousEdit
    );
    assert_eq!(
        apply_literal_edit("x x", "x", "y", true, "file").unwrap(),
        ("y y".to_owned(), 2)
    );
    assert_eq!(
        apply_literal_edit("x", "", "y", false, "file")
            .unwrap_err()
            .code,
        FsErrorCode::FsEditNotFound
    );
    assert_eq!(
        restore_line_endings("one\ntwo\n", LineEndings::Crlf),
        "one\r\ntwo\r\n"
    );
    assert_eq!(restore_line_endings("one\n", LineEndings::Lf), "one\n");
}
