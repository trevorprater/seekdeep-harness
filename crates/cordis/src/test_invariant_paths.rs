//! Package-scoped selection for the automatic test invariant host.

/// Private readiness service required by ordinary test-root plugins.
pub const TEST_INVARIANT_READY_SERVICE: &str = "testInvariantReady";

/// Test suites that construct their own invariant service tree.
#[must_use]
pub fn uses_manual_invariant_tree(test_path: &str) -> bool {
    let normalized = test_path.replace('\\', "/");
    if normalized.ends_with("/packages/runtime-diagnostics/invariants/tests/service.spec.ts")
        || normalized.ends_with("/packages/examples/agent-spine-demo/tests/agent-core.spec.ts")
    {
        return true;
    }
    normalized.match_indices("/packages/").any(|(start, _)| {
        let path = &normalized[start + "/packages/".len()..];
        let parts = path.split('/').collect::<Vec<_>>();
        matches!(parts.as_slice(), [group, package, "tests", file]
            if !group.is_empty() && !package.is_empty() && file.contains("invariant") && file.ends_with(".spec.ts"))
    })
}

/// Selects the owner's lazy companion, or every companion for the topology test.
///
/// # Errors
/// Rejects a package test whose companion is absent from the discovered map.
pub fn test_invariant_companion_paths(
    test_path: &str,
    available: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<String>, String> {
    let normalized = test_path.replace('\\', "/");
    let mut paths = available
        .into_iter()
        .map(Into::into)
        .collect::<Vec<String>>();
    paths.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    if normalized.ends_with("/scripts/test-invariants.spec.ts") {
        return Ok(paths);
    }
    for (start, _) in normalized.match_indices("/packages/") {
        let path = &normalized[start + "/packages/".len()..];
        let mut parts = path.split('/');
        let (Some(group), Some(package), Some("tests"), Some(_)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if group.is_empty() || package.is_empty() {
            continue;
        }
        let companion = format!("../packages/{group}/{package}/src/invariant.ts");
        if paths.contains(&companion) {
            return Ok(vec![companion]);
        }
        return Err(format!(
            "test invariants: package test has no companion at {companion}"
        ));
    }
    Ok(Vec::new())
}
