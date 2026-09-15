//! Stages the Host's runtime assets beside built Host binaries.
//!
//! The Host's code-runtime worker and its Node plugin realm load a compiled Rust boundary
//! (`seekdeep-code-runtime-node`, packaged by `package-code-runtime-node`) from a
//! `code-runtime-node` directory next to the executable, and the glob/grep tools spawn a
//! ripgrep binary packaged beside it (the source ships `@vscode/ripgrep`). Release closures
//! get both from the release pipeline; a checkout's `target/<profile>` binaries get them
//! from here, so the browser lanes and a locally built Host start without a manual
//! packaging step.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Directory name the Host accepts flat beside its executable.
pub(super) const DIRECTORY: &str = "code-runtime-node";
/// Ripgrep binary name the search tools accept beside the executable.
const RIPGREP: &str = "rg";

const WASM_PACKAGE: &str = "seekdeep-code-runtime-node";
const WASM_ARTIFACT: &str = "seekdeep_code_runtime_node.wasm";
const PACKAGER_PACKAGE: &str = "seekdeep-code-runtime-worker-thread";
const PACKAGER: &str = "package-code-runtime-node";

/// Stages every runtime asset a Host built into `target/<profile>` needs beside it: the
/// compiled Node runtime closure and a ripgrep binary. Returns the profile directory.
pub(super) fn stage(metadata: &super::CargoMetadata, profile: &str) -> anyhow::Result<PathBuf> {
    stage_node_runtime(metadata, profile)?;
    stage_ripgrep(metadata, profile)?;
    Ok(metadata.target_directory.join(profile))
}

/// Copies the workspace's packaged ripgrep, or a PATH fallback, beside the Host executable.
fn stage_ripgrep(metadata: &super::CargoMetadata, profile: &str) -> anyhow::Result<PathBuf> {
    let name = if cfg!(windows) { "rg.exe" } else { RIPGREP };
    let source = ripgrep_source(
        &metadata.workspace_root,
        name,
        std::env::var_os("PATH").as_deref(),
    )?
    .canonicalize()?;
    let directory = metadata.target_directory.join(profile);
    std::fs::create_dir_all(&directory)?;
    let output = directory.join(name);
    let staging = directory.join(format!(".{name}.staging-{}", std::process::id()));
    copy_executable(&source, &staging)?;
    std::fs::rename(&staging, &output)?;
    Ok(output)
}

fn ripgrep_source(
    root: &Path,
    name: &str,
    search_path: Option<&std::ffi::OsStr>,
) -> anyhow::Result<PathBuf> {
    if let Some(packaged) = packaged_ripgrep(root) {
        return Ok(packaged);
    }
    search_path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ripgrep is missing; install workspace dependencies with `pnpm install` or install ripgrep on PATH"
            )
        })
}

fn packaged_ripgrep(root: &Path) -> Option<PathBuf> {
    // The package's public export resolves its platform dependency across npm and pnpm layouts.
    let output = Command::new("node")
        .args([
            "-e",
            "const { createRequire } = require('node:module'); process.stdout.write(createRequire(process.argv[1])('@vscode/ripgrep').rgPath)",
        ])
        .arg(root.join("packages/fs/tool-fs-search/package.json"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?);
    path.is_file().then_some(path)
}

fn copy_executable(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Builds the compiled Node runtime and its packager, then publishes the runtime closure as
/// `target/<profile>/code-runtime-node`, replacing a previously staged closure atomically.
fn stage_node_runtime(metadata: &super::CargoMetadata, profile: &str) -> anyhow::Result<PathBuf> {
    let root = &metadata.workspace_root;
    anyhow::ensure!(
        root.join("support/node-runtime-dependencies/node_modules/chokidar/package.json")
            .is_file(),
        "install the pinned Node runtime dependencies: pnpm --dir support/node-runtime-dependencies install --ignore-workspace --frozen-lockfile"
    );
    for arguments in [
        vec![
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            WASM_PACKAGE,
            "--lib",
        ],
        vec![
            "build",
            "--locked",
            "-p",
            PACKAGER_PACKAGE,
            "--bin",
            PACKAGER,
        ],
    ] {
        let status = Command::new("cargo")
            .current_dir(root)
            .env("CARGO_BUILD_JOBS", "2")
            .env("CARGO_PROFILE_RELEASE_DEBUG", "line-tables-only")
            .env("CARGO_PROFILE_RELEASE_STRIP", "none")
            .args(&arguments)
            .status()?;
        anyhow::ensure!(
            status.success(),
            "Node code-runtime build failed: cargo {}",
            arguments.join(" ")
        );
    }
    let wasm = metadata
        .target_directory
        .join("wasm32-unknown-unknown/release")
        .join(WASM_ARTIFACT);
    let packager = metadata.target_directory.join("debug").join(PACKAGER);
    let output = metadata.target_directory.join(profile).join(DIRECTORY);
    let status = Command::new(&packager)
        .current_dir(root)
        .arg(&wasm)
        .arg(&output)
        .stdout(Stdio::null())
        .status()?;
    anyhow::ensure!(
        status.success(),
        "Node code-runtime packaging failed for {}",
        output.display()
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_ripgrep_needs_no_system_install_and_precedes_the_path_fallback() {
        let root = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "rg.exe" } else { RIPGREP };
        let package = root
            .path()
            .join("packages/fs/tool-fs-search/node_modules/@vscode/ripgrep");
        let packaged = root.path().join("platform-package/bin").join(name);
        let path_directory = root.path().join("bin");
        std::fs::create_dir_all(packaged.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"main":"index.cjs"}"#).unwrap();
        std::fs::write(
            package.join("index.cjs"),
            format!(
                "exports.rgPath = {};\n",
                serde_json::to_string(&packaged).unwrap()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(&path_directory).unwrap();
        let fallback = path_directory.join(name);
        std::fs::write(&packaged, b"packaged ripgrep").unwrap();
        std::fs::write(&fallback, b"PATH ripgrep").unwrap();
        let search_path = std::env::join_paths([path_directory]).unwrap();
        assert_eq!(ripgrep_source(root.path(), name, None).unwrap(), packaged);
        assert_eq!(
            ripgrep_source(root.path(), name, Some(&search_path)).unwrap(),
            packaged
        );
        let metadata = super::super::CargoMetadata {
            packages: Vec::new(),
            target_directory: root.path().join("target"),
            workspace_root: root.path().to_owned(),
        };
        let staged = stage_ripgrep(&metadata, "debug").unwrap();
        assert_eq!(staged, root.path().join("target/debug").join(name));
        assert_eq!(std::fs::read(staged).unwrap(), b"packaged ripgrep");
        std::fs::remove_file(&packaged).unwrap();
        assert_eq!(
            ripgrep_source(root.path(), name, Some(&search_path)).unwrap(),
            fallback
        );
        assert!(
            ripgrep_source(root.path(), name, None)
                .unwrap_err()
                .to_string()
                .contains("pnpm install")
        );
    }
}
