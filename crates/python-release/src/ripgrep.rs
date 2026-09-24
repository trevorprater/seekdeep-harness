//! Packaged ripgrep acquisition and verified release staging.
//!
//! The source's `glob` and `grep` tools spawn the ripgrep binary that `@vscode/ripgrep`
//! ships through its per-platform npm packages. A release closure carries that exact
//! binary beside the executable, in a `ripgrep` directory the search tools accept, with
//! the package provenance recorded in a manifest that every copy re-verifies.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256, Sha512};

use crate::{
    executable::{Arch, Platform, Target, validate_native_artifact},
    node_runtime::{DistributionFetcher, safe_relative},
};

/// Directory name resolved beside native and Python-carried executables.
pub const DIRECTORY: &str = "ripgrep";
/// Provenance manifest emitted beside the staged binary.
pub const MANIFEST: &str = "ripgrep-manifest.json";
/// Executable name the search tools spawn.
pub const EXECUTABLE: &str = "rg";
/// License file copied out of the package.
pub const LICENSE: &str = "LICENSE";
/// `@vscode/ripgrep` platform package version the source lockfile pins.
pub const PACKAGE_VERSION: &str = "1.18.0";

/// Lowercase hex SHA-512 of each pinned platform tarball (the lockfile's `sha512-` integrity).
const PINNED: &[(&str, &str)] = &[
    (
        "darwin-arm64",
        "af792d1d2bdb172710345eac97bb0d0cfa1ca6c23b27e984ce1d48699164634b299b793667db7c84f01e38aefea7497aa7afecc24402d785d65f4d65a30fbde1",
    ),
    (
        "darwin-x64",
        "db96f88166cbd77f1d1ae414db8e1e6c228a734ab4e542c1328152cfda001141cd7aa2bf94e2558834bb0d1bdb0dacf303348475a9f7f7566792cfd1e6691a4d",
    ),
    (
        "linux-arm64",
        "950ff9cd31bef94d04dc8855812e043d34e7fd4e2892771a44c33918e15f398672c12e27b83df51a19076cd6250e4e42b830baf3e858274fde41ec822890c0fa",
    ),
    (
        "linux-x64",
        "990ddb56b5299c3dafb3b413d2f5fdd0bb7472752ae3aeee16d124b4876c249996dbde91a1250b446a968331b500ac720693430fa97864ee2a29fe928320b6dd",
    ),
];

/// One exact npm platform package: registry identity plus the pinned tarball integrity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RipgrepPackage {
    /// Native target the package serves.
    pub target: Target,
    /// npm platform suffix, for example `darwin-arm64`.
    pub platform: String,
    /// Package version.
    pub version: String,
    /// Lowercase hex SHA-512 of the registry tarball.
    pub archive_sha512: String,
}

impl RipgrepPackage {
    /// The package the source lockfile pins for a target.
    ///
    /// # Errors
    /// Rejects a target the source ships no ripgrep package for.
    pub fn pinned(target: &Target) -> anyhow::Result<Self> {
        let platform = npm_platform(target);
        let archive_sha512 = PINNED
            .iter()
            .find(|(candidate, _)| *candidate == platform)
            .map(|(_, digest)| (*digest).to_owned())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the source ships no ripgrep package for {}",
                    target.platform_arch()
                )
            })?;
        Ok(Self {
            target: target.clone(),
            platform,
            version: PACKAGE_VERSION.to_owned(),
            archive_sha512,
        })
    }

    /// npm package name.
    #[must_use]
    pub fn name(&self) -> String {
        format!("@vscode/ripgrep-{}", self.platform)
    }

    /// Registry tarball filename.
    #[must_use]
    pub fn archive(&self) -> String {
        format!("ripgrep-{}-{}.tgz", self.platform, self.version)
    }

    /// Canonical registry tarball URL.
    #[must_use]
    pub fn url(&self) -> String {
        format!(
            "https://registry.npmjs.org/{}/-/{}",
            self.name(),
            self.archive()
        )
    }
}

