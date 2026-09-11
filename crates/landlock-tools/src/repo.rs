//! Package discovery and prepack validation from the checked-in native matrix.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Native package repository, rooted at `native/landlock-run`.
#[derive(Clone, Debug)]
pub struct Repository {
    /// Directory containing the family manifest and `packages/`.
    pub root: PathBuf,
}

impl Repository {
    /// Select a package repository without inspecting or changing it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Platform package paths relative to the repository root, sorted by name.
    ///
    /// # Errors
    ///
    /// Returns when `packages/` cannot be listed.
    pub fn platform_dirs(&self) -> Result<Vec<PathBuf>> {
        self.discover(true)
    }

    /// Entry package paths relative to the repository root, sorted by name.
    ///
    /// # Errors
    ///
    /// Returns when `packages/` cannot be listed.
    pub fn entry_dirs(&self) -> Result<Vec<PathBuf>> {
        self.discover(false)
    }

    /// Published package paths in platform-before-entry publication order.
    ///
    /// # Errors
    ///
    /// Returns when `packages/` cannot be listed.
    pub fn package_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut directories = self.platform_dirs()?;
        directories.extend(self.entry_dirs()?);
        Ok(directories)
    }

    /// Resolve a package path against the native repository root.
    pub fn package_path(&self, directory: &Path) -> PathBuf {
        self.root.join(directory)
    }

    fn discover(&self, platforms: bool) -> Result<Vec<PathBuf>> {
        let mut directories = fs::read_dir(self.root.join("packages"))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        directories.sort_by(|left, right| {
            left.to_string_lossy()
                .encode_utf16()
                .cmp(right.to_string_lossy().encode_utf16())
        });
        Ok(directories
            .into_iter()
            .filter(|name| {
                let directory = self.root.join("packages").join(name);
                let is_platform = directory.join("prebuilds.json").exists();
                is_platform == platforms && (is_platform || directory.join("package.json").exists())
            })
            .map(|name| Path::new("packages").join(name))
            .collect())
    }
}

/// The native package family belonging to this build of the tooling.
pub fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../native/landlock-run")
        .components()
        .fold(PathBuf::new(), |mut path, component| {
            if matches!(component, std::path::Component::ParentDir) {
                path.pop();
            } else {
                path.push(component);
            }
            path
        })
}

/// Node-compatible platform name used by the checked-in native matrix.
pub fn host_platform() -> String {
    format!("{}-{}", host_os(), host_arch())
}

/// Node-compatible operating-system name.
pub fn host_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node-compatible architecture name.
pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// Read a UTF-8 JSON file.
///
/// # Errors
///
/// Returns filesystem and JSON parsing failures.
pub fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("invalid JSON in {}", path.display()))
}

/// One platform package's declared binary inventory.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Prebuilds {
    /// Node-compatible OS and architecture identifier.
    pub platform: String,
    /// Ordered native outputs.
    pub binaries: Vec<PrebuildBinary>,
}

/// One binary allowed in a platform package.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PrebuildBinary {
    /// Launcher tool name.
    pub tool: String,
    /// Build/linking contract.
    pub kind: String,
    /// Output path relative to the package directory.
    pub path: String,
}

/// Successful platform-package verification summary.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PlatformVerification {
    /// Published package name.
    pub name: String,
    /// Number of declared binaries checked.
    pub count: usize,
}

/// Check the package's declared binaries, executable access, architecture, and extra files.
///
/// # Errors
///
/// Returns on the first missing, non-executable, wrong-architecture, or undeclared binary.
pub fn verify_platform_binaries(package_dir: &Path) -> Result<PlatformVerification> {
    let manifest = read_json(&package_dir.join("package.json"))?;
    let prebuilds: Prebuilds =
        serde_json::from_value(read_json(&package_dir.join("prebuilds.json"))?)?;
    let name = manifest
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    let cpu = manifest
        .get("cpu")
        .and_then(|cpu| cpu.get(0))
        .and_then(Value::as_str);
    let expected_machine: u16 = match cpu {
        Some("x64") => 62,
        Some("arm64") => 183,
        _ => bail!(
            "{name}: unsupported or missing \"cpu\" in package.json (expected one of: x64, arm64)"
        ),
    };
    for binary in &prebuilds.binaries {
        let file = package_dir.join(&binary.path);
        if !file.exists() {
            bail!(
                "{name}: missing {} — run `pnpm build:native` on a {} host (or assemble release artifacts) before packing.",
                binary.path,
                prebuilds.platform
            );
        }
        if !is_executable(&file) {
            bail!(
                "{name}: {} is not executable — a pack/extract step stripped the mode bit.",
                binary.path
            );
        }
        let bytes = fs::read(&file)?;
        let Some(machine) = bytes.get(18..20) else {
            if bytes.len() < 2 {
                bail!("Attempt to access memory outside buffer bounds");
            }
            bail!(
                "The value of \"offset\" is out of range. It must be >= 0 and <= {}. Received 18",
                bytes.len() - 2
            );
        };
        let machine = u16::from_le_bytes([machine[0], machine[1]]);
        if machine != expected_machine {
            bail!(
                "{name}: {} has ELF e_machine {machine}, expected {expected_machine} for {} — the binary was built for a different architecture.",
                binary.path,
                cpu.unwrap_or_default()
            );
        }
    }
    let declared = prebuilds
        .binaries
        .iter()
        .filter_map(|binary| Path::new(&binary.path).file_name())
        .collect::<Vec<_>>();
    let bin_dir = package_dir.join("bin");
    let mut extra = if bin_dir.exists() {
        fs::read_dir(bin_dir)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    extra.retain(|name| !declared.contains(&name.as_os_str()));
    extra.sort_by(|left, right| {
        left.to_string_lossy()
            .encode_utf16()
            .cmp(right.to_string_lossy().encode_utf16())
    });
    if !extra.is_empty() {
        bail!(
            "{name}: bin/ contains files not declared in prebuilds.json: {}",
            extra
                .iter()
                .map(|name| name.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(PlatformVerification {
        name: name.to_owned(),
        count: prebuilds.binaries.len(),
    })
}

/// Refuse an entry package whose compiled compatibility exports are absent.
///
/// # Errors
///
/// Returns when package metadata cannot be read or either compiled entry file is missing.
pub fn verify_entry_lib(package_dir: &Path) -> Result<String> {
    let manifest = read_json(&package_dir.join("package.json"))?;
    let name = manifest
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    for file in ["lib/index.js", "lib/index.d.ts"] {
        if !package_dir.join(file).exists() {
            bail!("verify-entry-lib: {name} has no {file} — run `pnpm build:ts` before packing.");
        }
    }
    Ok(format!("verify-entry-lib: {name} built lib/ present."))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}
