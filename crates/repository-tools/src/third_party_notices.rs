//! Dependency discovery, license policy, and reproducible third-party notices.

use std::{collections::HashSet, fs, path::Path, sync::OnceLock};

use anyhow::{Context as _, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ts_lexical::locale_compare;

mod collect;
mod python;
mod render;
mod spdx;

pub use collect::collect;
pub use python::{collect_python_dependencies, parse_pyproject_requirements};
pub use render::{render, render_collection};
pub use spdx::is_permissive;

/// Exact official SDK identity covered by the pinned owner authorization.
pub const CLAUDE_AGENT_SDK_PACKAGE: &str = "@anthropic-ai/claude-agent-sdk";
const CLAUDE_PLATFORM_DECLARED_LICENSE: &str = "SEE LICENSE IN LICENSE.md";
const DEV_ONLY_AREAS: &[&str] = &[
    "package.json",
    "packages/test-support/",
    "packages/test-support/client-runtime/",
    "website/",
    "examples/",
    "native/",
];
const FIRST_PARTY: &[&str] = &[
    "@seekdeep-ai/node-addon-landlock-run",
    "@seekdeep-ai/node-addon-landlock-run-linux-arm64",
    "@seekdeep-ai/node-addon-landlock-run-linux-x64",
];
const ALL_KINDS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];
const JS_SPACE_BODY: &str = r"\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

/// The parsed installed or workspace npm manifest, preserving declaration order.
pub type Manifest = serde_json::Map<String, Value>;

/// One package's declared license and browsable repository URL.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Metadata {
    /// Installed SPDX expression or declared license reference.
    pub license: String,
    /// Normalized browsable repository URL.
    pub repo: String,
}

/// Python disclosure metadata recorded for a directly declared requirement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PythonMetadata {
    /// Declared license expression.
    pub license: String,
    /// Upstream repository URL.
    pub repo: String,
    /// Dependency role disclosed in the generated table.
    pub role: String,
}

/// A fully resolved direct external npm declaration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalDep {
    /// Exact external npm identity.
    pub name: String,
    /// Installed license declaration, including explicit overrides.
    pub license: String,
    /// Normalized repository or homepage URL.
    pub repo: String,
    /// Whether a shipped workspace area declares a runtime dependency edge.
    pub runtime: bool,
}

/// One vendored-package manifest row, in the canonical table's order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VendoredRow {
    /// Published first-party scope of the vendored package.
    pub npm_name: String,
    /// Original package identity retained for attribution.
    pub upstream_name: String,
    /// Original repository URL.
    pub upstream: String,
}

/// A normalized direct external Python dependency.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PythonDependency {
    /// Normalized Python distribution name.
    pub name: String,
    /// License declaration from the recorded metadata.
    pub license: String,
    /// Upstream repository URL.
    pub repo: String,
    /// Direct-dependency role disclosed in the table.
    pub role: String,
}

/// A local pnpm patch declaration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PatchedDependency {
    /// Package-and-version key from the workspace declaration.
    pub spec: String,
    /// Repository-relative patch path.
    pub patch: String,
}

/// A package invoked directly during a build, outside workspace dependencies.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTimeTool {
    /// Exact fetched package identity.
    pub name: String,
    /// Recorded license declaration.
    pub license: String,
    /// Upstream repository URL.
    pub repo: String,
    /// Purpose of the tool in the build pipeline.
    pub role: String,
    /// Source owner that must retain the package pin.
    pub pin_source: String,
}

/// An official SDK-declared optional platform payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClaudePlatformPayload {
    /// SDK-declared optional package identity.
    pub name: String,
    /// Exact SDK-declared platform package version.
    pub version: String,
}

/// Installed official SDK and CLI distribution facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeDistribution {
    /// Installed official SDK version.
    pub sdk_version: String,
    /// CLI executable version reported by the SDK.
    pub claude_code_version: String,
    /// Declared platform packages in locale order.
    pub payloads: Vec<ClaudePlatformPayload>,
}