fn npm_platform(target: &Target) -> String {
    let os = match target.platform() {
        Platform::Macos => "darwin",
        Platform::Linux => "linux",
    };
    let arch = match target.arch() {
        Arch::X64 => "x64",
        Arch::Arm64 => "arm64",
    };
    format!("{os}-{arch}")
}

/// Verified package contents kept alive until release staging completes.
#[derive(Debug)]
pub struct AcquiredRipgrep {
    /// The exact package acquired.
    pub package: RipgrepPackage,
    extracted: tempfile::TempDir,
}

impl AcquiredRipgrep {
    /// Temporary extraction root containing only `rg` and `LICENSE`.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.extracted.path()
    }
}

/// Provenance of the packaged binary carried in a release closure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepProvenance {
    /// npm package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Native platform and architecture, for example `macos-arm64`.
    pub target: String,
    /// Registry tarball filename.
    pub archive: String,
    /// Lowercase hex SHA-512 verified before extracting the tarball.
    pub archive_sha512: String,
    /// Canonical registry tarball URL.
    pub url: String,
    /// Relative executable path inside the closure.
    pub executable: String,
    /// Relative license path inside the closure.
    pub license: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    package: RipgrepProvenance,
    files: BTreeMap<String, AssetFile>,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct AssetFile {
    bytes: u64,
    sha256: String,
}

/// Acquires the pinned package for every target, reusing verified cached tarballs.
///
/// Tarballs are verified on every use; only the executable and license are extracted, to a
/// fresh temporary directory per package.
///
/// # Errors
/// Rejects transport errors, digest mismatches, unsafe archive paths, missing members, and a
/// binary of the wrong native format.
pub fn acquire_packages(
    packages: &[RipgrepPackage],
    cache: &Path,
    fetcher: &dyn DistributionFetcher,
) -> anyhow::Result<Vec<AcquiredRipgrep>> {
    fs::create_dir_all(cache)?;
    anyhow::ensure!(
        fs::symlink_metadata(cache)?.is_dir(),
        "ripgrep package cache is not a real directory"
    );
    let mut acquired = Vec::new();
    for package in packages {
        anyhow::ensure!(
            valid_sha512(&package.archive_sha512),
            "pinned ripgrep integrity for {} is malformed",
            package.name()
        );
        let version_dir = cache.join(&package.version);
        fs::create_dir_all(&version_dir)?;
        anyhow::ensure!(
            fs::symlink_metadata(&version_dir)?.is_dir(),
            "ripgrep version cache is not a real directory"
        );
        let archive = version_dir.join(package.archive());
        if archive.exists() {
            anyhow::ensure!(
                fs::symlink_metadata(&archive)?.is_file(),
                "cached ripgrep package is not a regular file"
            );
            anyhow::ensure!(
                sha512(&archive)? == package.archive_sha512,
                "cached ripgrep package digest mismatch: {}",
                archive.display()
            );
        } else {
            let temporary = tempfile::NamedTempFile::new_in(&version_dir)?;
            fetcher.download(&package.url(), temporary.path())?;
            anyhow::ensure!(
                sha512(temporary.path())? == package.archive_sha512,
                "ripgrep package digest mismatch: {}",
                package.archive()
            );
            temporary.persist_noclobber(&archive)?;
        }
        let extracted = extract(&archive, package, &version_dir)?;
        acquired.push(AcquiredRipgrep {
            package: package.clone(),
            extracted,
        });
    }
    Ok(acquired)
}

