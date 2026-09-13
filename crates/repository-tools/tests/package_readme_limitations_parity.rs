//! Canonical, whitelist, missing, duplicate, variant, and body fixtures.

use seekdeep_repository_tools::package_readme_limitations::inspect_package_readme_limitations;
use tempfile::TempDir;

fn package(root: &TempDir, path: &str, readme: Option<&str>) {
    let directory = root.path().join(path);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("package.json"), "{}\n").unwrap();
    if let Some(readme) = readme {
        std::fs::write(directory.join("README.md"), readme).unwrap();
    }
}

fn root_with_brand() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    package(&root, "packages/util/brand", Some("# Brand\n"));
    root
}

#[test]
fn canonical_section_and_audited_omission_conform() {
    let root = root_with_brand();
    package(
        &root,
        "packages/core/good",
        Some("# Good\n\n## Known Limitations and Deferred Work\n\n- One real limitation.\n"),
    );
    let report = inspect_package_readme_limitations(root.path()).unwrap();
    assert_eq!(report.checked, 2);
    assert_eq!(report.whitelisted, 1);
    assert!(report.failures.is_empty());
}

#[test]
fn missing_readme_and_missing_section_are_distinct() {
    let root = root_with_brand();
    package(&root, "packages/core/no-readme", None);
    package(&root, "packages/core/no-section", Some("# No section\n"));
    let report = inspect_package_readme_limitations(root.path()).unwrap();
    assert!(report.failures[0].contains("no-readme/README.md: package manifest has no sibling"));
    assert!(report.failures[1].contains("no-section/README.md: missing the `## Known"));
}

#[test]
fn duplicate_and_noncanonical_headings_fail_before_body_policy() {
    let root = root_with_brand();
    package(
        &root,
        "packages/core/duplicate",
        Some("# Duplicate\n\n## Limitations\n\n## Deferred Work\n"),
    );
    package(
        &root,
        "packages/core/variant",
        Some("# Variant\n\n### Known Limitations and Deferred Work\n\n- Detail\n"),
    );
    let failures = inspect_package_readme_limitations(root.path())
        .unwrap()
        .failures;
    assert!(failures[0].contains("2 limitations-like headings (lines 3, 5)"));
    assert!(failures[1].contains("non-canonical heading"));
}

#[test]
fn body_requires_a_top_level_dash_bullet_before_the_next_heading() {
    let root = root_with_brand();
    package(
        &root,
        "packages/core/no-bullet",
        Some(
            "# No bullet\n\n## Known Limitations and Deferred Work\n\n  - Nested only\n\n## Later\n\n- Too late\n",
        ),
    );
    let failures = inspect_package_readme_limitations(root.path())
        .unwrap()
        .failures;
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("has no top-level `- ` bullet"));
}

#[test]
fn whitelisted_package_must_omit_every_limitations_like_heading() {
    let root = tempfile::tempdir().unwrap();
    package(
        &root,
        "packages/util/brand",
        Some("# Brand\n\n## Non-goals\n\n- Runtime behavior.\n"),
    );
    let failures = inspect_package_readme_limitations(root.path())
        .unwrap()
        .failures;
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("whitelisted as having no known limitations"));
}
