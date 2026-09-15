//! Repository glob ordering, symlink deduplication, and line-reference coverage.

use regex::Regex;
use seekdeep_repository_tools::repo_files::{find_reference_violations, unique_repo_files};

fn write(root: &std::path::Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn patterns_are_ordered_hidden_paths_are_skipped_and_symlinks_are_deduplicated() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "packages/core/a/src/index.ts", "export {}\n");
    write(root.path(), "packages/core/a/lib/built.ts", "built\n");
    write(root.path(), ".hidden/hidden.ts", "hidden\n");
    write(root.path(), "examples/fixture.ts", "fixture\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.path().join("packages/core/a/src/index.ts"),
        root.path().join("examples/alias.ts"),
    )
    .unwrap();

    let files = unique_repo_files(
        root.path(),
        &["examples/**/*.ts", "packages/**/*.ts"],
        |path| path.contains("/lib/"),
    )
    .unwrap();
    let authored = files
        .iter()
        .map(|file| {
            file.absolute
                .strip_prefix(root.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    #[cfg(unix)]
    assert_eq!(authored, ["examples/alias.ts", "examples/fixture.ts"]);
    #[cfg(not(unix))]
    assert_eq!(
        authored,
        ["examples/fixture.ts", "packages/core/a/src/index.ts"]
    );
}

#[test]
fn reference_scan_reports_each_rejected_match_with_one_based_line() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "docs/present.md", "# Present\n");
    write(
        root.path(),
        "packages/core/a/src/index.ts",
        concat!(
            "// docs/present.md\n",
            "// docs/missing.md and .agents/notes/missing.md\n",
        ),
    );
    let file = root.path().join("packages/core/a/src/index.ts");
    let pattern = Regex::new(r"(?:\bdocs|\.agents/notes)/[A-Za-z0-9._/-]+\.md").unwrap();
    let violations =
        find_reference_violations(root.path(), &file, &pattern, str::to_owned, |reference| {
            !root.path().join(reference).exists()
        })
        .unwrap();
    assert_eq!(violations.len(), 2);
    assert_eq!(violations[0].line, 2);
    assert_eq!(violations[0].reference, "docs/missing.md");
    assert_eq!(violations[1].reference, ".agents/notes/missing.md");
}