/// All validated inputs needed to render notices without further I/O.
#[derive(Clone, Debug, Serialize)]
pub struct NoticeCollection {
    /// Direct external npm declarations in locale order.
    pub npm: Vec<ExternalDep>,
    /// Complete vendored manifest table, in table order.
    pub vendored: Vec<VendoredRow>,
    /// Direct external Python dependencies in normalized name order.
    pub python: Vec<PythonDependency>,
    /// pnpm patches in declaration order.
    pub patched: Vec<PatchedDependency>,
    /// Verified tools fetched during the current build pipeline.
    pub build_time_tools: Vec<BuildTimeTool>,
    /// Optional official SDK and platform distribution facts.
    pub claude_distribution: Option<ClaudeDistribution>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Policy {
    overrides: IndexMap<String, PartialMetadata>,
    python_metadata: IndexMap<String, PythonMetadata>,
    build_time_tools: Vec<BuildTimeTool>,
    permissive_licenses: HashSet<String>,
    licenses: HashSet<String>,
    exceptions: HashSet<String>,
}

#[derive(Deserialize)]
struct PartialMetadata {
    license: Option<String>,
    repo: Option<String>,
}

fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| {
        serde_json::from_str(include_str!("third_party_notices/policy.json"))
            .expect("embedded notices policy is valid JSON")
    })
}

/// The sole runtime package identity covered by the pinned owner authorization.
#[must_use]
pub fn is_owner_authorized_runtime(name: &str) -> bool {
    name == CLAUDE_AGENT_SDK_PACKAGE
}

/// Derives manifest locations from the pnpm workspace declaration.
#[must_use]
pub fn manifest_patterns(root_members: &[String]) -> Vec<String> {
    std::iter::once("package.json".to_owned())
        .chain(
            root_members
                .iter()
                .map(|member| format!("{member}/package.json")),
        )
        .chain(std::iter::once("examples/*/package.json".to_owned()))
        .collect()
}

/// Classifies direct external declarations by the runtime reachability of their area.
///
/// # Errors
/// Returns malformed dependency-section or requirement-string failures.
pub fn tier_external_deps<S: std::hash::BuildHasher>(
    manifests: &IndexMap<String, Manifest>,
    names: &HashSet<String, S>,
) -> anyhow::Result<IndexMap<String, bool>> {
    let mut tiers = IndexMap::from([("tsx".to_owned(), true)]);
    for (path, manifest) in manifests {
        let dev_only = DEV_ONLY_AREAS.iter().any(|area| {
            if area.ends_with('/') {
                path.starts_with(area)
            } else {
                path == area
            }
        });
        for kind in ALL_KINDS {
            for (dependency, range) in dependency_entries(manifest, kind)? {
                if names.contains(dependency) || range.starts_with("workspace:") {
                    continue;
                }
                let runtime = !dev_only && matches!(*kind, "dependencies" | "optionalDependencies");
                *tiers.entry(dependency.to_owned()).or_default() |= runtime;
            }
        }
    }
    Ok(tiers)
}

fn dependency_entries<'a>(
    manifest: &'a Manifest,
    kind: &str,
) -> anyhow::Result<Vec<(&'a str, &'a str)>> {
    let Some(value) = manifest.get(kind).filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let entries = value.as_object().context(format!(
        "gen-third-party-notices: {kind} must be an object."
    ))?;
    js_entries(entries.iter().map(|(name, value)| (name.as_str(), value)))
        .into_iter()
        .map(|(name, value)| {
            Ok((
                name,
                value.as_str().context(format!(
                    "gen-third-party-notices: {name} {kind} range must be a string."
                ))?,
            ))
        })
        .collect()
}

fn js_entries<'a, T>(entries: impl Iterator<Item = (&'a str, &'a T)>) -> Vec<(&'a str, &'a T)> {
    let mut entries = entries.collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| {
        key.parse::<u32>()
            .ok()
            .filter(|index| *index != u32::MAX && index.to_string() == *key)
            .map_or((1, 0), |index| (0, index))
    });
    entries
}

/// Reads a package from a pnpm store, including truncated peer-suffixed directories.
///
/// # Errors
/// Returns directory, manifest-read, or JSON parse failures, including a broken
/// ordinary prefix match rather than silently trying a different package copy.
pub fn virtual_manifest(virtual_store: &Path, name: &str) -> anyhow::Result<Option<Manifest>> {
    let entries = sorted_entries(virtual_store)?;
    let prefix = format!("{}@", name.replacen('/', "+", 1));
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
    {
        return read_manifest(
            &entry
                .path()
                .join("node_modules")
                .join(name)
                .join("package.json"),
        )
        .map(Some);
    }
    for entry in entries {
        let candidate = entry
            .path()
            .join("node_modules")
            .join(name)
            .join("package.json");
        if candidate.exists() {
            return read_manifest(&candidate).map(Some);
        }
    }
    Ok(None)
}

