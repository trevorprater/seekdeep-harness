//! Source-oracle coverage for Loader configuration discovery exclusions.

use seekdeep_repository_tools::cordis_config_files::cordis_config_files;

#[test]
fn finds_loader_yaml_without_treating_translation_records_as_configs() {
    let root = tempfile::tempdir().unwrap();
    for directory in [
        ".claude",
        ".hidden",
        "docs",
        "directory.cordis.yml",
        "examples",
        "nested/vendor",
        "node_modules/pkg",
        "packages/pkg/node_modules",
        "real",
        "vendor/pkg",
    ] {
        std::fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    for file in [
        ".claude/hidden.cordis.yml",
        ".hidden/hidden.cordis.yml",
        "docs/cordis-primer.i18n.yaml",
        "examples/agent.cordis.yaml",
        "examples/headless.cordis.yml",
        "nested/vendor/nested.cordis.yml",
        "node_modules/pkg/hidden.cordis.yml",
        "packages/pkg/node_modules/nested.cordis.yml",
        "real/target.cordis.yml",
        "UPPER.CORDIS.YML",
        "vendor/pkg/hidden.cordis.yml",
    ] {
        std::fs::write(root.path().join(file), "[]\n").unwrap();
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.path().join("real/target.cordis.yml"),
        root.path().join("linked.cordis.yml"),
    )
    .unwrap();

    let mut expected = vec![
        "directory.cordis.yml",
        "examples/agent.cordis.yaml",
        "examples/headless.cordis.yml",
    ];
    #[cfg(unix)]
    expected.push("linked.cordis.yml");
    expected.extend([
        "nested/vendor/nested.cordis.yml",
        "packages/pkg/node_modules/nested.cordis.yml",
        "real/target.cordis.yml",
    ]);
    if cfg!(any(target_os = "macos", windows)) {
        expected.insert(0, "UPPER.CORDIS.YML");
    }
    assert_eq!(cordis_config_files(root.path()).unwrap(), expected);
}
