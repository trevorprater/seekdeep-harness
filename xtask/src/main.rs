//! Repository gates that keep the source inventory and Rust parity evidence synchronized.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use base64::Engine as _;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify every exported Rust API has documentation and all prose-adjacent lints pass.
    Docs,
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
    /// Builds one Rust/WASM Client package as a synchronous classic module-table bundle.
    WasmPackage {
        /// Cargo package containing the browser cdylib.
        #[arg(long, default_value = "seekdeep-client-runtime")]
        package: String,
        /// Cargo artifact stem (`-` becomes `_`).
        #[arg(long, default_value = "seekdeep_client_runtime")]
        artifact: String,
        /// Client module-table identity.
        #[arg(long, default_value = "@seekdeep-ai/seekdeep-client-runtime")]
        module_id: String,
        /// Package output directory.
        #[arg(long, default_value = "packages/client/runtime/lib")]
        out_dir: PathBuf,
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
        Command::Docs => docs(),
        Command::Inventory { source } => inventory(&source),
        Command::Parity { source, scope } => parity(&source, scope),
        Command::WasmPackage {
            package,
            artifact,
            module_id,
            out_dir,
        } => wasm_package(&package, &artifact, &module_id, &out_dir),
    }
}

fn wasm_package(
    package: &str,
    artifact: &str,
    module_id: &str,
    out_dir: &Path,
) -> anyhow::Result<()> {
    let status = ProcessCommand::new("cargo")
        .args([
            "build",
            "-p",
            package,
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "Rust/WASM release build failed for {package}"
    );
    let wasm =
        PathBuf::from("target/wasm32-unknown-unknown/release").join(format!("{artifact}.wasm"));
    anyhow::ensure!(
        wasm.is_file(),
        "Rust/WASM artifact is missing: {}",
        wasm.display()
    );
    let staging = PathBuf::from("target/xtask/wasm-package").join(artifact);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let global = format!("__seekdeep_{}_wasm", artifact.replace('-', "_"));
    let status = ProcessCommand::new("wasm-bindgen")
        .args([
            "--target",
            "no-modules",
            "--out-name",
            "client",
            "--no-modules-global",
            &global,
            "--out-dir",
        ])
        .arg(&staging)
        .arg(&wasm)
        .status()?;
    anyhow::ensure!(status.success(), "wasm-bindgen failed for {package}");
    let bindings = std::fs::read_to_string(staging.join("client.js"))?;
    let bytes = std::fs::read(staging.join("client_bg.wasm"))?;
    let bundle = classic_module_bundle(&bindings, &bytes, &global, module_id)?;
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join("client.js"), bundle)?;
    let type_dir = out_dir.join("types/client");
    std::fs::create_dir_all(&type_dir)?;
    std::fs::copy(staging.join("client.d.ts"), type_dir.join("index.d.ts"))?;
    println!(
        "built {module_id} Rust/WASM classic bundle at {}",
        out_dir.join("client.js").display()
    );
    Ok(())
}

fn classic_module_bundle(
    bindings: &str,
    wasm: &[u8],
    global: &str,
    module_id: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_javascript_identifier(global),
        "WASM global must be a JavaScript identifier"
    );
    let unique_declaration = format!("var {global} =");
    let bindings = if bindings.contains("let wasm_bindgen =") {
        bindings.replacen("let wasm_bindgen =", &unique_declaration, 1)
    } else {
        anyhow::ensure!(
            bindings.contains(&unique_declaration),
            "wasm-bindgen output omitted its expected global declaration"
        );
        bindings.to_owned()
    };
    let module_id = serde_json::to_string(module_id)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(wasm);
    Ok(format!(
        "{bindings}\n(() => {{\n  const binary = atob({encoded:?});\n  const bytes = Uint8Array.from(binary, value => value.charCodeAt(0));\n  {global}.initSync({{ module: bytes }});\n  window.__ModuleLoader__.load({{ id: {module_id}, factory: () => {global} }});\n}})();\n"
    ))
}

fn is_javascript_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|character| {
        character == '_' || character == '$' || character.is_ascii_alphabetic()
    }) && chars
        .all(|character| character == '_' || character == '$' || character.is_ascii_alphanumeric())
}

fn docs() -> anyhow::Result<()> {
    let status = ProcessCommand::new("cargo")
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "workspace exported-API documentation gate failed"
    );
    println!("verified exported Rust API documentation and strict prose-adjacent lints");
    Ok(())
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
            || is_generated_package_output(path)
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

fn is_generated_package_output(path: &Path) -> bool {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .filter(|component| *component != ".")
        .collect::<Vec<_>>();
    parts.len() >= 4 && parts[0] == "packages" && parts[3] == "lib"
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{classic_module_bundle, is_generated_package_output, is_localization};

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

    #[test]
    fn classic_bundle_embeds_bytes_initializes_sync_and_registers_exact_module() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1, 2, 3],
            "__seekdeep_probe_wasm",
            "@seekdeep-ai/probe",
        )
        .unwrap();
        assert!(bundle.contains("AQID"));
        assert!(bundle.starts_with("var __seekdeep_probe_wasm = {};"));
        assert!(!bundle.contains("let wasm_bindgen ="));
        assert!(bundle.contains("__seekdeep_probe_wasm.initSync({ module: bytes })"));
        assert!(bundle.contains("id: \"@seekdeep-ai/probe\""));
        assert!(bundle.contains("factory: () => __seekdeep_probe_wasm"));
    }

    #[test]
    fn classic_bundle_rejects_an_unsafe_global_identifier() {
        let error = classic_module_bundle("", &[], "not-valid", "probe").unwrap_err();
        assert!(error.to_string().contains("JavaScript identifier"));
    }

    #[test]
    fn rust_only_gate_skips_only_package_lib_derivatives() {
        assert!(is_generated_package_output(Path::new(
            "packages/client/runtime/lib/client.js"
        )));
        assert!(!is_generated_package_output(Path::new(
            "packages/client/runtime/src/client.js"
        )));
        assert!(!is_generated_package_output(Path::new(
            "packages/client/lib/client.js"
        )));
    }
}
