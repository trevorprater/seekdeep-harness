use std::{path::Path, process::Command};

use anyhow::Context as _;
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

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
    install_site_dependencies_with_runner(root, run)
}

fn install_site_dependencies_with_runner(
    root: &Path,
    install: impl FnOnce(&mut Command) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
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
    let mut digest = Sha1::new();
    for (path, contents) in artifacts {
        digest.update(contents.as_bytes());
        digest.update([0]);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(&contents) {
            std::fs::write(path, contents)?;
        }
    }
    let fingerprint = format!("{:x}\n", digest.finalize());
    let installed = cache.join("installed-fingerprint");
    if std::fs::read_to_string(&installed).ok().as_deref() != Some(&fingerprint)
        || !website.join("node_modules/.bin/vitepress").exists()
    {
        install(
            Command::new(if cfg!(windows) { "pnpm.cmd" } else { "pnpm" })
                .args([
                    "install",
                    "--ignore-workspace",
                    "--ignore-scripts",
                    "--frozen-lockfile",
                    "--modules-dir",
                ])
                .arg(website.join("node_modules"))
                .env("CI", "true")
                .current_dir(&cache),
        )?;
        std::fs::write(installed, fingerprint)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_refresh_executes_the_platform_launcher_from_path() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("documentation with 中文 and spaces");
        std::fs::create_dir_all(root.join("website"))?;
        std::fs::write(
            root.join("package.json"),
            r#"{"packageManager":"pnpm@11.7.0"}"#,
        )?;
        std::fs::write(
            root.join("website/package.json"),
            r#"{"devDependencies":{}}"#,
        )?;
        std::fs::write(
            root.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\nimporters:\n  website: {}\n",
        )?;
        let programs = root.join("programs");
        std::fs::create_dir(&programs)?;
        std::fs::write(
            programs.join("capture.mjs"),
            "import { writeFileSync } from 'node:fs';\nwriteFileSync(process.env.SEEKDEEP_INSTALL_ARGUMENTS, JSON.stringify(process.argv.slice(2)));\n",
        )?;
        let program = programs.join(if cfg!(windows) { "pnpm.cmd" } else { "pnpm" });
        std::fs::write(
            &program,
            if cfg!(windows) {
                "@echo off\r\nnode \"%~dp0capture.mjs\" %*\r\n"
            } else {
                "#!/bin/sh\nexec node \"$(dirname \"$0\")/capture.mjs\" \"$@\"\n"
            },
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
        }
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(programs).chain(std::env::split_paths(&inherited_path)),
        )?;
        let arguments = root.join("arguments.json");
        install_site_dependencies_with_runner(&root, |command| {
            command
                .env("PATH", path)
                .env("SEEKDEEP_INSTALL_ARGUMENTS", &arguments);
            run(command)
        })?;
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(arguments)?)?,
            json!([
                "install",
                "--ignore-workspace",
                "--ignore-scripts",
                "--frozen-lockfile",
                "--modules-dir",
                root.join("website/node_modules")
            ])
        );
        assert!(
            root.join("website/.cache/dependencies/installed-fingerprint")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn failed_dependency_refresh_retries_with_noninteractive_frozen_install() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let binary = root.join("website/node_modules/.bin/vitepress");
        std::fs::create_dir_all(binary.parent().expect("binary directory"))?;
        std::fs::write(&binary, "stale executable")?;
        std::fs::write(
            root.join("package.json"),
            r#"{"packageManager":"pnpm@11.1.1"}"#,
        )?;
        std::fs::write(
            root.join("website/package.json"),
            r#"{"devDependencies":{"vitepress":"1.6.4"}}"#,
        )?;
        std::fs::write(
            root.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\nimporters:\n  website: {}\n",
        )?;
        let installed = root.join("website/.cache/dependencies/installed-fingerprint");
        let failed = install_site_dependencies_with_runner(root, |command| {
            assert!(command.get_envs().any(|(key, value)| {
                key == "CI" && value == Some(std::ffi::OsStr::new("true"))
            }));
            assert!(command.get_args().any(|arg| arg == "--frozen-lockfile"));
            anyhow::bail!("dependency install failed")
        });
        assert!(failed.is_err());
        assert!(!installed.exists());

        let mut retried = false;
        install_site_dependencies_with_runner(root, |_| {
            retried = true;
            Ok(())
        })?;
        assert!(
            retried,
            "a preexisting executable cannot hide a failed refresh"
        );
        assert!(installed.is_file());
        install_site_dependencies_with_runner(root, |_| panic!("unchanged successful install"))?;

        std::fs::remove_file(binary)?;
        let mut repaired = false;
        install_site_dependencies_with_runner(root, |_| {
            repaired = true;
            Ok(())
        })?;
        assert!(
            repaired,
            "a missing executable invalidates the install receipt"
        );
        Ok(())
    }
}
