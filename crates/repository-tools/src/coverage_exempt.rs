//! Coverage-exempt heavy-suite roster and mechanical selection verification.

use std::{collections::HashMap, path::Path};

/// Target-side environment flag for the instrumented coverage lane.
pub const COVERAGE_EXEMPT_ENV: &str = "SEEKDEEP_COVERAGE_EXEMPT_HEAVY";

/// One heavy suite's positional filter and instrumented-lane exclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoverageExemptSuite {
    /// Prefix selecting the suite in the uninstrumented lane.
    pub filter: &'static str,
    /// Exact file or directory glob removed from the instrumented lane.
    pub exclude: &'static str,
}

/// Source-compatible heavy suite roster.
pub const COVERAGE_EXEMPT_HEAVY_SUITES: &[CoverageExemptSuite] = &[
    CoverageExemptSuite {
        filter: "packages/typert/generator/tests/",
        exclude: "packages/typert/generator/tests/**",
    },
    CoverageExemptSuite {
        filter: "scripts/install-lefthook.spec.ts",
        exclude: "scripts/install-lefthook.spec.ts",
    },
    CoverageExemptSuite {
        filter: "scripts/oxlint-contract.spec.ts",
        exclude: "scripts/oxlint-contract.spec.ts",
    },
    CoverageExemptSuite {
        filter: "scripts/change-scope.spec.ts",
        exclude: "scripts/change-scope.spec.ts",
    },
];

/// Verifies non-empty filter/exclude equivalence and disjoint roster membership.
///
/// # Errors
///
/// Returns repository traversal failures.
pub fn verify_coverage_exempt(root: &Path) -> anyhow::Result<Vec<String>> {
    let specs = spec_inventory(root)?;
    let mut violations = Vec::new();
    let mut seen = HashMap::<String, &str>::new();
    for suite in COVERAGE_EXEMPT_HEAVY_SUITES {
        let mut from_filter = specs
            .iter()
            .filter(|spec| spec.starts_with(suite.filter))
            .cloned()
            .collect::<Vec<_>>();
        let mut from_exclude = specs
            .iter()
            .filter(|spec| exclude_matches(suite.exclude, spec))
            .cloned()
            .collect::<Vec<_>>();
        from_filter.sort();
        from_exclude.sort();
        if from_filter.is_empty() {
            violations.push(format!(
                "coverage exempt filter {:?} selects no specs",
                suite.filter
            ));
        }
        if from_filter != from_exclude {
            violations.push(format!(
                "coverage exempt filter {:?} and exclude {:?} select different specs",
                suite.filter, suite.exclude
            ));
        }
        for spec in from_exclude {
            if let Some(previous) = seen.insert(spec.clone(), suite.exclude) {
                violations.push(format!(
                    "{spec} matched by {previous} and {}",
                    suite.exclude
                ));
            }
        }
    }
    Ok(violations)
}

fn spec_inventory(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut specs = Vec::new();
    for base in ["packages", "apps", "examples", "scripts"] {
        let directory = root.join(base);
        if !directory.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(directory) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().strip_prefix(root)?;
            let normalized = path
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if is_spec_path(&normalized) {
                specs.push(normalized);
            }
        }
    }
    specs.sort();
    Ok(specs)
}

fn is_spec_path(path: &str) -> bool {
    let parts = path.split('/').collect::<Vec<_>>();
    let extension = path.ends_with(".spec.ts") || path.ends_with(".spec.tsx");
    if !extension {
        return false;
    }
    match parts.as_slice() {
        ["packages", _, _, "tests", ..] | ["apps" | "examples", _, "tests", ..] => true,
        ["scripts", ..] => path.ends_with(".spec.ts"),
        _ => false,
    }
}

fn exclude_matches(exclude: &str, path: &str) -> bool {
    exclude
        .strip_suffix("/**")
        .map_or(path == exclude, |prefix| {
            path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
}
