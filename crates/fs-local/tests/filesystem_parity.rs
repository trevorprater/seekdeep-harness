//! Local filesystem service parity over real host files.

use std::{path::Path, sync::Arc};

use futures::TryStreamExt as _;
use seekdeep_cordis::Context;
use seekdeep_fs::{
    FS, FileSystem,
    types::{FsEditRequest, FsError, FsErrorCode, FsKind, FsPathKind, FsWriteIntent},
};
use seekdeep_fs_local::{Config, LocalFileSystem};
use seekdeep_llm::AbortSignal;

fn filesystem(root: &Path, diff_limit: Option<u64>) -> Arc<LocalFileSystem> {
    LocalFileSystem::new(Config {
        cwd: Some(root.to_string_lossy().into_owned()),
        diff_basis_max_bytes: diff_limit,
    })
    .expect("filesystem")
}

fn code(error: &anyhow::Error) -> FsErrorCode {
    error
        .downcast_ref::<FsError>()
        .expect("typed fs error")
        .code
}

#[tokio::test]
async fn config_resolution_metadata_urls_and_lstat_match_the_source() {
    assert!(
        LocalFileSystem::new(Config {
            diff_basis_max_bytes: Some(0),
            ..Config::default()
        })
        .is_err()
    );
    assert!(
        LocalFileSystem::new(Config {
            diff_basis_max_bytes: Some(1_073_741_824),
            ..Config::default()
        })
        .is_err()
    );

    let root = tempfile::tempdir().expect("root");
    let alternate = tempfile::tempdir().expect("alternate");
    let fs = filesystem(root.path(), None);
    let path = alternate.path().join("file.txt");
    std::fs::write(&path, "first").expect("file");
    let target = fs
        .resolve("file.txt", Some(alternate.path().to_str().unwrap()), None)
        .await
        .expect("relative resolve");
    assert_eq!(
        fs.process_path(&target),
        path.canonicalize().unwrap().to_string_lossy()
    );
    assert!(fs.file_url(&target).starts_with("file://"));
    let root_target = fs
        .resolve(alternate.path().to_str().unwrap(), None, None)
        .await
        .expect("root target");
    assert!(fs.contains(&root_target, &target));

    let before = fs.stat(&target, None).await.unwrap().expect("stat");
    assert_eq!(before.kind, FsKind::File);
    assert_eq!(before.size, Some(5));
    std::fs::write(&path, "other").expect("same-size rewrite");
    let after = fs.stat(&target, None).await.unwrap().expect("stat after");
    assert_ne!(before.version, after.version);
    assert!(
        fs.stat(&fs.resolve("missing", None, None).await.unwrap(), None)
            .await
            .unwrap()
            .is_none()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = alternate.path().join("link");
        symlink(&path, &link).expect("link");
        assert_eq!(
            fs.lstat(link.to_str().unwrap(), None, None)
                .await
                .unwrap()
                .expect("lstat")
                .kind,
            FsPathKind::Symlink
        );
    }
}

#[tokio::test]
async fn text_stream_bytes_and_directory_listing_preserve_types_and_bounds() {
    let root = tempfile::tempdir().expect("root");
    let fs = filesystem(root.path(), None);
    let text_path = root.path().join("large.txt");
    let text = format!("{}😀tail", "a".repeat(16_383));
    std::fs::write(&text_path, &text).expect("text");
    let target = fs.resolve("large.txt", None, None).await.unwrap();
    assert_eq!(fs.read_text(&target, None).await.unwrap(), text);
    let chunks = fs
        .stream_text(&target, None)
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(chunks.concat(), text);
    assert_eq!(
        fs.read_bytes(&target, None, text.len()).await.unwrap(),
        text.as_bytes()
    );
    assert_eq!(
        code(
            &fs.read_bytes(&target, None, text.len() - 1)
                .await
                .unwrap_err()
        ),
        FsErrorCode::FsTooLarge
    );

    let binary_path = root.path().join("binary.bin");
    std::fs::write(&binary_path, [0, 159, 255]).expect("binary");
    let binary = fs.resolve("binary.bin", None, None).await.unwrap();
    assert_eq!(
        code(&fs.read_text(&binary, None).await.unwrap_err()),
        FsErrorCode::FsNotText
    );
    assert_eq!(
        fs.read_bytes(&binary, None, 3).await.unwrap(),
        [0, 159, 255]
    );

    std::fs::create_dir(root.path().join("directory")).expect("directory");
    std::fs::write(root.path().join("z.txt"), "z").expect("z");
    std::fs::write(root.path().join("a.txt"), "a").expect("a");
    let root_target = fs.resolve(".", None, None).await.unwrap();
    let entries = fs.list_dir(&root_target, None).await.unwrap();
    let names = entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["a.txt", "binary.bin", "directory", "large.txt", "z.txt"]
    );
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.name == "directory")
            .unwrap()
            .kind,
        FsKind::Directory
    );
}

