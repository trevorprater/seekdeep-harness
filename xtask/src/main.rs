//! Repository gates that keep the source inventory and Rust parity evidence synchronized.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Synchronize the tracked source-file inventory while preserving evidence.
    Inventory {
        /// Source checkout recorded in `SOURCE_SNAPSHOT`.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Verify that every source surface has explicit parity evidence.
    Parity {
        /// Source checkout recorded in `SOURCE_SNAPSHOT`.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Surfaces enforced: `all` is the final gate, `runtime` defers
        /// localization artifacts (Chinese translations and their metadata).
        #[arg(long, value_enum, default_value_t = Scope::All)]
        scope: Scope,
    },
}

/// Which surfaces the parity gate enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Scope {
    /// Enforce every tracked source surface (the final manifest gate).
    All,
    /// Enforce runtime surfaces only; defer localization artifacts.
    Runtime,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Inventory { source } => inventory(&source),
        Command::Parity { source, scope } => parity(&source, scope),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    source_commit: String,
    source_repository: String,
    source_product: String,
    target_product: String,
    target_binary: String,
    surfaces: Vec<Surface>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Surface {
    source: String,
    kind: SurfaceKind,
    status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Pending,
    Ported,
    Verified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SurfaceKind {
    ProductionCode,
    Test,
    Snapshot,
    Fixture,
    Configuration,
    Documentation,
    Asset,
    RepositoryMetadata,
}

fn inventory(source: &Path) -> anyhow::Result<()> {
    verify_source(source)?;
    let mut manifest = read_manifest()?;
    let previous = std::mem::take(&mut manifest.surfaces)
        .into_iter()
        .map(|surface| (surface.source.clone(), surface))
        .collect::<BTreeMap<_, _>>();
    manifest.surfaces = source_files(source)?
        .into_iter()
        .map(|path| {
            previous.get(&path).cloned().unwrap_or_else(|| Surface {
                kind: classify(&path),
                source: path,
                status: Status::Pending,
                targets: Vec::new(),
                evidence: Vec::new(),
                note: None,
            })
        })
        .collect();
    let output = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write("porting/parity.json", output)?;
    println!(
        "inventoried {} tracked source surfaces",
        manifest.surfaces.len()
    );
    Ok(())
}

fn parity(source: &Path, scope: Scope) -> anyhow::Result<()> {
    verify_source(source)?;
    let manifest = read_manifest()?;
    let expected = source_files(source)?;
    let actual = manifest
        .surfaces
        .iter()
        .map(|surface| surface.source.as_str())
        .collect::<Vec<_>>();
    let expected_refs = expected.iter().map(String::as_str).collect::<Vec<_>>();
    anyhow::ensure!(
        actual == expected_refs,
        "parity inventory is stale; run `cargo xtask inventory`"
    );

    let mut pending = Vec::new();
    let mut ported = Vec::new();
    let mut deferred = 0usize;
    for surface in &manifest.surfaces {
        if scope == Scope::Runtime
            && is_localization(&surface.source)
            && surface.status != Status::Verified
        {
            deferred += 1;
            continue;
        }
        match surface.status {
            Status::Pending => pending.push(surface.source.as_str()),
            Status::Ported => ported.push(surface.source.as_str()),
            Status::Verified => {
                anyhow::ensure!(
                    !surface.targets.is_empty(),
                    "verified surface has no Rust target: {}",
                    surface.source
                );
                anyhow::ensure!(
                    !surface.evidence.is_empty(),
                    "verified surface has no evidence: {}",
                    surface.source
                );
                for target in &surface.targets {
                    anyhow::ensure!(
                        Path::new(target).exists(),
                        "parity target does not exist: {target}"
                    );
                }
            }
        }
    }

    verify_rust_only()?;
    let scope_label = match scope {
        Scope::All => "all",
        Scope::Runtime => "runtime",
    };
    if !pending.is_empty() || !ported.is_empty() {
        let sample = pending
            .iter()
            .chain(&ported)
            .take(12)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "{scope_label} parity incomplete: {} pending, {} ported but unverified (deferred: {deferred}) (sample: {sample})",
            pending.len(),
            ported.len()
        );
    }
    println!(
        "verified {} {scope_label} source surfaces at 100% parity (deferred: {deferred})",
        manifest.surfaces.len() - deferred
    );
    Ok(())
}

