//! The pinned `@openai/codex` package the real-product tests drive.
//!
//! The source resolves `codex` from the package's own `node_modules/.bin`, so the pinned
//! launcher wins over any other Codex on the host's PATH.

use std::path::{Path, PathBuf};

/// The `@openai/codex` version the package manifest pins.
pub(crate) const PINNED_CODEX_VERSION: &str = "0.147.0";
/// What that launcher reports for `codex --version`.
pub(crate) const PINNED_CODEX_VERSION_LINE: &str = "codex-cli 0.147.0";

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/subagent/subagent-codex")
}

/// The package-local `.bin` directory that supplies the pinned launcher.
pub(crate) fn codex_bin_dir() -> PathBuf {
    package_root().join("node_modules/.bin")
}

/// The pinned launcher itself.
pub(crate) fn codex_launcher() -> PathBuf {
    codex_bin_dir().join("codex")
}

/// `PATH` with the pinned launcher ahead of the host's entries, as the source's tests pass it.
pub(crate) fn codex_path() -> String {
    let mut entries = vec![codex_bin_dir()];
    entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(entries)
        .expect("the codex bin directory carries no path separator")
        .to_string_lossy()
        .into_owned()
}

/// The version the installed `@openai/codex` manifest declares.
///
/// # Errors
///
/// Returns the read or parse failure, or a manifest without a version.
pub(crate) fn installed_codex_version() -> anyhow::Result<String> {
    let manifest = package_root().join("node_modules/@openai/codex/package.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
    manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("the @openai/codex manifest declares no version"))
}
