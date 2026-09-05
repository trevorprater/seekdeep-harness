//! Shared profile paths, preparation, and boot-free config composition.

use std::path::{Path, PathBuf};

use path_clean::PathClean as _;
use seekdeep_app_boot::{
    ConfigDumpLayer, LoadProfileOptions, PROFILE_PATCH_FILENAME, Profile, load_optional_patches,
    load_overlay_patches, load_profile, render_config_dump_stderr,
};
use seekdeep_util::home_paths::{SEEKDEEP_HOME_ENV, resolve_process_seekdeep_home};

use crate::args::DumpConfigInvocation;

const NAME: &str = "seekdeep";
/// Empty root written before every profile composition.
pub const PROFILE_ROOT_FILENAME: &str = "cordis.yml";
const PROFILE_ROOT_CONFIG: &str = concat!(
    "# seekdeep profile root — an empty entry list. The tree is composed as patches:\n",
    "# each bundle in package.json's seekdeep.profile.bundles, then cordis.patch.yml, then any\n",
    "# --patch overlays. Edit cordis.patch.yml, not this file.\n",
    "[]\n",
);

const BUILTIN_BUNDLES: [(&str, &str); 3] = [
    (
        "@seekdeep-ai/seekdeep-base",
        include_str!("../../../packages/bundle/base/cordis.patch.yml"),
    ),
    (
        "@seekdeep-ai/seekdeep-headless",
        include_str!("../../../packages/bundle/headless/cordis.patch.yml"),
    ),
    (
        "@seekdeep-ai/seekdeep-web-app",
        include_str!("../../../packages/bundle/web-app/cordis.patch.yml"),
    ),
];

const EMBEDDED_INSTALLATION_DIR: &str = ".seekdeep-installation";

/// Materialized Rust installation manifest used as the installed bundle-resolution anchor.
#[must_use]
pub fn install_anchor(home: &Path) -> PathBuf {
    home.join("profiles")
        .join(EMBEDDED_INSTALLATION_DIR)
        .join("package.json")
}

/// Home-level patch applied over every profile.
///
/// # Errors
///
/// Returns Harness-home resolution failures.
pub fn home_patch_path() -> anyhow::Result<PathBuf> {
    let configured = std::env::var_os(SEEKDEEP_HOME_ENV);
    Ok(resolve_process_seekdeep_home(configured.as_deref())?.join(PROFILE_PATCH_FILENAME))
}

/// Loads one profile and rewrites its empty Loader root.
///
/// # Errors
///
/// Returns home, profile, bundle, patch, or root-write failures.
pub fn prepare_profile(name: &str, user_layer: bool) -> anyhow::Result<Profile> {
    let configured = std::env::var_os(SEEKDEEP_HOME_ENV);
    let home = resolve_process_seekdeep_home(configured.as_deref())?;
    prepare_profile_at(name, user_layer, &home, &install_anchor(&home))
}

/// Explicit-path profile preparation used by launchers and tests.
///
/// # Errors
///
/// Returns fallback, profile, bundle, patch, or root-write failures.
pub fn prepare_profile_at(
    name: &str,
    user_layer: bool,
    home: &Path,
    anchor: &Path,
) -> anyhow::Result<Profile> {
    ensure_builtin_profile_bundles(home)?;
    let profile = load_profile(NAME, name, anchor, home, LoadProfileOptions { user_layer })?;
    std::fs::write(profile.dir.join(PROFILE_ROOT_FILENAME), PROFILE_ROOT_CONFIG)?;
    Ok(profile)
}

/// Materializes the compiled shipped bundle assets into the managed installation anchor.
///
/// # Errors
///
/// Returns directory, manifest, or patch-write failures.
pub(crate) fn ensure_builtin_profile_bundles(home: &Path) -> anyhow::Result<()> {
    let root = home.join("profiles").join(EMBEDDED_INSTALLATION_DIR);
    std::fs::create_dir_all(&root)?;
    std::fs::write(
        root.join("package.json"),
        concat!(
            "{\n",
            "  \"name\": \"@seekdeep-ai/seekdeep-rust-installation\",\n",
            "  \"private\": true,\n",
            "  \"dependencies\": {}\n",
            "}\n",
        ),
    )?;
    for (package_name, patch) in BUILTIN_BUNDLES {
        let target = root.join("node_modules").join(package_name);
        std::fs::create_dir_all(&target)?;
        std::fs::write(
            target.join("package.json"),
            format!(
                concat!(
                    "{{\n",
                    "  \"name\": {},\n",
                    "  \"private\": true,\n",
                    "  \"version\": {},\n",
                    "  \"seekdeep\": {{ \"bundle\": {{ \"patch\": \"./cordis.patch.yml\" }} }}\n",
                    "}}\n"
                ),
                serde_json::to_string(package_name)?,
                serde_json::to_string(env!("CARGO_PKG_VERSION"))?,
            ),
        )?;
        std::fs::write(target.join("cordis.patch.yml"), patch)?;
    }
    Ok(())
}

/// Composes one profile dump without booting or evaluating expressions.
///
/// # Errors
///
/// Returns profile, patch, current-directory, or rendering failures.
pub fn dump_profile_config(invocation: &DumpConfigInvocation) -> anyhow::Result<String> {
    let configured = std::env::var_os(SEEKDEEP_HOME_ENV);
    let home = resolve_process_seekdeep_home(configured.as_deref())?;
    let cwd = std::env::current_dir()?;
    dump_profile_config_at(invocation, &home, &install_anchor(&home), &cwd)
}

/// Explicit-path config dump used by process and differential tests.
///
/// # Errors
///
/// Returns profile, patch, or rendering failures.
fn dump_profile_config_at(
    invocation: &DumpConfigInvocation,
    home: &Path,
    anchor: &Path,
    cwd: &Path,
) -> anyhow::Result<String> {
    let profile = prepare_profile_at(
        invocation.profile.as_str(),
        !invocation.default_only,
        home,
        anchor,
    )?;
    let mut layers = profile
        .layers
        .iter()
        .map(|layer| ConfigDumpLayer {
            label: layer.package_name.clone(),
            patches: layer.patches.clone(),
        })
        .collect::<Vec<_>>();
    if !invocation.default_only {
        if profile.patch_path.exists() {
            layers.push(ConfigDumpLayer {
                label: profile.patch_path.to_string_lossy().into_owned(),
                patches: profile.patches.clone(),
            });
        }
        let home_patch = home.join(PROFILE_PATCH_FILENAME);
        if let Some(patches) = load_optional_patches(NAME, &home_patch)? {
            layers.push(ConfigDumpLayer {
                label: home_patch.to_string_lossy().into_owned(),
                patches,
            });
        }
        for file in &invocation.patches {
            let absolute = cwd.join(file).clean();
            layers.push(ConfigDumpLayer {
                label: absolute.to_string_lossy().into_owned(),
                patches: load_overlay_patches(NAME, &absolute)?,
            });
        }
    }
    render_config_dump_stderr(NAME, &profile.dir.join(PROFILE_ROOT_FILENAME), &layers)
}
