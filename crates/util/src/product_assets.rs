//! Locates product assets that ship outside the executable.
//!
//! The built web frontend and the shipped agent presets are npm-packaged data: the source
//! resolved them through Node's module lookup from the package that owned them. The
//! compiled executable has no module graph, so each asset is searched in this order:
//!
//! 1. an explicit launch-environment variable naming its directory;
//! 2. `node_modules` lookups walking up from the installation anchor (the materialized
//!    installation manifest below the Harness home);
//! 3. the same lookup walking up from the executable, which finds the packages an npm
//!    install hoists next to the platform package carrying the executable;
//! 4. a directory adjacent to the executable, for packaged layouts without `node_modules`;
//! 5. the source checkout the executable was built from, for development builds.

use std::path::{Path, PathBuf};

use path_clean::PathClean as _;

use crate::launch_environment::LaunchEnvironmentSnapshot;

/// Launch-environment variable naming the directory holding the built web frontend.
pub const WEB_FRONTEND_DIST_ENV: &str = "SEEKDEEP_WEB_DIST";

/// Launch-environment variable naming the shipped agent-preset directory.
pub const AGENT_PRESET_DIR_ENV: &str = "SEEKDEEP_AGENT_PRESET_DIR";

/// Directory below the executable's neighbours that packaged layouts use.
const PACKAGED_LIBRARY_DIRECTORY: &str = "lib/seekdeep";

/// One asset and every place it may live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetSpec {
    /// Launch-environment variable naming the asset directory explicitly.
    pub environment: &'static str,
    /// Path of the asset below the directory the variable names (empty for the directory).
    pub environment_path: &'static str,
    /// npm package that ships the asset.
    pub package: &'static str,
    /// Path of the asset below the package directory.
    pub package_path: &'static str,
    /// Path of the asset below a directory adjacent to the executable.
    pub adjacent: &'static str,
    /// Path of the asset below the source checkout root.
    pub checkout: &'static str,
}

/// The built web frontend's index document.
pub const WEB_FRONTEND_INDEX: AssetSpec = AssetSpec {
    environment: WEB_FRONTEND_DIST_ENV,
    environment_path: "index.html",
    package: "@seekdeep-ai/seekdeep-web-frontend",
    package_path: "dist/index.html",
    adjacent: "web/index.html",
    checkout: "apps/web/dist/index.html",
};

/// The shipped agent-preset directory.
pub const SHIPPED_AGENT_PRESETS: AssetSpec = AssetSpec {
    environment: AGENT_PRESET_DIR_ENV,
    environment_path: "",
    package: "@seekdeep-ai/seekdeep",
    package_path: "config/agent-presets",
    adjacent: "agent-presets",
    checkout: "apps/cli/config/agent-presets",
};

/// Places one lookup starts from; every anchor is optional.
#[derive(Clone, Copy, Debug, Default)]
pub struct AssetAnchors<'a> {
    /// Frozen launch environment; the process environment when absent.
    pub environment: Option<&'a LaunchEnvironmentSnapshot>,
    /// Installation manifest (or any file/directory) whose `node_modules` chain is searched.
    pub install_anchor: Option<&'a Path>,
    /// The running executable.
    pub executable: Option<&'a Path>,
    /// Root of the source checkout the executable was built from.
    pub checkout: Option<&'a Path>,
}

/// Resolves the first existing location of one asset.
#[must_use]
pub fn resolve(spec: &AssetSpec, anchors: &AssetAnchors<'_>) -> Option<PathBuf> {
    candidates(spec, anchors)
        .into_iter()
        .find(|candidate| candidate.exists())
}

/// Every location the lookup would try, in search order and without duplicates.
#[must_use]
pub fn candidates(spec: &AssetSpec, anchors: &AssetAnchors<'_>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push = |candidate: PathBuf| {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(directory) = environment_directory(spec.environment, anchors.environment) {
        push(join_relative(&directory, spec.environment_path));
    }
    if let Some(anchor) = anchors.install_anchor {
        for package in package_directories(anchor, spec.package) {
            push(package.join(spec.package_path));
        }
    }
    if let Some(executable) = anchors.executable {
        for package in package_directories(executable, spec.package) {
            push(package.join(spec.package_path));
        }
        if let Some(directory) = executable.parent() {
            push(directory.join(spec.adjacent));
            push(directory.join("..").join(spec.adjacent).clean());
            push(
                directory
                    .join(PACKAGED_LIBRARY_DIRECTORY)
                    .join(spec.adjacent),
            );
            push(
                directory
                    .join("..")
                    .join(PACKAGED_LIBRARY_DIRECTORY)
                    .join(spec.adjacent)
                    .clean(),
            );
        }
    }
    if let Some(checkout) = anchors.checkout {
        push(checkout.join(spec.checkout));
    }
    candidates
}

