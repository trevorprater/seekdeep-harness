//! Pinned-source and controlled-roster coverage exemption fixtures.

use seekdeep_repository_tools::coverage_exempt::{
    COVERAGE_EXEMPT_ENV, COVERAGE_EXEMPT_HEAVY_SUITES, verify_coverage_exempt,
};

#[test]
fn target_environment_and_source_roster_are_exact() {
    assert_eq!(COVERAGE_EXEMPT_ENV, "SEEKDEEP_COVERAGE_EXEMPT_HEAVY");
    assert_eq!(COVERAGE_EXEMPT_HEAVY_SUITES.len(), 4);
    assert_eq!(
        verify_coverage_exempt(std::path::Path::new("/Users/trevor/ws/deepseek-harness")).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn empty_membership_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        "packages/typert/generator/tests/a.spec.ts",
        "scripts/install-lefthook.spec.ts",
        "scripts/oxlint-contract.spec.ts",
        "scripts/change-scope.spec.ts",
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    assert!(verify_coverage_exempt(root.path()).unwrap().is_empty());
    std::fs::remove_file(root.path().join("scripts/change-scope.spec.ts")).unwrap();
    assert!(
        verify_coverage_exempt(root.path())
            .unwrap()
            .iter()
            .any(|violation| violation.contains("selects no specs"))
    );
}
