use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use base64::Engine as _;
use path_clean::PathClean as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256, Sha512};

use super::{
    BaselineCommit, BaselinePackageName, BaselineRunner, BaselineVersion, RELEASE_MANIFEST_NAME,
    normalize_registry, run_options, strings,
};

const DEPENDENCY_SECTIONS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

/// Whether a package follows harness or vendored publication-payload policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageOrigin {
    /// A first-party package whose source and source maps must not be shipped.
    #[default]
    Harness,
    /// A rescoped upstream package retaining its upstream payload contract.
    Vendor,
}

/// One workspace package selected for baseline publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BaselinePackage {
    /// Published package identity.
    pub name: BaselinePackageName,
    /// Repository-relative package directory.
    pub directory: PathBuf,
    /// Payload policy used for this package.
    pub origin: PackageOrigin,
}

/// Validated workspace package membership and stable root version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePackageSet {
    /// Packages in source-compatible locale name order.
    pub packages: Vec<BaselinePackage>,
    /// Stable root version from which a commit-addressed version is derived.
    pub base_version: BaselineVersion,
}

impl WorkspacePackageSet {
    /// Discovers the source's vendor, grouped-package, and application manifests.
    ///
    /// # Errors
    /// Returns missing manifests, invalid identities, duplicates, or inconsistent versions.
    pub fn discover(root: &Path) -> anyhow::Result<Self> {
        let mut paths = Vec::new();
        for pattern in [
            "vendor/*/package.json",
            "packages/*/*/package.json",
            "apps/*/package.json",
        ] {
            let pattern = format!(
                "{}/{pattern}",
                glob::Pattern::escape(&root.to_string_lossy())
            );
            for path in glob::glob_with(
                &pattern,
                glob::MatchOptions {
                    require_literal_leading_dot: true,
                    ..glob::MatchOptions::new()
                },
            )? {
                paths.push(
                    path?
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        paths.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        if paths.is_empty() {
            anyhow::bail!("no package manifests found under vendor/, packages/, or apps/");
        }
        let base_version = expect_string(
            &read_object(&root.join("package.json"))?,
            "version",
            "package.json",
        )?;
        validate_base_version(&base_version, "package.json")?;
        let mut names = BTreeSet::new();
        let mut packages = Vec::new();
        for path in paths {
            let manifest = read_object(&root.join(&path))?;
            let name = expect_string(&manifest, "name", &path)?;
            let version = expect_string(&manifest, "version", &path)?;
            let vendor = path.starts_with("vendor/");
            if !name.starts_with("@seekdeep-ai/") {
                anyhow::bail!("{path} must name an @seekdeep-ai package");
            }
            if name == "@seekdeep-ai/seekdeep-root" {
                anyhow::bail!("{path} unexpectedly selected the workspace root");
            }
            if !names.insert(name.clone()) {
                anyhow::bail!("duplicate package name: {name}");
            }
            if !vendor && version != base_version {
                anyhow::bail!("{path} has version {version}; expected {base_version}");
            }
            packages.push(BaselinePackage {
                name: BaselinePackageName::new(name),
                directory: Path::new(&path)
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_owned(),
                origin: if vendor {
                    PackageOrigin::Vendor
                } else {
                    PackageOrigin::Harness
                },
            });
        }
        packages.sort_by(|left, right| {
            crate::ts_lexical::locale_compare(left.name.as_str(), right.name.as_str())
        });
        Ok(Self {
            packages,
            base_version: BaselineVersion::new(base_version),
        })
    }

    /// Pins all internal dependency sections and makes each selected package public.
    ///
    /// # Errors
    /// Returns manifest, dependency-shape, or write failures.
    pub fn stage(&self, root: &Path, version: &BaselineVersion) -> anyhow::Result<()> {
        let names = self
            .packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<BTreeSet<_>>();
        for package in &self.packages {
            let path = root.join(&package.directory).join("package.json");
            let mut manifest = read_object(&path)?;
            manifest.insert("version".to_owned(), Value::String(version.to_string()));
            manifest.shift_remove("private");
            for section in DEPENDENCY_SECTIONS {
                let Some(dependencies) = manifest.get_mut(*section) else {
                    continue;
                };
                let dependencies = dependencies.as_object_mut().ok_or_else(|| {
                    anyhow::anyhow!("{} {section} must be an object", path.display())
                })?;
                for (name, range) in dependencies {
                    if names.contains(&BaselinePackageName::new(name.clone())) {
                        *range = Value::String(version.to_string());
                    }
                }
            }
            write_json(&path, &manifest)?;
        }
        Ok(())
    }
}

/// Identity and independent digests of one immutable tarball.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackedBaselinePackage {
    /// Published package identity.
    pub name: BaselinePackageName,
    /// Filename relative to the bundle directory.
    pub tarball: String,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
    /// npm SHA-512 subresource-integrity value.
    pub integrity: String,
    /// Payload policy, with legacy manifests defaulting to harness.
    #[serde(default)]
    pub origin: PackageOrigin,
}

/// Persisted version-one baseline manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseManifest {
    /// Schema discriminator; only one is supported.
    pub schema_version: u32,
    /// Repository commit used by the detached build.
    pub commit: BaselineCommit,
    /// Exact version shared by all tarballs.
    pub version: BaselineVersion,
    /// Development channel updated for each package.
    pub dist_tag: String,
    /// Registry to which publication and verification are bound.
    pub registry: String,
    /// Packages in the persisted publication order.
    pub packages: Vec<PackedBaselinePackage>,
}

/// Locally verified immutable release metadata and tarballs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseBundle {
    /// Absolute directory containing the tarballs and manifest.
    pub directory: PathBuf,
    /// Validated persisted metadata.
    pub manifest: ReleaseManifest,
}

