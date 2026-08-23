//! Canonical lexical and filesystem-identity containment parity.

use seekdeep_fs_sandbox::is_path_under;

#[tokio::test]
async fn equal_descendant_root_and_case_modes_match_the_source() {
    let base = tempfile::tempdir().expect("base");
    let base = base.path().to_string_lossy().into_owned();
    assert!(is_path_under(&base, &base, true).await.unwrap());
    assert!(
        is_path_under(&format!("{base}/child"), &base, true)
            .await
            .unwrap()
    );
    assert!(is_path_under(&base, "/", true).await.unwrap());
    assert!(
        is_path_under(
            &format!("{}/child", base.to_uppercase()),
            &base.to_lowercase(),
            false
        )
        .await
        .unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn alias_identity_recognizes_a_missing_descendant() {
    use std::os::unix::fs::symlink;

    let base = tempfile::tempdir().expect("base");
    let real = base.path().join("real");
    let alias = base.path().join("alias");
    std::fs::create_dir(&real).expect("real");
    symlink(&real, &alias).expect("alias");
    let path = real
        .canonicalize()
        .expect("canonical")
        .join("missing/file.txt");
    assert!(
        is_path_under(&path.to_string_lossy(), &alias.to_string_lossy(), true)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unrelated_missing_and_non_directory_roots_are_not_contained() {
    let base = tempfile::tempdir().expect("base");
    let allowed = base.path().join("allowed");
    let outside = base.path().join("outside");
    std::fs::create_dir(&allowed).expect("allowed");
    std::fs::create_dir(&outside).expect("outside");
    let target = outside.join("file.txt");
    assert!(
        !is_path_under(&target.to_string_lossy(), &allowed.to_string_lossy(), true)
            .await
            .unwrap()
    );
    assert!(
        !is_path_under(
            &target.to_string_lossy(),
            &base.path().join("missing-root").to_string_lossy(),
            true
        )
        .await
        .unwrap()
    );

    let blocker = base.path().join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");
    assert!(
        !is_path_under(
            &blocker.join("child.txt").to_string_lossy(),
            &allowed.to_string_lossy(),
            true
        )
        .await
        .unwrap()
    );
}
