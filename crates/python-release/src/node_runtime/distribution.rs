//! Source-compatible Node range selection and verified official archive acquisition.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read as _,
    path::Path,
    process::{Command, Stdio},
};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::executable::{Platform, Target, validate_native_artifact};

use super::{safe_relative, valid_sha256};

const INDEX: &str = "https://nodejs.org/dist/index.json";

/// Injectable boundary for official release metadata and archive responses.
pub trait DistributionFetcher {
    /// Writes one complete response to a caller-owned temporary file.
    ///
    /// # Errors
    /// Returns transport or response failures without accepting a partial download.
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()>;
}

/// Build-time HTTPS transfer through the platform curl executable.
pub struct CurlFetcher;

impl DistributionFetcher for CurlFetcher {
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()> {
        let output = Command::new("curl")
            .args([
                "--disable",
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--output",
            ])
            .arg(destination)
            .arg(url)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "official Node download failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

/// One concrete release chosen from the official index for a native target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeDistribution {
    /// Exact v-prefixed upstream version.
    pub version: String,
    /// Requested native platform, architecture, and source Node range.
    pub target: Target,
    /// Official tar.gz distribution filename.
    pub archive: String,
}

impl NodeDistribution {
    /// Creates an official archive descriptor from an exact version.
    ///
    /// # Errors
    /// Rejects malformed versions before using them in paths or URLs.
    pub fn for_version(version: &str, target: &Target) -> anyhow::Result<Self> {
        let raw = version.strip_prefix('v').unwrap_or(version);
        let parts = raw.split('.').collect::<Vec<_>>();
        anyhow::ensure!(
            parts.len() == 3
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
            "invalid exact Node version {version:?}"
        );
        let version = format!("v{raw}");
        let os = match target.platform() {
            Platform::Macos => "darwin",
            Platform::Linux => "linux",
        };
        let archive = format!("node-{version}-{os}-{}.tar.gz", target.arch().as_str());
        Ok(Self {
            version,
            target: target.clone(),
            archive,
        })
    }

    /// Canonical upstream archive URL.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://nodejs.org/dist/{}/{}", self.version, self.archive)
    }

    fn directory_name(&self) -> &str {
        self.archive
            .strip_suffix(".tar.gz")
            .expect("official gzip archive suffix")
    }
}

/// Verified archive contents kept alive until release staging completes.
#[derive(Debug)]
pub struct AcquiredNode {
    /// Concrete official distribution descriptor.
    pub distribution: NodeDistribution,
    /// Digest matched against that version's official SHASUMS256 file.
    pub archive_sha256: String,
    extracted: tempfile::TempDir,
}

impl AcquiredNode {
    /// Temporary extraction root containing only `bin/node` and `LICENSE`.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.extracted.path()
    }
}

#[derive(Deserialize)]
struct IndexEntry {
    version: String,
    files: Vec<String>,
}

/// Resolves the first official release matching source range and platform rules.
///
/// # Errors
/// Rejects malformed index data, unavailable targets, and exact pins outside the requested major.
pub fn select_distribution(
    index: &Value,
    target: &Target,
    exact_pin: Option<&str>,
) -> anyhow::Result<NodeDistribution> {
    let spec = target.spec();
    let range = spec
        .split('-')
        .next()
        .and_then(|range| range.strip_prefix("node"))
        .ok_or_else(|| anyhow::anyhow!("validated target has no Node range"))?;
    let prefix = format!("v{range}");
    if let Some(pin) = exact_pin {
        let selected = NodeDistribution::for_version(pin, target)?;
        anyhow::ensure!(
            selected.version.split('.').next() == Some(prefix.as_str()),
            "exact Node pin {} does not match requested {spec}",
            selected.version
        );
        return Ok(selected);
    }
    let entries: Vec<IndexEntry> = serde_json::from_value(index.clone())?;
    let os = match target.platform() {
        Platform::Macos => "osx",
        Platform::Linux => "linux",
    };
    let platform = format!("{os}-{}", target.arch().as_str());
    let selected = entries
        .into_iter()
        .find(|entry| {
            // `v2` must not match `v24.x`: the major ends at its dot.
            entry.version.starts_with(&format!("{prefix}."))
                && entry.files.iter().any(|file| file.starts_with(&platform))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Node version {range} not found for {}",
                target.platform_arch()
            )
        })?;
    NodeDistribution::for_version(&selected.version, target)
}

/// Finds one exact archive's digest in its official checksum response.
///
/// # Errors
/// Rejects missing, duplicate, or malformed checksum records.
pub fn archive_checksum(checksums: &str, archive: &str) -> anyhow::Result<String> {
    let matches = checksums
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            let filename = parts.next()?;
            let name = filename.strip_prefix('*').unwrap_or(filename);
            (name == archive && parts.next().is_none()).then_some(digest)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [digest] if valid_sha256(digest) => Ok(digest.to_ascii_lowercase()),
        _ => anyhow::bail!("official checksum for {archive} is missing, duplicated, or malformed"),
    }
}

