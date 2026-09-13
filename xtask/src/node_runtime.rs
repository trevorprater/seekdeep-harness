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

/// Copies the ripgrep found on this machine's PATH to `target/<profile>/rg`, the location a
/// release Host requires (a debug Host falls back to PATH itself).
fn stage_ripgrep(metadata: &super::CargoMetadata, profile: &str) -> anyhow::Result<PathBuf> {
    let name = if cfg!(windows) { "rg.exe" } else { RIPGREP };
    let source = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ripgrep is not installed on PATH; the Host's glob and grep tools need it beside the executable (for example `brew install ripgrep`)"
            )
        })?;
    let source = source.canonicalize()?;
    let directory = metadata.target_directory.join(profile);
    std::fs::create_dir_all(&directory)?;
    let output = directory.join(name);
    let staging = directory.join(format!(".{name}.staging-{}", std::process::id()));
    copy_executable(&source, &staging)?;
    std::fs::rename(&staging, &output)?;
    Ok(output)
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
