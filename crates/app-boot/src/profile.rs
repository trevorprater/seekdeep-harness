//! Profile paths, manifests, templates, optional patches, and pure composition.

use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use indexmap::IndexMap;
use seekdeep_loader::profile_patch::{
    ProfileComposition, ProfilePatch, ProfilePatchError, compose_profile_layers,
    parse_patch_list_yaml,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Directory below the Harness home containing every profile.
pub const PROFILES_DIR: &str = "profiles";
/// User patch layer within one profile directory.
pub const PROFILE_PATCH_FILENAME: &str = "cordis.patch.yml";

/// Validated profile identity crossing launcher and filesystem boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    /// Validates one profile name.
    ///
    /// # Errors
    ///
    /// Rejects empty, traversal, separator, and reserved fallback names.
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        anyhow::ensure!(
            !value.is_empty()
                && !value.contains('/')
                && !value.contains('\\')
                && !matches!(value.as_str(), "." | ".." | "node_modules"),
            "seekdeep: invalid profile name {}",
            serde_json::to_string(&value)?
        );
        Ok(Self(value))
    }

    /// Exact profile directory basename.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Bundle metadata exported from a package manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekDeepBundleManifest {
    /// Patch path relative to the bundle package root.
    pub patch: String,
    /// Future manifest fields preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Ordered bundle list selected by one profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekDeepProfileManifest {
    /// Bundle package names in layer order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundles: Option<Vec<String>>,
    /// Future manifest fields preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// SeekDeep-owned section of package metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekDeepManifestSection {
    /// Bundle-export metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<SeekDeepBundleManifest>,
    /// Profile-composition metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<SeekDeepProfileManifest>,
    /// Future `SeekDeep` metadata preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Package manifest fields consumed by profile boot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileManifest {
    /// Optional npm package name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Ordinary package dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<IndexMap<String, String>>,
    /// Peer dependencies participating in fallback healing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_dependencies: Option<IndexMap<String, String>>,
    /// `SeekDeep` profile/bundle metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seekdeep: Option<SeekDeepManifestSection>,
    /// Every unrelated package manifest field.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One resolved bundle patch layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileLayer {
    /// Bundle package name from the profile manifest.
    pub package_name: String,
    /// Package directory resolved from the installation or profile anchor.
    pub package_dir: PathBuf,
    /// Absolute bundle patch file.
    pub patch_path: PathBuf,
    /// Parsed patch list.
    pub patches: Vec<ProfilePatch>,
}

/// Loaded profile with ordered bundle layers and its optional user layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Validated profile identity.
    pub name: ProfileName,
    /// Absolute profile directory.
    pub dir: PathBuf,
    /// Bundle layers in manifest order.
    pub layers: Vec<ProfileLayer>,
    /// Profile-owned patch path.
    pub patch_path: PathBuf,
    /// Parsed user patches, empty when absent or deliberately skipped.
    pub patches: Vec<ProfilePatch>,
}

/// Profile loading choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadProfileOptions {
    /// Whether the profile's user patch layer is read.
    pub user_layer: bool,
}

impl Default for LoadProfileOptions {
    fn default() -> Self {
        Self { user_layer: true }
    }
}