fn extract(
    archive: &Path,
    package: &RipgrepPackage,
    cache: &Path,
) -> anyhow::Result<tempfile::TempDir> {
    let listing = Command::new("tar").args(["-tzf"]).arg(archive).output()?;
    anyhow::ensure!(
        listing.status.success(),
        "ripgrep package index failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listing = String::from_utf8(listing.stdout)?;
    let paths = listing.lines().collect::<Vec<_>>();
    anyhow::ensure!(
        paths.iter().all(|path| safe_relative(Path::new(path))),
        "ripgrep package contains an unsafe path"
    );
    let directory = tempfile::tempdir_in(cache)?;
    for (member, output) in [("package/bin/rg", EXECUTABLE), ("package/LICENSE", LICENSE)] {
        anyhow::ensure!(
            paths.iter().filter(|path| **path == member).count() == 1,
            "ripgrep package must contain exactly one {member}"
        );
        let status = Command::new("tar")
            .args(["-xzf"])
            .arg(archive)
            .args(["-C"])
            .arg(directory.path())
            .args(["--", member])
            .status()?;
        anyhow::ensure!(status.success(), "ripgrep package extraction failed");
        let extracted = directory.path().join(member);
        anyhow::ensure!(
            fs::symlink_metadata(&extracted)?.is_file(),
            "ripgrep package member {member} is not a regular file"
        );
        fs::rename(&extracted, directory.path().join(output))?;
    }
    fs::remove_dir_all(directory.path().join("package"))?;
    let executable = directory.path().join(EXECUTABLE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    }
    validate_native_artifact(&executable, &package.target)?;
    anyhow::ensure!(
        !fs::read(directory.path().join(LICENSE))?.is_empty(),
        "ripgrep package license is empty"
    );
    Ok(directory)
}

/// Stages one acquired package as a closure at `destination` and verifies it.
///
/// # Errors
/// Rejects an existing destination, copy failures, and a closure that fails verification.
pub fn stage_package(acquired: &AcquiredRipgrep, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        !destination.exists(),
        "ripgrep closure destination already exists: {}",
        destination.display()
    );
    fs::create_dir_all(destination)?;
    copy_regular_file(
        &acquired.directory().join(EXECUTABLE),
        &destination.join(EXECUTABLE),
    )?;
    copy_regular_file(
        &acquired.directory().join(LICENSE),
        &destination.join(LICENSE),
    )?;
    let package = &acquired.package;
    let provenance = RipgrepProvenance {
        package: package.name(),
        version: package.version.clone(),
        target: package.target.platform_arch(),
        archive: package.archive(),
        archive_sha512: package.archive_sha512.clone(),
        url: package.url(),
        executable: EXECUTABLE.to_owned(),
        license: LICENSE.to_owned(),
    };
    write_manifest(destination, provenance)?;
    verify_directory_against(destination, package)?;
    Ok(())
}

