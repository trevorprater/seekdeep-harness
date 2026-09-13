//! Native-only static-musl builds of the compiled Rust Landlock launcher.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};

use crate::{
    process::{CommandSpec, Runner},
    repo::{Prebuilds, Repository, host_arch, host_os, read_json, verify_platform_binaries},
};

/// Host identity supplied at the native build boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildHost {
    /// Node-compatible operating-system name.
    pub os: String,
    /// Node-compatible architecture name.
    pub arch: String,
}

impl Default for BuildHost {
    fn default() -> Self {
        Self {
            os: host_os().to_owned(),
            arch: host_arch().to_owned(),
        }
    }
}

/// One output declared for this native host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildTarget {
    /// Absolute platform-package directory.
    pub package_dir: PathBuf,
    /// Launcher tool name.
    pub tool: String,
    /// Output path relative to the platform package.
    pub binary_path: String,
    /// Static linking/toolchain contract.
    pub kind: String,
}

/// Discover this host's ordered native build targets.
///
/// # Errors
///
/// Returns for non-Linux hosts, unreadable metadata, and hosts with no declared binaries.
pub fn build_targets(repo: &Repository, host: &BuildHost) -> Result<Vec<BuildTarget>> {
    if host.os != "linux" {
        bail!(
            "build: native tools are built natively per Linux architecture (no cross toolchain) — nothing to build on {}. CI's per-arch runners build and rehearse every platform package.",
            host.os
        );
    }
    let host_platform = format!("linux-{}", host.arch);
    let mut targets = Vec::new();
    for directory in repo.platform_dirs()? {
        let package_dir = repo.package_path(&directory);
        let prebuilds: Prebuilds =
            serde_json::from_value(read_json(&package_dir.join("prebuilds.json"))?)?;
        if prebuilds.platform == host_platform {
            targets.extend(prebuilds.binaries.into_iter().map(|binary| BuildTarget {
                package_dir: package_dir.clone(),
                tool: binary.tool,
                binary_path: binary.path,
                kind: binary.kind,
            }));
        }
    }
    if targets.is_empty() {
        bail!(
            "build: no platform package declares binaries for {host_platform} — supported platforms are the packages/*/prebuilds.json \"platform\" values."
        );
    }
    Ok(targets)
}

/// Build every matching native output from the Rust launcher and verify its architecture.
///
/// `host` is an injected build/test input; the CLI always supplies the actual host.
/// Each Linux runner supplies its native `musl-gcc` and matching Rust musl target.
///
/// # Errors
///
/// Returns metadata, unsupported tool/kind, build, copy, or binary verification failures.
pub fn build_native(
    repo: &Repository,
    host: &BuildHost,
    runner: &mut dyn Runner,
) -> Result<Vec<PathBuf>> {
    let targets = build_targets(repo, host)?;
    let rust_target = match host.arch.as_str() {
        "x64" => "x86_64-unknown-linux-musl",
        "arm64" => "aarch64-unknown-linux-musl",
        _ => bail!("build: no native Rust musl target for {}", host.arch),
    };
    let target_dir = repo.root.join(".release/rust-target");
    let mut outputs = Vec::new();
    let mut packages = BTreeSet::new();
    for target in targets {
        if target.tool != "landlock-run" {
            bail!(
                "build: prebuilds.json names unknown tool \"{}\" — add it to the TOOLS table in scripts/build.ts.",
                target.tool
            );
        }
        if target.kind != "static-musl" {
            bail!(
                "build: unknown binary kind \"{}\" — the only toolchain here is static musl.",
                target.kind
            );
        }
        let binary = target.package_dir.join(&target.binary_path);
        let parent = binary
            .parent()
            .ok_or_else(|| anyhow::anyhow!("build: binary path has no parent"))?;
        fs::create_dir_all(parent)?;
        let command = native_command(repo, rust_target, &target_dir);
        match runner.run(&command) {
            Ok(output) if output.status == Some(0) => {}
            Ok(_) => bail!(
                "build: Rust static-musl build failed — install the {rust_target} Rust target and native musl-tools."
            ),
            Err(error) => bail!(
                "build: Rust static-musl build failed ({error} — are Cargo, the {rust_target} Rust target, and native musl-tools installed?)"
            ),
        }
        fs::copy(
            target_dir.join(rust_target).join("release/landlock-run"),
            &binary,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        }
        let package_name = target
            .package_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        runner.log(&format!(
            "build: built {package_name}/{}",
            target.binary_path
        ));
        packages.insert(target.package_dir);
        outputs.push(binary);
    }
    for package in packages {
        verify_platform_binaries(&package)?;
    }
    Ok(outputs)
}

fn native_command(repo: &Repository, rust_target: &str, target_dir: &Path) -> CommandSpec {
    CommandSpec {
        program: "cargo".to_owned(),
        args: vec![
            "rustc".to_owned(),
            "--release".to_owned(),
            "--package".to_owned(),
            "seekdeep-landlock-run".to_owned(),
            "--bin".to_owned(),
            "landlock-run".to_owned(),
            "--target".to_owned(),
            rust_target.to_owned(),
            "--target-dir".to_owned(),
            target_dir.to_string_lossy().into_owned(),
            "--".to_owned(),
            "-C".to_owned(),
            "target-feature=+crt-static".to_owned(),
            "-C".to_owned(),
            "strip=symbols".to_owned(),
        ],
        cwd: repo.root.join("../.."),
        env: BTreeMap::from([
            ("CARGO_INCREMENTAL".to_owned(), "0".to_owned()),
            (
                format!(
                    "CARGO_TARGET_{}_LINKER",
                    rust_target.replace('-', "_").to_ascii_uppercase()
                ),
                "musl-gcc".to_owned(),
            ),
        ]),
        capture: false,
        max_buffer: 1_048_576,
        inherit_stdin: false,
    }
}