/// Shipped profile templates by name.
pub static PROFILE_TEMPLATES: std::sync::LazyLock<IndexMap<&'static str, &'static [&'static str]>> =
    std::sync::LazyLock::new(|| {
        IndexMap::from([
            (
                "web",
                &[
                    "@seekdeep-ai/seekdeep-base",
                    "@seekdeep-ai/seekdeep-web-app",
                ][..],
            ),
            (
                "headless",
                &[
                    "@seekdeep-ai/seekdeep-base",
                    "@seekdeep-ai/seekdeep-headless",
                ][..],
            ),
        ])
    });

static INSTALLATION_OWNED_PROFILE_TUPLES: std::sync::LazyLock<
    IndexMap<&'static str, &'static [&'static str]>,
> = std::sync::LazyLock::new(|| {
    IndexMap::from([(
        "headless",
        &[
            "@seekdeep-ai/seekdeep-base",
            "@seekdeep-ai/seekdeep-web-app",
            "@seekdeep-ai/seekdeep-headless",
        ][..],
    )])
});

/// Bundle list for a custom profile initialized without a shipped template.
pub const DEFAULT_PROFILE_BUNDLES: &[&str] = &["@seekdeep-ai/seekdeep-base"];

const PROFILE_PATCH_TEMPLATE: &str = concat!(
    "# Your patch layer for this seekdeep profile, applied after every bundle layer:\n",
    "# a top-level YAML array of loader patch entries (id-targeted config\n",
    "# overrides, disables, and insert lists; `!!js` expressions allowed).\n",
    "[]\n",
);

const PROFILE_PNPM_WORKSPACE: &str = concat!(
    "packages:\n",
    "  - .\n\n",
    "nodeLinker: hoisted\n",
    "autoInstallPeers: false\n",
);

/// Resolves one profile directory below an explicit Harness home.
///
/// # Errors
///
/// Rejects an invalid profile name before joining filesystem paths.
pub fn resolve_profile_dir(name: &str, home: &Path) -> anyhow::Result<PathBuf> {
    let name = ProfileName::new(name)?;
    Ok(home.join(PROFILES_DIR).join(name.as_str()))
}

fn write_new(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(content)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Initializes a profile without changing any existing file.
///
/// # Errors
///
/// Returns directory or file creation failures.
pub fn init_profile(dir: &Path, bundles: &[&str]) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    let profile_name = dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| anyhow::anyhow!("profile directory has no UTF-8 basename"))?;
    let manifest = json!({
        "name": format!("seekdeep-profile-{profile_name}"),
        "private": true,
        "dependencies": {},
        "seekdeep": {"profile": {"bundles": bundles}},
    });
    let mut manifest = serde_json::to_string_pretty(&manifest)?;
    manifest.push('\n');
    write_new(&dir.join("package.json"), manifest.as_bytes())?;
    write_new(
        &dir.join(PROFILE_PATCH_FILENAME),
        PROFILE_PATCH_TEMPLATE.as_bytes(),
    )?;
    write_new(
        &dir.join("pnpm-workspace.yaml"),
        PROFILE_PNPM_WORKSPACE.as_bytes(),
    )?;
    Ok(())
}

/// Reads and validates a profile manifest object.
///
/// # Errors
///
/// Returns read, JSON, or top-level-shape failures with the launcher prefix.
pub fn read_profile_manifest(bin_name: &str, dir: &Path) -> anyhow::Result<ProfileManifest> {
    let path = dir.join("package.json");
    let raw = fs::read_to_string(&path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read profile manifest {}: {error}",
            path.display()
        )
    })?;
    let value: Value = serde_json::from_str(&raw)?;
    anyhow::ensure!(
        value.is_object(),
        "{bin_name}: profile manifest {} must hold a JSON object",
        path.display()
    );
    Ok(serde_json::from_value(value)?)
}

/// Writes a profile manifest as two-space JSON with one trailing newline.
///
/// # Errors
///
/// Returns serialization or write failures.
pub fn write_profile_manifest(dir: &Path, manifest: &ProfileManifest) -> anyhow::Result<()> {
    let mut output = serde_json::to_string_pretty(manifest)?;
    output.push('\n');
    fs::write(dir.join("package.json"), output)?;
    Ok(())
}

fn same_bundles(left: &[String], right: &[&str]) -> bool {
    left.iter().map(String::as_str).eq(right.iter().copied())
}