/// The directory a launch-environment variable names, absent when unset or empty.
fn environment_directory(
    name: &str,
    environment: Option<&LaunchEnvironmentSnapshot>,
) -> Option<PathBuf> {
    let value = match environment {
        Some(environment) => environment.get(name).map(|entry| entry.value),
        None => std::env::var(name).ok(),
    }?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn join_relative(directory: &Path, relative: &str) -> PathBuf {
    if relative.is_empty() {
        directory.to_owned()
    } else {
        directory.join(relative)
    }
}

/// Every `node_modules/<package>` directory found walking up from the anchor.
///
/// Mirrors Node's nearest-module lookup: the anchor's own directory first (or its parent
/// when the anchor is a file), then each ancestor.
fn package_directories(anchor: &Path, package: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut current = if anchor.is_dir() {
        Some(anchor)
    } else {
        anchor.parent()
    };
    while let Some(directory) = current {
        let candidate = directory.join("node_modules").join(package);
        if candidate.join("package.json").is_file() {
            found.push(candidate);
        }
        current = directory.parent();
    }
    found
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use super::{
        AGENT_PRESET_DIR_ENV, AssetAnchors, SHIPPED_AGENT_PRESETS, WEB_FRONTEND_DIST_ENV,
        WEB_FRONTEND_INDEX, candidates, resolve,
    };
    use crate::launch_environment::{
        LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
        create_launch_environment_snapshot,
    };

    fn environment(entries: &[(&str, &str)]) -> LaunchEnvironmentSnapshot {
        create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: entries
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }])
    }

    fn package(root: &Path, name: &str, asset: &str) -> std::path::PathBuf {
        let directory = root.join("node_modules").join(name);
        std::fs::create_dir_all(directory.join(asset).parent().unwrap()).unwrap();
        std::fs::write(directory.join("package.json"), "{}").unwrap();
        std::fs::write(directory.join(asset), asset).unwrap();
        directory.join(asset)
    }

    #[test]
    fn the_environment_variable_wins_and_an_empty_value_is_unset() {
        let temporary = tempfile::tempdir().unwrap();
        let dist = temporary.path().join("explicit");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.html"), "explicit").unwrap();
        let checkout = temporary.path().join("checkout");
        std::fs::create_dir_all(checkout.join("apps/web/dist")).unwrap();
        std::fs::write(checkout.join("apps/web/dist/index.html"), "checkout").unwrap();
        let explicit = environment(&[(WEB_FRONTEND_DIST_ENV, dist.to_str().unwrap())]);
        let anchors = AssetAnchors {
            environment: Some(&explicit),
            checkout: Some(&checkout),
            ..AssetAnchors::default()
        };
        assert_eq!(
            resolve(&WEB_FRONTEND_INDEX, &anchors),
            Some(dist.join("index.html"))
        );
        let empty = environment(&[(WEB_FRONTEND_DIST_ENV, "")]);
        let anchors = AssetAnchors {
            environment: Some(&empty),
            ..anchors
        };
        assert_eq!(
            resolve(&WEB_FRONTEND_INDEX, &anchors),
            Some(checkout.join("apps/web/dist/index.html"))
        );
    }

    #[test]
    fn the_installation_anchor_is_searched_before_the_executable_and_the_checkout() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary
            .path()
            .join("home/profiles/.seekdeep-installation");
        std::fs::create_dir_all(&home).unwrap();
        let anchor = home.join("package.json");
        std::fs::write(&anchor, "{}").unwrap();
        let installed = package(
            &temporary.path().join("home/profiles"),
            "@seekdeep-ai/seekdeep-web-frontend",
            "dist/index.html",
        );
        let hoisted = package(
            temporary.path(),
            "@seekdeep-ai/seekdeep-web-frontend",
            "dist/index.html",
        );
        let executable = temporary
            .path()
            .join("node_modules/@seekdeep-ai/seekdeep-linux-x64/bin/seekdeep");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "").unwrap();
        let none = environment(&[]);
        let anchors = AssetAnchors {
            environment: Some(&none),
            install_anchor: Some(&anchor),
            executable: Some(&executable),
            checkout: Some(temporary.path()),
        };
        assert_eq!(resolve(&WEB_FRONTEND_INDEX, &anchors), Some(installed));
        std::fs::remove_dir_all(temporary.path().join("home/profiles/node_modules")).unwrap();
        assert_eq!(resolve(&WEB_FRONTEND_INDEX, &anchors), Some(hoisted));
    }

    #[test]
    fn packaged_layouts_place_assets_next_to_the_executable() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("bin/seekdeep");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "").unwrap();
        let none = environment(&[]);
        let anchors = AssetAnchors {
            environment: Some(&none),
            executable: Some(&executable),
            ..AssetAnchors::default()
        };
        assert_eq!(resolve(&SHIPPED_AGENT_PRESETS, &anchors), None);
        let library = temporary.path().join("lib/seekdeep/agent-presets");
        std::fs::create_dir_all(&library).unwrap();
        assert_eq!(resolve(&SHIPPED_AGENT_PRESETS, &anchors), Some(library));
        let adjacent = temporary.path().join("bin/agent-presets");
        std::fs::create_dir_all(&adjacent).unwrap();
        assert_eq!(resolve(&SHIPPED_AGENT_PRESETS, &anchors), Some(adjacent));
        let listed = candidates(&SHIPPED_AGENT_PRESETS, &anchors);
        assert_eq!(listed[0], temporary.path().join("bin/agent-presets"));
        assert!(listed.contains(&temporary.path().join("agent-presets")));
        assert!(listed.contains(&temporary.path().join("bin/lib/seekdeep/agent-presets")));
        assert!(listed.contains(&temporary.path().join("lib/seekdeep/agent-presets")));
    }

    #[test]
    fn the_preset_variable_names_the_directory_itself() {
        let temporary = tempfile::tempdir().unwrap();
        let presets = temporary.path().join("presets");
        std::fs::create_dir_all(&presets).unwrap();
        let explicit = environment(&[(AGENT_PRESET_DIR_ENV, presets.to_str().unwrap())]);
        let anchors = AssetAnchors {
            environment: Some(&explicit),
            ..AssetAnchors::default()
        };
        assert_eq!(resolve(&SHIPPED_AGENT_PRESETS, &anchors), Some(presets));
    }
}
