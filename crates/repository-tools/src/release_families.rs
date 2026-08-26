//! Independent `SeekDeep` and vendor npm release-family policy.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use regex::Regex;
use serde_json::Value;

use crate::publication_payload::validate_tarball_payload;

const WORKSPACE_ROOT_PACKAGE: &str = "@seekdeep-ai/seekdeep-root";
const PACKAGE_SCOPE: &str = "@seekdeep-ai/";

/// One publishable package in a release family.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseMember {
    /// Repository-relative package directory.
    pub directory: String,
    /// Published package name.
    pub name: String,
    /// Published package version.
    pub version: String,
    /// Complete manifest.
    pub manifest: Value,
}

/// Executable used to verify installed artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledEntry {
    /// Package carrying the executable.
    pub package_name: String,
    /// Executable path inside the package.
    pub bin_path: String,
}

/// Closed release families owned by the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseFamily {
    /// `packages/*` plus `apps/*`, one shared version.
    SeekDeep,
    /// `vendor/*`, independent upstream-derived versions.
    Vendor,
}

impl ReleaseFamily {
    /// Resolves a workflow-facing family identifier.
    ///
    /// # Errors
    ///
    /// Returns the closed known-family diagnostic.
    pub fn resolve(identifier: &str) -> anyhow::Result<Self> {
        match identifier {
            "seekdeep" => Ok(Self::SeekDeep),
            "vendor" => Ok(Self::Vendor),
            _ => anyhow::bail!(
                "unknown release family {identifier}; expected one of seekdeep, vendor"
            ),
        }
    }