/// Normalizes the one installation-owned legacy tuple to the current template.
///
/// Every other bundle list is user-owned and remains byte-semantically unchanged.
///
/// # Errors
///
/// Returns a manifest write failure when normalization is required.
pub fn normalize_shipped_profile(
    name: &str,
    dir: &Path,
    mut manifest: ProfileManifest,
) -> anyhow::Result<ProfileManifest> {
    let Some(installation_owned) = INSTALLATION_OWNED_PROFILE_TUPLES.get(name) else {
        return Ok(manifest);
    };
    let Some(current) = PROFILE_TEMPLATES.get(name) else {
        return Ok(manifest);
    };
    let Some(profile) = manifest
        .seekdeep
        .as_mut()
        .and_then(|section| section.profile.as_mut())
    else {
        return Ok(manifest);
    };
    let Some(bundles) = profile.bundles.as_ref() else {
        return Ok(manifest);
    };
    if !same_bundles(bundles, installation_owned) {
        return Ok(manifest);
    }
    profile.bundles = Some(current.iter().map(|bundle| (*bundle).to_owned()).collect());
    write_profile_manifest(dir, &manifest)?;
    Ok(manifest)
}

/// Loads an optional profile/home/CLI patch file.
///
/// # Errors
///
/// A present unreadable, unparsable, or malformed layer fails loudly.
pub fn load_optional_patches(
    bin_name: &str,
    path: &Path,
) -> anyhow::Result<Option<Vec<ProfilePatch>>> {
    if !path.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read patches {}: {error}",
            path.display()
        )
    })?;
    parse_patch_list_yaml(&source).map(Some).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to parse patches {}: {error}",
            path.display()
        )
    })
}

/// Composes ordered patch layers over an empty entry root.
///
/// # Errors
///
/// Returns the first malformed patch failure after delivering earlier warnings.
pub fn compose_entries(
    layers: &[Vec<ProfilePatch>],
) -> Result<ProfileComposition, ProfilePatchError> {
    compose_profile_layers(layers)
}

fn package_dir_from_anchor(anchor: &Path, package_name: &str) -> Option<PathBuf> {
    let mut current = if anchor.is_dir() {
        anchor
    } else {
        anchor.parent()?
    };
    loop {
        let candidate = current.join("node_modules").join(package_name);
        if candidate.join("package.json").exists() {
            return Some(candidate);
        }
        current = current.parent()?;
    }
}

/// Resolves a bundle from the installation first and profile second.
///
/// Resolution probes `node_modules` directories directly, so a package need
/// not export `./package.json`.
///
/// # Errors
///
/// Returns installation guidance when neither anchor resolves.
pub fn resolve_bundle_dir(
    bin_name: &str,
    package_name: &str,
    install_anchor: &Path,
    profile_dir: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(dir) = package_dir_from_anchor(install_anchor, package_name)
        .or_else(|| package_dir_from_anchor(&profile_dir.join("package.json"), package_name))
    {
        return Ok(dir);
    }
    let profile = profile_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("profile");
    anyhow::bail!(
        "{bin_name}: cannot resolve profile bundle {} from the seekdeep installation or {}; run 'seekdeep plugin --profile {profile} install' if its dependency is not installed",
        serde_json::to_string(package_name)?,
        profile_dir.display()
    )
}

fn load_required_patches(bin_name: &str, path: &Path) -> anyhow::Result<Vec<ProfilePatch>> {
    let source = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read patches {}: {error}",
            path.display()
        )
    })?;
    parse_patch_list_yaml(&source).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to parse patches {}: {error}",
            path.display()
        )
    })
}

