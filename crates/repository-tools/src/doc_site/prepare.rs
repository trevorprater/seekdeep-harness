use std::{path::Path, process::Command};

use anyhow::Context as _;
use serde_json::{Value, json};

use super::{DocsManifest, docs_source_files, project_docs, site_configuration};

/// Environment switch: when set, `prepare_site` projects the documentation itself and
/// records no projector, so a site build on a host that cannot run this executable (the
/// Wine Windows gate drives Windows Node over a Linux checkout) consumes the projection.
const PREPROJECT_ENV: &str = "SEEKDEEP_DOCS_PREPROJECT";

/// Builds the Rust/WASM website callbacks and writes `VitePress` adapter inputs.
///
/// # Errors
/// Returns Cargo, binding generation, asset-copy, source-validation, or metadata failures.
pub fn prepare_site(
    root: &Path,
    manifest: &DocsManifest,
    revision: &str,
    edit_branch: &str,
) -> anyhow::Result<()> {
    let base = std::env::var("DOCS_BASE").unwrap_or_else(|_| "/".to_owned());
    let configuration = site_configuration(root, manifest, &base)?;
    let sources = docs_source_files(root, manifest)?;
    install_site_dependencies(root)?;
    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .env("CARGO_INCREMENTAL", "0")
        .output()?;
    anyhow::ensure!(
        metadata.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    let target = Path::new(
        metadata["target_directory"]
            .as_str()
            .context("Cargo metadata has no target_directory.")?,
    );
    run(Command::new("cargo")
        .args([
            "build",
            "--package",
            "seekdeep-docs-site-runtime",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .current_dir(root)
        .env("CARGO_INCREMENTAL", "0"))?;
    let wasm = target.join("wasm32-unknown-unknown/release/seekdeep_docs_site_runtime.wasm");
    let cache = root.join("website/.cache");
    let public = cache.join("public");
    if public.exists() {
        std::fs::remove_dir_all(&public)?;
    }
    std::fs::create_dir_all(&public)?;
    let assets = root.join("website/public");
    for entry in walkdir::WalkDir::new(&assets) {
        let entry = entry?;
        let destination = public.join(entry.path().strip_prefix(&assets)?);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(destination)?;
        } else if entry.file_type().is_file() {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    let browser = public.join("_seekdeep");
    let native = cache.join("native");
    for (destination, target) in [(&browser, "web"), (&native, "nodejs")] {
        run(Command::new("wasm-bindgen")
            .arg(&wasm)
            .args([
                "--target",
                target,
                "--out-name",
                "docs_site_runtime",
                "--out-dir",
            ])
            .arg(destination))?;
    }
    std::fs::write(native.join("package.json"), "{\"type\":\"commonjs\"}\n")?;
    std::fs::write(
        browser.join("docs-site.mjs"),
        "import init, * as runtime from './docs_site_runtime.js';\nawait init();\nglobalThis.__seekdeepDocsRuntime = runtime;\nexport const sidebarScrollbar = new runtime.SidebarScrollbar();\n",
    )?;
    let projector = if std::env::var_os(PREPROJECT_ENV).is_some_and(|value| !value.is_empty()) {
        project_docs(root, &root.join("website/.generated"), manifest, revision)?;
        Value::Null
    } else {
        json!(std::env::current_exe()?)
    };
    let state = json!({
        "config":configuration, "sources":sources,
        "root":root, "revision":revision, "editBranch":edit_branch,
        "projector":projector, "runtime":native.join("docs_site_runtime.js")
    });
    std::fs::write(
        cache.join("site-config.json"),
        format!("{}\n", serde_json::to_string_pretty(&state)?),
    )?;
    Ok(())
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command
        .status()
        .with_context(|| format!("Could not start {command:?}"))?;
    anyhow::ensure!(status.success(), "{command:?} exited with {status}.");
    Ok(())
}

fn install_site_dependencies(root: &Path) -> anyhow::Result<()> {
    let website = root.join("website");
    let cache = website.join(".cache/dependencies");
    let package: Value = serde_json::from_slice(&std::fs::read(website.join("package.json"))?)?;
    let root_package: Value = serde_json::from_slice(&std::fs::read(root.join("package.json"))?)?;
    let package = json!({
        "name":"seekdeep-docs-build-dependencies", "private":true,
        "packageManager":root_package["packageManager"],
        "devDependencies":package["devDependencies"]
    });
    let mut lock: serde_yml::Value =
        serde_yml::from_slice(&std::fs::read(root.join("pnpm-lock.yaml"))?)?;
    let importer = lock["importers"]["website"].clone();
    anyhow::ensure!(
        !importer.is_null(),
        "pnpm-lock.yaml has no website importer."
    );
    lock["importers"] = serde_yml::from_str("{}")?;
    lock["importers"]["."] = importer;
    let mapping = lock
        .as_mapping_mut()
        .context("pnpm lockfile must be a mapping.")?;
    mapping.remove(serde_yml::Value::String("overrides".to_owned()));
    mapping.remove(serde_yml::Value::String("patchedDependencies".to_owned()));
    std::fs::create_dir_all(&cache)?;
    let artifacts = [
        (
            cache.join("package.json"),
            format!("{}\n", serde_json::to_string_pretty(&package)?),
        ),
        (cache.join("pnpm-lock.yaml"), serde_yml::to_string(&lock)?),
    ];
    let mut changed = false;
    for (path, contents) in artifacts {
        if std::fs::read_to_string(&path).ok().as_deref() != Some(&contents) {
            std::fs::write(path, contents)?;
            changed = true;
        }
    }
    if changed || !website.join("node_modules/.bin/vitepress").exists() {
        run(Command::new("pnpm")
            .args([
                "install",
                "--ignore-workspace",
                "--ignore-scripts",
                "--frozen-lockfile",
                "--modules-dir",
            ])
            .arg(website.join("node_modules"))
            .current_dir(&cache))?;
    }
    Ok(())
}