fn sorted_entries(path: &Path) -> anyhow::Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|a, b| {
        a.file_name()
            .to_string_lossy()
            .encode_utf16()
            .cmp(b.file_name().to_string_lossy().encode_utf16())
    });
    Ok(entries)
}

fn read_manifest(path: &Path) -> anyhow::Result<Manifest> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

/// Normalizes the repository/homepage forms accepted by npm manifests.
#[must_use]
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Repository URL suffixes follow the source's case-sensitive normalization."
)]
pub fn normalize_repo(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|raw| !raw.is_empty())?;
    let mut url = raw.to_owned();
    for (prefix, replacement) in [
        ("git+ssh://git@", "https://"),
        ("git+", ""),
        ("git://", "https://"),
        ("github:", "https://github.com/"),
    ] {
        if let Some(rest) = url.strip_prefix(prefix) {
            url = format!("{replacement}{rest}");
        }
    }
    if url.ends_with(".git") {
        url.truncate(url.len() - 4);
    }
    if !url.starts_with("http") {
        url = format!("https://github.com/{url}");
    }
    Some(url)
}

/// Parses complete rows from the pinned vendored-package table grammar.
///
/// # Panics
/// Panics if the embedded table grammar is invalid.
#[must_use]
pub fn parse_vendored_rows(text: &str) -> Vec<VendoredRow> {
    static ROW: OnceLock<Regex> = OnceLock::new();
    let pattern = ROW.get_or_init(|| Regex::new(&r"^\| `\S+/` \| `([^`]+)` \| `([^`]+)` \| \S+ \| (https://\S+?)(?: \([^)]*\))? \| `[0-9a-f]+` \|$".replace(r"\S", &format!("[^{JS_SPACE_BODY}]"))).expect("static vendored row pattern"));
    text.split('\n')
        .filter_map(|line| pattern.captures(line))
        .map(|capture| VendoredRow {
            npm_name: capture[1].to_owned(),
            upstream_name: capture[2].to_owned(),
            upstream: capture[3].to_owned(),
        })
        .collect()
}

fn printed(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_owned(), Value::to_string)
}

fn required_string(manifest: &Manifest, field: &str) -> anyhow::Result<String> {
    let value = manifest
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    value.map(str::to_owned).with_context(|| {
        format!("gen-third-party-notices: {CLAUDE_AGENT_SDK_PACKAGE} has no {field}.")
    })
}

/// Derives the platform namespace and versions from the installed SDK itself.
///
/// # Errors
/// Returns wrong-identity, missing-version, empty-payload, and unrelated-optional errors.
pub fn claude_distribution_from_manifest(
    manifest: &Manifest,
) -> anyhow::Result<ClaudeDistribution> {
    if manifest.get("name").and_then(Value::as_str) != Some(CLAUDE_AGENT_SDK_PACKAGE) {
        bail!(
            "gen-third-party-notices: expected {CLAUDE_AGENT_SDK_PACKAGE} manifest, got {}.",
            printed(manifest.get("name"))
        );
    }
    let sdk_version = required_string(manifest, "version")?;
    let claude_code_version = required_string(manifest, "claudeCodeVersion")?;
    let entries = dependency_entries(manifest, "optionalDependencies")?;
    if entries.is_empty() {
        bail!(
            "gen-third-party-notices: {CLAUDE_AGENT_SDK_PACKAGE} declares no optional platform payloads."
        );
    }
    let mut payloads = Vec::new();
    for (name, version) in entries {
        if !name.starts_with(&format!("{CLAUDE_AGENT_SDK_PACKAGE}-")) {
            bail!(
                "gen-third-party-notices: {CLAUDE_AGENT_SDK_PACKAGE} optional dependency {name} is outside its authorized platform-payload identity."
            );
        }
        if version.is_empty() {
            bail!(
                "gen-third-party-notices: {CLAUDE_AGENT_SDK_PACKAGE} has no {name} optional dependency version."
            );
        }
        payloads.push(ClaudePlatformPayload {
            name: name.to_owned(),
            version: version.to_owned(),
        });
    }
    payloads.sort_by(|a, b| locale_compare(&a.name, &b.name));
    Ok(ClaudeDistribution {
        sdk_version,
        claude_code_version,
        payloads,
    })
}