    /// Workflow-facing identifier.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::SeekDeep => "seekdeep",
            Self::Vendor => "vendor",
        }
    }

    /// Discovers, validates, and directory-sorts family members.
    ///
    /// # Errors
    ///
    /// Returns traversal, JSON, shape, scope, root-selection, duplicate, or
    /// empty-family diagnostics.
    pub fn members(self, root: &Path) -> anyhow::Result<Vec<ReleaseMember>> {
        let mut manifests = family_manifests(root, self)?;
        manifests.sort_by(|left, right| {
            relative(root, left)
                .encode_utf16()
                .cmp(relative(root, right).encode_utf16())
        });
        if manifests.is_empty() {
            anyhow::bail!("release family {} matched no manifests", self.identifier());
        }
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        for path in manifests {
            let relative = relative(root, &path);
            let manifest = serde_json::from_str::<Value>(&std::fs::read_to_string(&path)?)?;
            let object = manifest
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
            let name = required_string(object, "name", &relative)?;
            let version = required_string(object, "version", &relative)?;
            if name == WORKSPACE_ROOT_PACKAGE {
                anyhow::bail!("{relative} selected the workspace root");
            }
            if !name.starts_with(PACKAGE_SCOPE) {
                anyhow::bail!("{relative} must name an @seekdeep-ai package");
            }
            if !seen.insert(name.to_owned()) {
                anyhow::bail!(
                    "{name} appears twice in release family {}",
                    self.identifier()
                );
            }
            members.push(ReleaseMember {
                directory: relative
                    .strip_suffix("/package.json")
                    .unwrap_or(&relative)
                    .to_owned(),
                name: name.to_owned(),
                version: version.to_owned(),
                manifest,
            });
        }
        Ok(members)
    }

    /// Orders every member after its in-family runtime dependencies.
    ///
    /// # Errors
    ///
    /// Returns a deterministic dependency-cycle chain.
    pub fn publish_order(self, members: &[ReleaseMember]) -> anyhow::Result<Vec<ReleaseMember>> {
        let by_name = members
            .iter()
            .map(|member| (member.name.as_str(), member))
            .collect::<HashMap<_, _>>();
        let mut seeds = members.iter().collect::<Vec<_>>();
        seeds.sort_by(|left, right| utf16_compare(&left.name, &right.name));
        let mut ordered = Vec::new();
        let mut placed = HashSet::new();
        let mut visiting = HashSet::new();
        for member in seeds {
            self.visit_member(
                member,
                &by_name,
                &mut placed,
                &mut visiting,
                &mut Vec::new(),
                &mut ordered,
            )?;
        }
        Ok(ordered)
    }

    /// Enforces the family's version baseline.
    ///
    /// # Errors
    ///
    /// Returns shared-version or publishable-semver diagnostics.
    pub fn verify_versions(self, members: &[ReleaseMember]) -> anyhow::Result<()> {
        static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
                .expect("static publishable version regex")
        });
        match self {
            Self::SeekDeep => {
                let versions = members
                    .iter()
                    .map(|member| member.version.as_str())
                    .collect::<HashSet<_>>();
                if versions.len() != 1 {
                    let detail = members
                        .iter()
                        .map(|member| format!("{}: {}", member.directory, member.version))
                        .collect::<Vec<_>>()
                        .join("\n");
                    anyhow::bail!("seekdeep release members must share one version:\n{detail}");
                }
            }
            Self::Vendor => {
                for member in members {
                    if !VERSION.is_match(&member.version) {
                        anyhow::bail!(
                            "{} has an unpublishable version: {}",
                            member.directory,
                            member.version
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Tag prefix for one member.
    #[must_use]
    pub fn tag_prefix_for(self, member: &ReleaseMember) -> String {
        match self {
            Self::SeekDeep => "seekdeep-v".to_owned(),
            Self::Vendor => format!(
                "vendor-{}-v",
                member
                    .name
                    .strip_prefix(PACKAGE_SCOPE)
                    .unwrap_or(&member.name)
            ),
        }
    }

    /// Full tag for one member version.
    #[must_use]
    pub fn tag_for(self, member: &ReleaseMember) -> String {
        format!("{}{}", self.tag_prefix_for(member), member.version)
    }

    /// Validates one packed payload according to family policy.
    ///
    /// # Errors
    ///
    /// Returns forbidden `SeekDeep` members or empty vendor payloads.
    pub fn validate_payload(self, member: &ReleaseMember, files: &[String]) -> anyhow::Result<()> {
        match self {
            Self::SeekDeep => validate_tarball_payload(files, &member.name),
            Self::Vendor if files.is_empty() => {
                anyhow::bail!("{} packed an empty tarball", member.name)
            }
            Self::Vendor => Ok(()),
        }
    }

    /// Installed executable probe, absent for the vendor library family.
    #[must_use]
    pub fn installed_entry(self) -> Option<InstalledEntry> {
        (self == Self::SeekDeep).then(|| InstalledEntry {
            package_name: "@seekdeep-ai/seekdeep".to_owned(),
            bin_path: "lib/bin.js".to_owned(),
        })
    }

    fn visit_member(
        self,
        member: &ReleaseMember,
        by_name: &HashMap<&str, &ReleaseMember>,
        placed: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        path: &mut Vec<String>,
        ordered: &mut Vec<ReleaseMember>,
    ) -> anyhow::Result<()> {
        if placed.contains(&member.name) {
            return Ok(());
        }
        if visiting.contains(&member.name) {
            let mut cycle = path.clone();
            cycle.push(member.name.clone());
            anyhow::bail!(
                "dependency cycle in release family {}: {}",
                self.identifier(),
                cycle.join(" -> ")
            );
        }
        visiting.insert(member.name.clone());
        path.push(member.name.clone());
        for dependency in order_edges(member, by_name) {
            self.visit_member(dependency, by_name, placed, visiting, path, ordered)?;
        }
        path.pop();
        visiting.remove(&member.name);
        placed.insert(member.name.clone());
        ordered.push(member.clone());
        Ok(())
    }
}

/// npm tarball filename written for one member.
#[must_use]
pub fn tarball_name(member: &ReleaseMember) -> String {
    let unscoped = member
        .name
        .strip_prefix('@')
        .map_or_else(|| member.name.clone(), |name| name.replacen('/', "-", 1));
    format!("{unscoped}-{}.tgz", member.version)
}

fn order_edges<'a>(
    member: &'a ReleaseMember,
    by_name: &HashMap<&str, &'a ReleaseMember>,
) -> Vec<&'a ReleaseMember> {
    let mut edges = Vec::new();
    for section in ["dependencies", "optionalDependencies"] {
        let Some(dependencies) = member.manifest.get(section).and_then(Value::as_object) else {
            continue;
        };
        for name in dependencies.keys() {
            if let Some(dependency) = by_name.get(name.as_str())
                && dependency.name != member.name
            {
                edges.push(*dependency);
            }
        }
    }
    edges.sort_by(|left, right| utf16_compare(&left.name, &right.name));
    edges
}

fn family_manifests(root: &Path, family: ReleaseFamily) -> anyhow::Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    match family {
        ReleaseFamily::SeekDeep => {
            let packages = root.join("packages");
            if packages.is_dir() {
                for group in directories(&packages)? {
                    for package in directories(&group)? {
                        let path = package.join("package.json");
                        if path.is_file() {
                            manifests.push(path);
                        }
                    }
                }
            }
            let apps = root.join("apps");
            if apps.is_dir() {
                for app in directories(&apps)? {
                    let path = app.join("package.json");
                    if path.is_file() {
                        manifests.push(path);
                    }
                }
            }
        }
        ReleaseFamily::Vendor => {
            let vendor = root.join("vendor");
            if vendor.is_dir() {
                for package in directories(&vendor)? {
                    let path = package.join("package.json");
                    if path.is_file() {
                        manifests.push(path);
                    }
                }
            }
        }
    }
    Ok(manifests)
}

fn directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            output.push(entry.path());
        }
    }
    Ok(output)
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} must declare a string {field}"))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn utf16_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