/// Whether a surface is a localization artifact (Chinese translation or its
/// translation metadata), deferred from the runtime parity gate.
fn is_localization(source: &str) -> bool {
    if source.contains(".zh.") {
        return true;
    }
    let base = source.rsplit('/').next().unwrap_or(source);
    base.contains(".i18n.")
}

fn read_manifest() -> anyhow::Result<Manifest> {
    Ok(serde_json::from_slice(&std::fs::read(
        "porting/parity.json",
    )?)?)
}

fn verify_source(source: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.is_dir(),
        "source checkout does not exist: {}",
        source.display()
    );
    let manifest = read_manifest()?;
    let head = git_output(source, &["rev-parse", "HEAD"])?;
    anyhow::ensure!(
        head.trim() == manifest.source_commit,
        "source HEAD drifted: expected {}, got {}",
        manifest.source_commit,
        head.trim()
    );
    Ok(())
}

fn source_files(source: &Path) -> anyhow::Result<Vec<String>> {
    let output = git_output(source, &["ls-files"])?;
    Ok(output.lines().map(str::to_owned).collect())
}

fn git_output(source: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(source)
        .output()?;
    anyhow::ensure!(output.status.success(), "git {} failed", args.join(" "));
    Ok(String::from_utf8(output.stdout)?)
}

fn classify(path: &str) -> SurfaceKind {
    let lower = path.to_ascii_lowercase();
    if lower.contains("snapshot")
        || Path::new(&lower)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("snap"))
    {
        return SurfaceKind::Snapshot;
    }
    if lower.contains("/fixtures/") || lower.contains("/fixture/") {
        return SurfaceKind::Fixture;
    }
    if lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".spec.")
        || lower.contains(".test.")
        || lower.contains(".e2e.")
    {
        return SurfaceKind::Test;
    }
    let extension = Path::new(path).extension().and_then(|value| value.to_str());
    match extension {
        Some("ts" | "tsx" | "js" | "mjs" | "cjs" | "py" | "c" | "cpp" | "sh") => {
            SurfaceKind::ProductionCode
        }
        Some("md" | "txt") => SurfaceKind::Documentation,
        Some("json" | "jsonl" | "yaml" | "yml" | "toml" | "ini" | "lock") => {
            SurfaceKind::Configuration
        }
        Some("png" | "svg" | "woff" | "woff2" | "ttf" | "html" | "css" | "webmanifest") => {
            SurfaceKind::Asset
        }
        _ => SurfaceKind::RepositoryMetadata,
    }
}

fn verify_rust_only() -> anyhow::Result<()> {
    let forbidden = ["ts", "tsx", "js", "mjs", "cjs", "py", "c", "cpp"];
    let mut violations = Vec::new();
    for entry in walkdir::WalkDir::new(".") {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|part| matches!(part.as_os_str().to_str(), Some(".git" | "target")))
        {
            continue;
        }
        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| forbidden.contains(&extension))
        {
            violations.push(path.display().to_string());
        }
    }
    anyhow::ensure!(
        violations.is_empty(),
        "non-Rust implementation files present: {}",
        violations.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_localization;

    #[test]
    fn localization_predicate_defers_only_chinese_and_translation_metadata() {
        assert!(is_localization(".agents/notes/README.zh.md"));
        assert!(is_localization("packages/core/README.zh.md"));
        assert!(is_localization(".agents/notes/README.i18n.yaml"));
        assert!(is_localization("packages/core/README.i18n.yaml"));
        assert!(!is_localization(".agents/notes/README.md"));
        assert!(!is_localization("packages/core/src/session.rs"));
        assert!(!is_localization("packages/core/package.json"));
        assert!(!is_localization("packages/core/tests/session.spec.ts"));
    }
}
