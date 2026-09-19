//! Pinned-source and controlled-roster coverage exemption fixtures.

use std::collections::BTreeSet;

use seekdeep_repository_tools::{
    coverage_exempt::{
        COVERAGE_EXEMPT_ENV, COVERAGE_EXEMPT_HEAVY_SUITES, INSTRUMENTED_LANE_EXCLUDED_PACKAGES,
        verify_coverage_exempt,
    },
    native_test_gates::{NativeTestCommand, NativeTestGate, native_suites},
};

#[test]
fn target_environment_and_source_roster_are_exact() {
    assert_eq!(COVERAGE_EXEMPT_ENV, "SEEKDEEP_COVERAGE_EXEMPT_HEAVY");
    assert_eq!(COVERAGE_EXEMPT_HEAVY_SUITES.len(), 4);
    assert_eq!(
        verify_coverage_exempt(
            &seekdeep_source_oracle::source_root(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
            )
            .unwrap()
        )
        .unwrap(),
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

#[test]
fn the_instrumented_lane_excludes_exactly_the_packages_hosting_compiled_heavy_suites() {
    let mut hosts = BTreeSet::new();
    for suite in native_suites(NativeTestGate::CoverageExemptHeavy) {
        if let NativeTestCommand::Cargo(arguments) = &suite.command
            && arguments.first() == Some(&"test")
        {
            let package = arguments
                .iter()
                .position(|argument| *argument == "--package")
                .and_then(|index| arguments.get(index + 1))
                .expect("compiled heavy suite names its package");
            hosts.insert(*package);
        }
    }
    assert_eq!(
        hosts.into_iter().collect::<Vec<_>>(),
        INSTRUMENTED_LANE_EXCLUDED_PACKAGES
    );
}