#[tokio::test]
async fn guarded_writes_are_atomic_and_report_contextual_before_after() {
    let root = tempfile::tempdir().expect("root");
    let fs = filesystem(root.path(), Some(8));
    let target = fs.resolve("file.txt", None, None).await.unwrap();
    let created = fs
        .write_text(
            &target,
            "old\r\n",
            Some(&FsWriteIntent::CreateIfAbsent),
            None,
            None,
        )
        .await
        .expect("create");
    assert_eq!(created.before, None);
    assert_eq!(created.after, "old\n");
    let version = fs.stat(&target, None).await.unwrap().unwrap().version;
    let replaced = fs
        .write_text(
            &target,
            "new\r\n",
            Some(&FsWriteIntent::ReplaceIfVersion {
                version: version.clone(),
            }),
            None,
            None,
        )
        .await
        .expect("replace");
    assert_eq!(replaced.before.as_deref(), Some("old\n"));
    assert_eq!(replaced.after, "new\n");
    assert_eq!(
        code(
            &fs.write_text(
                &target,
                "stale",
                Some(&FsWriteIntent::ReplaceIfVersion { version }),
                None,
                None,
            )
            .await
            .unwrap_err()
        ),
        FsErrorCode::FsStaleVersion
    );
    assert_eq!(
        code(
            &fs.write_text(
                &target,
                "again",
                Some(&FsWriteIntent::CreateIfAbsent),
                None,
                None,
            )
            .await
            .unwrap_err()
        ),
        FsErrorCode::FsNotObserved
    );

    std::fs::write(target.target_key.as_str(), [0, 1, 2]).expect("binary prior");
    let outcome = fs
        .write_text(&target, "text", None, None, None)
        .await
        .expect("binary overwrite");
    assert_eq!(outcome.before, None);

    std::fs::write(target.target_key.as_str(), "12345678").expect("bound prior");
    let outcome = fs
        .write_text(&target, "small", None, None, None)
        .await
        .expect("bounded overwrite");
    assert_eq!(outcome.before, None);
}

#[tokio::test]
async fn competing_creators_and_guarded_writers_preserve_the_winner() {
    let root = tempfile::tempdir().expect("root");
    let left = filesystem(root.path(), None);
    let right = filesystem(root.path(), None);
    let left_target = left.resolve("race.txt", None, None).await.unwrap();
    let right_target = right.resolve("race.txt", None, None).await.unwrap();
    let intent = FsWriteIntent::CreateIfAbsent;
    let (left_result, right_result) = tokio::join!(
        left.write_text(&left_target, "left", Some(&intent), None, None),
        right.write_text(&right_target, "right", Some(&intent), None, None),
    );
    assert_ne!(left_result.is_ok(), right_result.is_ok());
    let winner = std::fs::read_to_string(root.path().join("race.txt")).unwrap();
    assert!(winner == "left" || winner == "right");
    let loser = left_result.err().or_else(|| right_result.err()).unwrap();
    assert_eq!(code(&loser), FsErrorCode::FsNotObserved);

    let fs = filesystem(root.path(), None);
    let target = fs.resolve("guarded.txt", None, None).await.unwrap();
    fs.write_text(&target, "base", None, None, None)
        .await
        .unwrap();
    let version = fs.stat(&target, None).await.unwrap().unwrap().version;
    let first = FsWriteIntent::ReplaceIfVersion {
        version: version.clone(),
    };
    let second = FsWriteIntent::ReplaceIfVersion { version };
    let (first_result, second_result) = tokio::join!(
        fs.write_text(&target, "first", Some(&first), None, None),
        fs.write_text(&target, "second", Some(&second), None, None),
    );
    assert_ne!(first_result.is_ok(), second_result.is_ok());
    assert_eq!(
        code(&first_result.err().or_else(|| second_result.err()).unwrap()),
        FsErrorCode::FsStaleVersion
    );
}