impl ReleaseBundle {
    /// Verifies packed membership and payloads before writing the manifest and sums.
    ///
    /// # Errors
    /// Returns package membership, identity, payload, pin, tar, hash, or write failures.
    pub fn create(
        directory: &Path,
        expected: &[BaselinePackage],
        mut manifest: ReleaseManifest,
        runner: &mut impl BaselineRunner,
    ) -> anyhow::Result<Self> {
        let names = expected
            .iter()
            .map(|package| package.name.clone())
            .collect::<BTreeSet<_>>();
        let mut missing = names.clone();
        let mut tarballs = std::fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        tarballs.retain(|name| is_baseline_tarball(name));
        tarballs.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        let mut packages = Vec::new();
        for tarball in tarballs {
            let path = directory.join(&tarball);
            let artifact = inspect_tarball(&path, runner)?;
            let expected = expected
                .iter()
                .find(|package| package.name == artifact.name);
            let Some(expected) = expected.filter(|_| missing.remove(&artifact.name)) else {
                anyhow::bail!("unexpected or duplicate packed package: {}", artifact.name);
            };
            verify_artifact(
                &artifact,
                expected.origin,
                &names,
                &manifest.version,
                &tarball,
            )?;
            packages.push(packed_package(artifact.name, &path, expected.origin)?);
        }
        if !missing.is_empty() {
            let mut missing = missing
                .into_iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            missing.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            anyhow::bail!("missing tarballs for: {}", missing.join(", "));
        }
        packages.sort_by(|left, right| {
            crate::ts_lexical::locale_compare(left.name.as_str(), right.name.as_str())
        });
        manifest.packages = packages;
        write_json(&directory.join(RELEASE_MANIFEST_NAME), &manifest)?;
        std::fs::write(
            directory.join("SHA256SUMS"),
            format!(
                "{}\n",
                manifest
                    .packages
                    .iter()
                    .map(|package| format!("{}  {}", package.sha256, package.tarball))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )?;
        Ok(Self {
            directory: directory.to_owned(),
            manifest,
        })
    }

    /// Loads metadata and rechecks every local checksum, identity, payload, and pin.
    ///
    /// # Errors
    /// Returns malformed or duplicate metadata, unsafe paths, tampering, and artifact failures.
    pub fn load(manifest_path: &Path, runner: &mut impl BaselineRunner) -> anyhow::Result<Self> {
        let manifest_path = std::path::absolute(manifest_path)?.clean();
        let raw = read_object(&manifest_path)?;
        if raw.get("schemaVersion").and_then(Value::as_f64) != Some(1.0) {
            anyhow::bail!(
                "unsupported release manifest schema: {}",
                js_value(raw.get("schemaVersion"))
            );
        }
        let package_values = raw
            .get("packages")
            .and_then(Value::as_array)
            .filter(|packages| !packages.is_empty())
            .ok_or_else(|| anyhow::anyhow!("release manifest contains no packages"))?;
        let mut names = BTreeSet::new();
        let mut packages = Vec::new();
        for (index, value) in package_values.iter().enumerate() {
            let package = parse_packed_package(value, index)?;
            if !names.insert(package.name.clone()) {
                anyhow::bail!("duplicate package in release manifest: {}", package.name);
            }
            packages.push(package);
        }
        let manifest = ReleaseManifest {
            schema_version: 1,
            commit: BaselineCommit::new(expect_string(&raw, "commit", RELEASE_MANIFEST_NAME)?),
            version: BaselineVersion::new(expect_string(&raw, "version", RELEASE_MANIFEST_NAME)?),
            dist_tag: expect_string(&raw, "distTag", RELEASE_MANIFEST_NAME)?,
            registry: normalize_registry(&expect_string(&raw, "registry", RELEASE_MANIFEST_NAME)?)?,
            packages,
        };
        let bundle = Self {
            directory: manifest_path.parent().unwrap_or(Path::new(".")).to_owned(),
            manifest,
        };
        bundle.verify_local(runner)?;
        Ok(bundle)
    }

    /// Resolves one validated bundle filename to its absolute tarball path.
    #[must_use]
    pub fn tarball_path(&self, package: &PackedBaselinePackage) -> PathBuf {
        self.directory.join(&package.tarball).clean()
    }

    /// Rechecks the complete local bundle without contacting a registry.
    ///
    /// # Errors
    /// Returns unsafe paths, tampering, identity, payload, and dependency-pin errors.
    pub fn verify_local(&self, runner: &mut impl BaselineRunner) -> anyhow::Result<()> {
        let names = self
            .manifest
            .packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<BTreeSet<_>>();
        for package in &self.manifest.packages {
            let path = Path::new(&package.tarball);
            let filename = package
                .tarball
                .strip_suffix(std::path::MAIN_SEPARATOR)
                .unwrap_or(&package.tarball);
            if path.is_absolute()
                || filename.is_empty()
                || filename.contains(std::path::MAIN_SEPARATOR)
            {
                anyhow::bail!(
                    "invalid tarball path for {}: {}",
                    package.name,
                    package.tarball
                );
            }
            let path = self.tarball_path(package);
            let actual = packed_package(package.name.clone(), &path, package.origin)?;
            if actual.sha256 != package.sha256 || actual.integrity != package.integrity {
                anyhow::bail!("tarball checksum mismatch: {}", package.tarball);
            }
            let artifact = inspect_tarball(&path, runner)?;
            if package.origin == PackageOrigin::Harness {
                crate::publication_payload::validate_tarball_payload(
                    &artifact.files,
                    &package.tarball,
                )?;
            }
            if artifact.name != package.name || artifact.version != self.manifest.version {
                anyhow::bail!("tarball identity mismatch: {}", package.tarball);
            }
            verify_private_and_dependencies(
                &artifact.manifest,
                &names,
                &self.manifest.version,
                &package.tarball,
            )?;
        }
        Ok(())
    }
}

struct InspectedTarball {
    name: BaselinePackageName,
    version: BaselineVersion,
    manifest: Map<String, Value>,
    files: Vec<String>,
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_baseline_tarball(name: &str) -> bool {
    name.ends_with(".tgz")
}

fn inspect_tarball(
    path: &Path,
    runner: &mut impl BaselineRunner,
) -> anyhow::Result<InspectedTarball> {
    let options = run_options(path.parent().unwrap_or(Path::new(".")));
    let manifest_source = runner.capture(
        "tar",
        &strings(&["-xOf", &path.to_string_lossy(), "package/package.json"]),
        &options,
    )?;
    let value = serde_json::from_str::<Value>(&manifest_source)?;
    let manifest = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{} contains an invalid package.json", path.display()))?
        .clone();
    let name = BaselinePackageName::new(expect_string(&manifest, "name", &path.to_string_lossy())?);
    let version = BaselineVersion::new(expect_string(
        &manifest,
        "version",
        &path.to_string_lossy(),
    )?);
    let files = runner
        .capture("tar", &strings(&["-tf", &path.to_string_lossy()]), &options)?
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    Ok(InspectedTarball {
        name,
        version,
        manifest,
        files,
    })
}

fn packed_package(
    name: BaselinePackageName,
    path: &Path,
    origin: PackageOrigin,
) -> anyhow::Result<PackedBaselinePackage> {
    let bytes = std::fs::read(path)?;
    Ok(PackedBaselinePackage {
        name,
        tarball: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        integrity: format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes))
        ),
        origin,
    })
}

