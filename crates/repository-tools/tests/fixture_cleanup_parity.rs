//! Missing, regular, nested-link, outside-target, and whole-tree fixtures.

use seekdeep_repository_tools::fixture_cleanup::{remove_fixture_safely, unlink_fixture_links};

#[test]
fn missing_paths_are_accepted() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    unlink_fixture_links(&missing).unwrap();
    remove_fixture_safely(&missing).unwrap();
}

#[test]
fn regular_files_and_directories_survive_the_unlink_only_pass() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("value.txt"), "kept\n").unwrap();
    unlink_fixture_links(root.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(nested.join("value.txt")).unwrap(),
        "kept\n"
    );
}

#[cfg(unix)]
#[test]
fn nested_links_are_unlinked_without_touching_their_targets() {
    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("sentinel.txt"), "outside\n").unwrap();
    let nested = fixture.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::os::unix::fs::symlink(outside.path(), nested.join("linked")).unwrap();

    unlink_fixture_links(fixture.path()).unwrap();
    assert!(!nested.join("linked").exists());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
        "outside\n"
    );
}

#[cfg(unix)]
#[test]
fn safe_removal_deletes_only_the_fixture_tree() {
    let parent = tempfile::tempdir().unwrap();
    let fixture = parent.path().join("fixture");
    let outside = parent.path().join("outside");
    std::fs::create_dir_all(&fixture).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("sentinel.txt"), "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.join("junction")).unwrap();
    std::fs::write(fixture.join("owned.txt"), "owned\n").unwrap();

    remove_fixture_safely(&fixture).unwrap();
    assert!(!fixture.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
        "outside\n"
    );
}

#[test]
fn safe_removal_also_accepts_a_regular_file_root() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fixture.txt");
    std::fs::write(&file, "fixture\n").unwrap();
    remove_fixture_safely(&file).unwrap();
    assert!(!file.exists());
}