#[tokio::test]
async fn literal_edits_preserve_line_endings_versions_and_error_precedence() {
    let root = tempfile::tempdir().expect("root");
    let fs = filesystem(root.path(), None);
    let path = root.path().join("edit.txt");
    std::fs::write(&path, "one\r\ntwo\r\ntwo\r\n").expect("file");
    let target = fs.resolve("edit.txt", None, None).await.unwrap();
    let version = fs.stat(&target, None).await.unwrap().unwrap().version;
    assert_eq!(
        code(
            &fs.edit_text(
                &target,
                &FsEditRequest {
                    old_string: "two".to_owned(),
                    new_string: "changed".to_owned(),
                    replace_all: false,
                },
                Some(&version),
                None,
                None,
            )
            .await
            .unwrap_err()
        ),
        FsErrorCode::FsAmbiguousEdit
    );
    let outcome = fs
        .edit_text(
            &target,
            &FsEditRequest {
                old_string: "two".to_owned(),
                new_string: "changed".to_owned(),
                replace_all: true,
            },
            Some(&version),
            None,
            None,
        )
        .await
        .expect("edit all");
    assert_eq!(outcome.before, "one\ntwo\ntwo\n");
    assert_eq!(outcome.after, "one\nchanged\nchanged\n");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one\r\nchanged\r\nchanged\r\n"
    );

    let stale = fs
        .edit_text(
            &target,
            &FsEditRequest {
                old_string: "absent".to_owned(),
                new_string: "x".to_owned(),
                replace_all: false,
            },
            Some(&version),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&stale), FsErrorCode::FsStaleVersion);
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_aliases_share_identity_lock_and_mutate_the_real_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let fs = filesystem(root.path(), None);
    let real = root.path().join("real.txt");
    let link = root.path().join("link.txt");
    std::fs::write(&real, "base").expect("real");
    symlink(&real, &link).expect("link");
    let real_target = fs.resolve("real.txt", None, None).await.unwrap();
    let link_target = fs.resolve("link.txt", None, None).await.unwrap();
    assert_eq!(real_target.target_key, link_target.target_key);
    fs.write_text(&link_target, "through-link", None, None, None)
        .await
        .expect("write link");
    assert_eq!(std::fs::read_to_string(real).unwrap(), "through-link");
}

#[tokio::test]
async fn plugin_disposal_withdraws_ctx_fs() {
    let root = tempfile::tempdir().expect("root");
    let context = Context::new();
    let plugin = context
        .plugin(
            seekdeep_fs_local::plugin(),
            serde_json::json!({"cwd": root.path()}),
        )
        .expect("plugin");
    plugin.await_settled().await.expect("active");
    assert!(context.get(FS).is_some());
    plugin.dispose().await.expect("dispose");
    assert!(context.get(FS).is_none());
}

#[tokio::test]
async fn pre_aborted_operations_return_fs_aborted_without_mutation() {
    let root = tempfile::tempdir().expect("root");
    let fs = filesystem(root.path(), None);
    let file = root.path().join("file.txt");
    std::fs::write(&file, "content").expect("file");
    let target = fs.resolve("file.txt", None, None).await.unwrap();
    let directory = fs.resolve(".", None, None).await.unwrap();
    let version = fs.stat(&target, None).await.unwrap().unwrap().version;
    let signal = AbortSignal::default();
    signal.abort();

    assert_eq!(
        code(
            &fs.resolve("file.txt", None, Some(&signal))
                .await
                .unwrap_err()
        ),
        FsErrorCode::FsAborted
    );
    assert_eq!(
        code(&fs.stat(&target, Some(&signal)).await.unwrap_err()),
        FsErrorCode::FsAborted
    );
    assert_eq!(
        code(&fs.lstat("file.txt", None, Some(&signal)).await.unwrap_err()),
        FsErrorCode::FsAborted
    );
    assert_eq!(
        code(&fs.read_text(&target, Some(&signal)).await.unwrap_err()),
        FsErrorCode::FsAborted
    );
    assert_eq!(
        code(
            &fs.read_bytes(&target, Some(&signal), 100)
                .await
                .unwrap_err()
        ),
        FsErrorCode::FsAborted
    );
    assert_eq!(
        code(&fs.list_dir(&directory, Some(&signal)).await.unwrap_err()),
        FsErrorCode::FsAborted
    );
    let new_file = fs.resolve("new.txt", None, None).await.unwrap();
    assert_eq!(
        code(
            &fs.write_text(&new_file, "new", None, Some(&signal), None)
                .await
                .unwrap_err()
        ),
        FsErrorCode::FsAborted
    );
    assert!(!root.path().join("new.txt").exists());
    assert_eq!(
        code(
            &fs.edit_text(
                &target,
                &FsEditRequest {
                    old_string: "content".to_owned(),
                    new_string: "changed".to_owned(),
                    replace_all: false,
                },
                Some(&version),
                Some(&signal),
                None,
            )
            .await
            .unwrap_err()
        ),
        FsErrorCode::FsAborted
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "content");
}
