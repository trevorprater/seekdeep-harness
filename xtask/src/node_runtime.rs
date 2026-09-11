//! Stages the compiled Node code-runtime beside built Host binaries.
//!
//! The Host's code-runtime worker and its Node plugin realm load a compiled Rust boundary
//! (`seekdeep-code-runtime-node`, packaged by `package-code-runtime-node`) from a
//! `code-runtime-node` directory next to the executable. Release closures get it from the
//! release pipeline; a checkout's `target/<profile>` binaries get it from here, so the
//! browser lanes and a locally built Host start without a manual packaging step.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

/// Directory name the Host accepts flat beside its executable.
pub(super) const DIRECTORY: &str = "code-runtime-node";

const WASM_PACKAGE: &str = "seekdeep-code-runtime-node";
const WASM_ARTIFACT: &str = "seekdeep_code_runtime_node.wasm";
const PACKAGER_PACKAGE: &str = "seekdeep-code-runtime-worker-thread";
const PACKAGER: &str = "package-code-runtime-node";

/// Builds the compiled Node runtime and its packager, then publishes the runtime closure as
/// `target/<profile>/code-runtime-node`, replacing a previously staged closure atomically.
pub(super) fn stage(metadata: &super::CargoMetadata, profile: &str) -> anyhow::Result<PathBuf> {
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
