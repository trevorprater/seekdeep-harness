//! Safe deletion, all-or-nothing planning, Landlock, and symlink fixtures.

use std::path::Path;

use seekdeep_repository_tools::clean::RepositoryCleaner;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn add_project(root: &Path, path: &str, out_dir: &str) {
    write(
        &root.join("tsconfig.json"),
        &format!(r#"{{"files":[],"references":[{{"path":"{path}"}}]}}"#),
    );
    write(
        &root.join(path).join("tsconfig.json"),
        &format!(
            r#"{{"compilerOptions":{{"composite":true,"outDir":"{out_dir}"}},"include":["src"]}}"#
        ),
    );
    write(&root.join(path).join("src/index.ts"), "export {}\n");
}

#[test]
fn live_outputs_and_safe_stale_package_residue_are_removed() {
    let root = tempfile::tempdir().unwrap();
    add_project(root.path(), "products/shell", "lib/types");
    write(&root.path().join("products/shell/lib/types/index.js"), "");
    write(&root.path().join("products/shell/lib/index.js"), "");
    write(&root.path().join(".typecheck/legacy.tsbuildinfo"), "");
    write(&root.path().join("root.tsbuildinfo"), "");
    write(
        &root
            .path()
            .join("packages/removed/ghost/node_modules/.bin/tool"),
        "",
    );
    let cleaner = RepositoryCleaner::new(root.path());
    let planned = cleaner.plan().unwrap();
    assert!(planned.contains(&"products/shell/lib".to_owned()));
    let removed = cleaner.clean().unwrap();
    assert!(removed.contains(&"products/shell/lib".to_owned()));
    assert!(!root.path().join("products/shell/lib").exists());
    assert!(root.path().join("products/shell/src/index.ts").exists());
    assert!(!root.path().join(".typecheck").exists());
    assert!(!root.path().join("root.tsbuildinfo").exists());
    assert!(!root.path().join("packages/removed/ghost").exists());
}

#[test]
fn unknown_orphan_prevents_every_planned_removal() {
    let root = tempfile::tempdir().unwrap();
    add_project(root.path(), "products/shell", "lib/types");
    write(&root.path().join("products/shell/lib/types/index.js"), "");
    write(&root.path().join("packages/removed/ghost/notes.txt"), "");
    let error = RepositoryCleaner::new(root.path()).clean().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("packages/removed/ghost/notes.txt")
    );
    assert!(root.path().join("products/shell/lib").exists());
}

#[test]
fn jsonc_comments_and_trailing_commas_preserve_project_graph_discovery() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join("tsconfig.json"),
        "{\n  // solution graph\n  \"files\": [],\n  \"references\": [{ \"path\": \"products/shell\" }],\n}\n",
    );
    write(
        &root.path().join("products/shell/tsconfig.json"),
        "{\n  /* package output */\n  \"compilerOptions\": { \"outDir\": \"lib/types\", },\n  \"include\": [\"src\"],\n}\n",
    );
    write(
        &root.path().join("products/shell/src/index.ts"),
        "export {}\n",
    );
    write(&root.path().join("products/shell/lib/index.js"), "");
    assert_eq!(
        RepositoryCleaner::new(root.path()).plan().unwrap(),
        ["products/shell/lib"]
    );
}

#[test]
fn native_landlock_output_and_solution_build_info_are_removed() {
    let root = tempfile::tempdir().unwrap();
    let entry = "native/landlock-run/packages/entry";
    add_project(root.path(), entry, "lib");
    write(&root.path().join(entry).join("lib/index.js"), "");
    write(
        &root.path().join("native/landlock-run/tsconfig.tsbuildinfo"),
        "",
    );
    RepositoryCleaner::new(root.path()).clean().unwrap();
    assert!(!root.path().join(entry).join("lib").exists());
    assert!(root.path().join(entry).join("src/index.ts").exists());
    assert!(
        !root
            .path()
            .join("native/landlock-run/tsconfig.tsbuildinfo")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn ancestor_symlink_outside_repository_refuses_all_deletion() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    write(
        &root.path().join("tsconfig.json"),
        r#"{"files":[],"references":[{"path":"./linked"}]}"#,
    );
    write(
        &external.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"composite":true,"outDir":"lib/types"},"include":["src"]}"#,
    );
    write(&external.path().join("src/index.ts"), "export {}\n");
    write(&external.path().join("lib/types/index.js"), "");
    symlink(external.path(), root.path().join("linked")).unwrap();
    let error = RepositoryCleaner::new(root.path()).clean().unwrap_err();
    assert!(error.to_string().contains("outside repository"));
    assert!(external.path().join("lib/types/index.js").exists());
}
