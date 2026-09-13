//! Shipped configuration credential and endpoint source-ownership policy.

use std::{path::Path, sync::LazyLock};

use regex::Regex;

use crate::repo_files::unique_repo_files;

const SHIPPED_CONFIG_PATTERNS: &[&str] = &[
    "apps/*/config/*.yml",
    "examples/*/*.cordis.yml",
    "examples/*/cordis.yml",
    "packages/bundle/*/cordis.patch.yml",
    "python/*/src/**/cordis.yml",
];

static INLINE_DENY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(apiKey|baseURL|apiKeyEnv|authToken|headers)\s*:\s*!!js\b")
        .expect("valid regex")
});

/// Returns every forbidden ordinary inline environment form in shipped config.
///
/// # Errors
///
/// Returns repository glob, canonicalization, or configuration read failures.
pub fn collect_config_source_ownership_violations(
    repository_root: &Path,
) -> anyhow::Result<Vec<String>> {
    let files = unique_repo_files(repository_root, SHIPPED_CONFIG_PATTERNS, |_| false)?;
    let mut failures = Vec::new();
    for file in files {
        let relative = file
            .absolute
            .strip_prefix(repository_root)?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(file.absolute)?;
        for (index, line) in String::from_utf8_lossy(&bytes).split('\n').enumerate() {
            if INLINE_DENY.is_match(line) {
                failures.push(format!(
                    "{relative}:{}: inlines a credential or endpoint from the environment. The adapter resolves apiKeyEnv through ctx.credentials and the endpoint through the environment snapshot; inlining here bypasses both ladders.",
                    index + 1
                ));
            }
        }
    }
    Ok(failures)
}