/// Loads one profile and all ordered bundle patch layers.
///
/// # Errors
///
/// Returns profile initialization, manifest, resolution, metadata, or patch failures.
pub fn load_profile(
    bin_name: &str,
    name: &str,
    install_anchor: &Path,
    home: &Path,
    options: LoadProfileOptions,
) -> anyhow::Result<Profile> {
    let name = ProfileName::new(name)?;
    let dir = resolve_profile_dir(name.as_str(), home)?;
    if !dir.join("package.json").exists() {
        let Some(template) = PROFILE_TEMPLATES.get(name.as_str()) else {
            anyhow::bail!(
                "{bin_name}: profile {} does not exist; create it with 'seekdeep plugin --profile {} add <package>'",
                serde_json::to_string(name.as_str())?,
                name.as_str()
            );
        };
        init_profile(&dir, template)?;
    }
    let manifest =
        normalize_shipped_profile(name.as_str(), &dir, read_profile_manifest(bin_name, &dir)?)?;
    let bundles = manifest
        .seekdeep
        .as_ref()
        .and_then(|section| section.profile.as_ref())
        .and_then(|profile| profile.bundles.as_ref())
        .cloned()
        .unwrap_or_default();
    let mut layers = Vec::with_capacity(bundles.len());
    for package_name in bundles {
        let package_dir = resolve_bundle_dir(bin_name, &package_name, install_anchor, &dir)?;
        let bundle_manifest: ProfileManifest =
            serde_json::from_str(&fs::read_to_string(package_dir.join("package.json"))?)?;
        let declared = bundle_manifest
            .seekdeep
            .as_ref()
            .and_then(|section| section.bundle.as_ref())
            .map(|bundle| bundle.patch.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{bin_name}: profile bundle {} declares no seekdeep.bundle in its package.json",
                    serde_json::to_string(&package_name)
                        .unwrap_or_else(|_| format!("{package_name:?}"))
                )
            })?;
        let patch_path = package_dir.join(declared);
        let patches = load_required_patches(bin_name, &patch_path)?;
        layers.push(ProfileLayer {
            package_name,
            package_dir,
            patch_path,
            patches,
        });
    }
    let patch_path = dir.join(PROFILE_PATCH_FILENAME);
    let patches = if options.user_layer {
        load_optional_patches(bin_name, &patch_path)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(Profile {
        name,
        dir,
        layers,
        patch_path,
        patches,
    })
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn ensure_symlink(link: &Path, target: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(link) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_symlink(),
                "seekdeep: {} exists and is not a symlink; remove it so seekdeep can manage the installation fallback",
                link.display()
            );
            if fs::read_link(link)? == target {
                return Ok(());
            }
            fs::remove_file(link)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match create_directory_link(target, link) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(link)?;
            if metadata.file_type().is_symlink() && fs::read_link(link)? == target {
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Maintains the flat installation package fallback below `profiles/node_modules`.
///
/// Traversal is breadth-first over dependencies and peer dependencies; first
/// resolution wins like Node's nearest-module lookup.
///
/// # Errors
///
/// Returns manifest, directory, symlink, or conflicting-real-entry failures.
pub fn heal_profiles_module_fallback(install_anchor: &Path, home: &Path) -> anyhow::Result<()> {
    let modules_dir = home.join(PROFILES_DIR).join("node_modules");
    fs::create_dir_all(&modules_dir)?;
    let app_manifest: ProfileManifest = serde_json::from_str(&fs::read_to_string(install_anchor)?)?;
    let mut links: IndexMap<String, PathBuf> = IndexMap::new();
    if let Some(name) = &app_manifest.name {
        links.insert(
            name.clone(),
            install_anchor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("install anchor has no parent"))?
                .to_owned(),
        );
    }
    let mut queue = VecDeque::from([(install_anchor.to_owned(), app_manifest)]);
    while let Some((anchor, manifest)) = queue.pop_front() {
        let dependencies = manifest
            .dependencies
            .iter()
            .flatten()
            .chain(manifest.peer_dependencies.iter().flatten())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for dependency in dependencies {
            if links.contains_key(&dependency) {
                continue;
            }
            let Some(dir) = package_dir_from_anchor(&anchor, &dependency) else {
                continue;
            };
            let manifest_path = dir.join("package.json");
            let manifest: ProfileManifest =
                serde_json::from_str(&fs::read_to_string(&manifest_path)?)?;
            links.insert(dependency, dir);
            queue.push_back((manifest_path, manifest));
        }
    }
    for (package_name, target) in links {
        let link = modules_dir.join(package_name);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        ensure_symlink(&link, &target)?;
    }
    Ok(())
}