fn parse_packed_package(value: &Value, index: usize) -> anyhow::Result<PackedBaselinePackage> {
    let context = format!("release manifest package at index {index}");
    let value = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid {context}"))?;
    let name = expect_string(value, "name", &context)?;
    let origin = match value.get("origin") {
        None => PackageOrigin::Harness,
        Some(Value::String(origin)) if origin == "harness" => PackageOrigin::Harness,
        Some(Value::String(origin)) if origin == "vendor" => PackageOrigin::Vendor,
        Some(origin) => anyhow::bail!("invalid package origin in release manifest: {origin}"),
    };
    if origin == PackageOrigin::Harness
        && (!name.starts_with("@seekdeep-ai/") || name == "@seekdeep-ai/seekdeep-root")
    {
        anyhow::bail!("invalid package name in release manifest: {name}");
    }
    Ok(PackedBaselinePackage {
        name: BaselinePackageName::new(name),
        tarball: expect_string(value, "tarball", &context)?,
        sha256: expect_string(value, "sha256", &context)?,
        integrity: expect_string(value, "integrity", &context)?,
        origin,
    })
}

fn verify_artifact(
    artifact: &InspectedTarball,
    origin: PackageOrigin,
    names: &BTreeSet<BaselinePackageName>,
    version: &BaselineVersion,
    context: &str,
) -> anyhow::Result<()> {
    if origin == PackageOrigin::Harness {
        crate::publication_payload::validate_tarball_payload(&artifact.files, context)?;
    }
    if artifact.version != *version {
        anyhow::bail!(
            "{context} has version {}; expected {version}",
            artifact.version
        );
    }
    verify_private_and_dependencies(&artifact.manifest, names, version, context)
}