/// Writes the provenance manifest for a staged closure, hashing every other file in it.
///
/// # Errors
/// Rejects symlinks and non-regular entries below the closure.
pub fn write_manifest(directory: &Path, package: RipgrepProvenance) -> anyhow::Result<()> {
    let manifest = Manifest {
        schema_version: 1,
        package,
        files: files(directory)?,
    };
    fs::write(
        directory.join(MANIFEST),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(())
}

/// Validates a staged closure against the package the source pins for `target`: manifest,
/// provenance, exact file set, and native binary.
///
/// # Errors
/// Rejects missing or changed files, symlinks, provenance that does not identify the pinned
/// package, and a wrong native binary.
pub fn verify_directory(directory: &Path, target: &Target) -> anyhow::Result<RipgrepProvenance> {
    verify_directory_against(directory, &RipgrepPackage::pinned(target)?)
}

/// Validates a staged closure against one exact package.
///
/// # Errors
/// Rejects missing or changed files, symlinks, provenance that does not identify `pinned`,
/// and a wrong native binary.
pub fn verify_directory_against(
    directory: &Path,
    pinned: &RipgrepPackage,
) -> anyhow::Result<RipgrepProvenance> {
    let target = &pinned.target;
    let manifest: Manifest = serde_json::from_slice(&fs::read(directory.join(MANIFEST))?)?;
    anyhow::ensure!(
        manifest.schema_version == 1,
        "ripgrep closure manifest schema {} is unsupported",
        manifest.schema_version
    );
    let provenance = manifest.package;
    anyhow::ensure!(
        provenance.target == target.platform_arch(),
        "ripgrep closure target {} does not match {}",
        provenance.target,
        target.platform_arch()
    );
    anyhow::ensure!(
        provenance.package == pinned.name()
            && provenance.version == pinned.version
            && provenance.archive == pinned.archive()
            && provenance.url == pinned.url()
            && provenance.archive_sha512 == pinned.archive_sha512,
        "ripgrep closure provenance does not identify the pinned {} package",
        pinned.name()
    );
    anyhow::ensure!(
        provenance.executable == EXECUTABLE && provenance.license == LICENSE,
        "ripgrep closure has unsupported executable or license paths"
    );
    let actual = files(directory)?;
    for (name, file) in &manifest.files {
        anyhow::ensure!(
            actual.get(name) == Some(file),
            "ripgrep closure file {name} is missing or modified"
        );
    }
    for name in actual.keys() {
        anyhow::ensure!(
            manifest.files.contains_key(name),
            "ripgrep closure contains an unlisted file {name}"
        );
    }
    anyhow::ensure!(
        manifest.files.contains_key(EXECUTABLE) && manifest.files.contains_key(LICENSE),
        "ripgrep closure manifest omits the executable or license"
    );
    validate_native_artifact(&directory.join(&provenance.executable), target)?;
    anyhow::ensure!(
        !fs::read(directory.join(&provenance.license))?.is_empty(),
        "ripgrep closure license is empty"
    );
    Ok(provenance)
}

/// Copies a verified closure and verifies the copy.
///
/// # Errors
/// Rejects an invalid closure, an existing destination, symlinks, and copy failures.
pub fn copy_directory(source: &Path, destination: &Path, target: &Target) -> anyhow::Result<()> {
    verify_directory(source, target)?;
    anyhow::ensure!(
        fs::symlink_metadata(source)?.is_dir(),
        "ripgrep closure source is not a real directory: {}",
        source.display()
    );
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_regular_file(&entry.path(), &destination.join(entry.file_name()))?;
    }
    verify_directory(destination, target)?;
    Ok(())
}

/// Finds the closure beside a runtime executable: target-qualified first, then flat.
///
/// # Errors
/// Fails when neither layout holds a valid closure.
pub fn adjacent_directory(executable: &Path, target: &Target) -> anyhow::Result<PathBuf> {
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime executable has no parent"))?;
    for candidate in [
        parent.join(DIRECTORY).join(target.platform_arch()),
        parent.join(DIRECTORY),
    ] {
        if candidate.join(MANIFEST).exists() {
            verify_directory(&candidate, target)?;
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "packaged ripgrep is missing beside {}: expected {DIRECTORY}/{} or a flat {DIRECTORY} closure",
        executable.display(),
        target.platform_arch()
    );
}

fn files(root: &Path) -> anyhow::Result<BTreeMap<String, AssetFile>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink() && metadata.is_file(),
            "ripgrep closure entries must be regular files: {}",
            path.display()
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("ripgrep closure file name is not UTF-8"))?;
        if name == MANIFEST {
            continue;
        }
        let content = fs::read(&path)?;
        files.insert(
            name,
            AssetFile {
                bytes: u64::try_from(content.len())?,
                sha256: format!("{:x}", Sha256::digest(&content)),
            },
        );
    }
    Ok(files)
}

fn copy_regular_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        fs::symlink_metadata(source)?.is_file(),
        "ripgrep closure asset is not a regular file: {}",
        source.display()
    );
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut File::open(source)?, &mut output)?;
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}

fn valid_sha512(value: &str) -> bool {
    value.len() == 128 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha512(path: &Path) -> anyhow::Result<String> {
    let mut reader = File::open(path)?;
    let mut digest = Sha512::new();
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