/// Acquires every target with one release-index snapshot and per-version checksum snapshots.
///
/// Archives are verified on every use; only selected regular payloads are extracted to fresh
/// temporary directories. An exact pin permits reuse of a cached checksum response and archive.
///
/// # Errors
/// Rejects transport errors, unsupported selections, digest mismatches, archive paths, and native headers.
pub fn acquire_distributions(
    targets: &[Target],
    cache: &Path,
    exact_pin: Option<&str>,
    fetcher: &dyn DistributionFetcher,
) -> anyhow::Result<Vec<AcquiredNode>> {
    fs::create_dir_all(cache)?;
    anyhow::ensure!(
        fs::symlink_metadata(cache)?.is_dir(),
        "Node distribution cache is not a real directory"
    );
    let index = if exact_pin.is_some() {
        Value::Array(Vec::new())
    } else {
        serde_json::from_slice(&fetch_response(fetcher, INDEX, cache)?)?
    };
    let mut checksums = BTreeMap::<String, String>::new();
    let mut acquired = Vec::new();
    for target in targets {
        let distribution = select_distribution(&index, target, exact_pin)?;
        let version_dir = cache.join(&distribution.version);
        fs::create_dir_all(&version_dir)?;
        anyhow::ensure!(
            fs::symlink_metadata(&version_dir)?.is_dir(),
            "Node version cache is not a real directory"
        );
        if !checksums.contains_key(&distribution.version) {
            let cached = version_dir.join("SHASUMS256.txt");
            if let Ok(metadata) = fs::symlink_metadata(&cached) {
                anyhow::ensure!(
                    metadata.is_file(),
                    "cached Node checksums are not a regular file"
                );
            }
            let content = if exact_pin.is_some() && cached.is_file() {
                fs::read_to_string(&cached)?
            } else {
                let bytes = fetch_response(
                    fetcher,
                    &format!(
                        "https://nodejs.org/dist/{}/SHASUMS256.txt",
                        distribution.version
                    ),
                    &version_dir,
                )?;
                let content = String::from_utf8(bytes)?;
                fs::write(&cached, &content)?;
                content
            };
            checksums.insert(distribution.version.clone(), content);
        }
        let checksum = archive_checksum(&checksums[&distribution.version], &distribution.archive)?;
        let archive = version_dir.join(&distribution.archive);
        if archive.exists() {
            anyhow::ensure!(
                fs::symlink_metadata(&archive)?.is_file(),
                "cached Node archive is not a regular file"
            );
            anyhow::ensure!(
                sha256(&archive)? == checksum,
                "cached Node archive digest mismatch: {}",
                archive.display()
            );
        } else {
            let temporary = tempfile::NamedTempFile::new_in(&version_dir)?;
            fetcher.download(&distribution.url(), temporary.path())?;
            anyhow::ensure!(
                sha256(temporary.path())? == checksum,
                "official Node archive digest mismatch: {}",
                distribution.archive
            );
            temporary.persist_noclobber(&archive)?;
        }
        let extracted = extract(&archive, &distribution, &version_dir)?;
        acquired.push(AcquiredNode {
            distribution,
            archive_sha256: checksum,
            extracted,
        });
    }
    Ok(acquired)
}

fn fetch_response(
    fetcher: &dyn DistributionFetcher,
    url: &str,
    directory: &Path,
) -> anyhow::Result<Vec<u8>> {
    let file = tempfile::NamedTempFile::new_in(directory)?;
    fetcher.download(url, file.path())?;
    Ok(fs::read(file.path())?)
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let mut reader = File::open(path)?;
    let mut digest = Sha256::new();
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

fn extract(
    archive: &Path,
    distribution: &NodeDistribution,
    cache: &Path,
) -> anyhow::Result<tempfile::TempDir> {
    let listing = Command::new("tar").args(["-tzf"]).arg(archive).output()?;
    anyhow::ensure!(
        listing.status.success(),
        "official Node archive index failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listing = String::from_utf8(listing.stdout)?;
    let paths = listing.lines().collect::<Vec<_>>();
    anyhow::ensure!(
        paths.iter().all(|path| safe_relative(Path::new(path))),
        "official Node archive contains an unsafe path"
    );
    let directory = tempfile::tempdir_in(cache)?;
    fs::create_dir(directory.path().join("bin"))?;
    for relative in ["bin/node", "LICENSE"] {
        let member = format!("{}/{relative}", distribution.directory_name());
        anyhow::ensure!(
            paths.iter().filter(|path| **path == member).count() == 1,
            "official Node archive must contain exactly one {member}"
        );
        let metadata = Command::new("tar")
            .arg("-tvzf")
            .arg(archive)
            .arg("--")
            .arg(&member)
            .env("LC_ALL", "C")
            .output()?;
        anyhow::ensure!(
            metadata.status.success() && metadata.stdout.first() == Some(&b'-'),
            "official Node archive member is not a regular file: {member}"
        );
        if relative == "bin/node" {
            anyhow::ensure!(
                matches!(metadata.stdout.get(3), Some(b'x' | b's')),
                "official Node archive executable has no owner execute permission"
            );
        }
        let file = File::create(directory.path().join(relative))?;
        let output = Command::new("tar")
            .arg("-xOzf")
            .arg(archive)
            .arg("--")
            .arg(&member)
            .stdout(Stdio::from(file))
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "official Node archive extraction failed for {member}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            directory.path().join("bin/node"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    validate_native_artifact(&directory.path().join("bin/node"), &distribution.target)?;
    anyhow::ensure!(
        !fs::read(directory.path().join("LICENSE"))?.is_empty(),
        "official Node archive license is empty"
    );
    Ok(directory)
}