fn verify_private_and_dependencies(
    manifest: &Map<String, Value>,
    names: &BTreeSet<BaselinePackageName>,
    version: &BaselineVersion,
    context: &str,
) -> anyhow::Result<()> {
    if manifest.get("private") == Some(&Value::Bool(true)) {
        anyhow::bail!("{context} is still private");
    }
    if manifest.values().any(contains_workspace_protocol) {
        anyhow::bail!("{context} still contains a workspace: dependency");
    }
    for section in DEPENDENCY_SECTIONS {
        let Some(dependencies) = manifest.get(*section) else {
            continue;
        };
        let dependencies = dependencies
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{context} {section} must be an object"))?;
        for (name, range) in dependencies {
            if names.contains(&BaselinePackageName::new(name.clone()))
                && range.as_str() != Some(version.as_str())
            {
                anyhow::bail!(
                    "{context} has internal {section} {name}@{}; expected exact version {version}",
                    js_value(Some(range))
                );
            }
        }
    }
    Ok(())
}

fn contains_workspace_protocol(value: &Value) -> bool {
    match value {
        Value::String(value) => value.starts_with("workspace:"),
        Value::Array(values) => values.iter().any(contains_workspace_protocol),
        Value::Object(values) => values.values().any(contains_workspace_protocol),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub(super) fn js_value(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_value(Some(value))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(value) => value.to_string(),
    }
}

pub(super) fn read_object(path: &Path) -> anyhow::Result<Map<String, Value>> {
    parse_object(&std::fs::read_to_string(path)?, &path.to_string_lossy())
}

pub(super) fn parse_object(source: &str, context: &str) -> anyhow::Result<Map<String, Value>> {
    let value = serde_json::from_str::<Value>(source)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{context} must contain a JSON object"))
}

pub(super) fn expect_string(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> anyhow::Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{context} must contain a non-empty {key}"))
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

pub(super) fn validate_base_version(value: &str, context: &str) -> anyhow::Result<()> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        anyhow::bail!("{context} must have a stable X.Y.Z version, got {value}");
    }
    Ok(())
}
