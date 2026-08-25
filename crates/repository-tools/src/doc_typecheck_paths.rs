//! Mapping from workspace source aliases to declaration-build targets.

/// Maps one workspace source alias target to its declaration-build target.
///
/// # Errors
///
/// Rejects aliases that do not select a supported package source directory,
/// subpath wildcard, exact TypeScript entry, or source subdirectory.
pub fn built_declaration_path(candidate: &str) -> anyhow::Result<String> {
    if let Some(package) = candidate.strip_suffix("/src") {
        return Ok(format!("{package}/lib/types"));
    }
    if let Some(package) = candidate.strip_suffix("/src/*") {
        return Ok(format!("{package}/lib/types/*"));
    }
    if let Some((package, source)) = candidate.rsplit_once("/src/")
        && !package.is_empty()
        && let Some(entry) = source.strip_suffix(".ts")
        && !entry.is_empty()
    {
        return Ok(format!("{package}/lib/types/{entry}.d.ts"));
    }
    if let Some((package, directory)) = candidate.rsplit_once("/src/")
        && !package.is_empty()
        && !directory.is_empty()
    {
        return Ok(format!("{package}/lib/types/{directory}"));
    }
    anyhow::bail!(
        "doc-typecheck: cannot map workspace source path to built declarations: {candidate}"
    )
}
