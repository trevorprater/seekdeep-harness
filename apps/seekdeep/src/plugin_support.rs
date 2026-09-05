//! Profile package-manager forwarding and installed bundle reconciliation.

use std::{collections::HashSet, ffi::OsString, path::Path, process::Command};

use path_clean::PathClean as _;
use seekdeep_app_boot::{
    DEFAULT_PROFILE_BUNDLES, PROFILE_TEMPLATES, ProfileManifest, init_profile,
    read_profile_manifest, resolve_bundle_dir, resolve_profile_dir, write_profile_manifest,
};
use seekdeep_util::home_paths::{SEEKDEEP_HOME_ENV, resolve_process_seekdeep_home};

use crate::{
    args::PluginInvocation,
    profile_support::{ensure_builtin_profile_bundles, install_anchor},
};

const NAME: &str = "seekdeep";

fn pnpm_spawn_spec(arguments: &[String], windows: bool) -> (OsString, Vec<OsString>) {
    if windows {
        let arguments = ["/d", "/v:off", "/s", "/c", "pnpm"]
            .into_iter()
            .map(OsString::from)
            .chain(arguments.iter().map(OsString::from))
            .collect();
        (OsString::from("cmd.exe"), arguments)
    } else {
        (
            OsString::from("pnpm"),
            arguments.iter().map(OsString::from).collect(),
        )
    }
}

/// Anchors a relative pnpm filesystem spec at the invoking directory.
#[must_use]
fn anchor_path_spec(argument: &str, cwd: &Path) -> String {
    let (prefix, path) = ["file:", "link:"]
        .into_iter()
        .find_map(|prefix| argument.strip_prefix(prefix).map(|path| (prefix, path)))
        .unwrap_or(("", argument));
    let relative = matches!(path, "." | "..")
        || path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with(".\\")
        || path.starts_with("..\\");
    if !relative {
        return argument.to_owned();
    }
    format!("{prefix}{}", cwd.join(path).clean().to_string_lossy())
}

fn exports_patch(package_name: &str, profile_dir: &Path, anchor: &Path) -> anyhow::Result<bool> {
    let Ok(directory) = resolve_bundle_dir(NAME, package_name, anchor, profile_dir) else {
        return Ok(false);
    };
    Ok(read_profile_manifest(NAME, &directory)?
        .seekdeep
        .and_then(|manifest| manifest.bundle)
        .is_some())
}

#[cfg(test)]
fn reconcile_plugins(
    before: &ProfileManifest,
    profile_dir: &Path,
    anchor: &Path,
) -> anyhow::Result<Vec<String>> {
    let mut warnings = Vec::new();
    reconcile_plugins_with_warning_sink(before, profile_dir, anchor, |package_name| {
        warnings.push(package_name.to_owned());
    })?;
    Ok(warnings)
}

fn reconcile_plugins_with_warning_sink(
    before: &ProfileManifest,
    profile_dir: &Path,
    anchor: &Path,
    mut warn: impl FnMut(&str),
) -> anyhow::Result<()> {
    let mut after = read_profile_manifest(NAME, profile_dir)?;
    let before_dependencies = before
        .dependencies
        .as_ref()
        .map(|dependencies| dependencies.keys().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let dependencies = after
        .dependencies
        .as_ref()
        .map(|dependencies| dependencies.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut bundles = after
        .seekdeep
        .as_ref()
        .and_then(|section| section.profile.as_ref())
        .and_then(|profile| profile.bundles.clone())
        .unwrap_or_default();
    let mut changed = false;
    for package_name in &dependencies {
        let is_bundle = exports_patch(package_name, profile_dir, anchor)?;
        if is_bundle && !bundles.contains(package_name) {
            bundles.push(package_name.clone());
            changed = true;
        } else if !is_bundle && !before_dependencies.contains(package_name) {
            warn(package_name);
        }
    }
    let dependency_set = dependencies.iter().cloned().collect::<HashSet<_>>();
    let mut retained = Vec::with_capacity(bundles.len());
    for package_name in bundles {
        let managed =
            before_dependencies.contains(&package_name) || dependency_set.contains(&package_name);
        let keep = !managed
            || (dependency_set.contains(&package_name)
                && exports_patch(&package_name, profile_dir, anchor)?);
        changed |= !keep;
        if keep {
            retained.push(package_name);
        }
    }
    if changed {
        after
            .seekdeep
            .get_or_insert_default()
            .profile
            .get_or_insert_default()
            .bundles = Some(retained);
        write_profile_manifest(profile_dir, &after)?;
    }
    Ok(())
}

/// Runs one profile plugin-management invocation.
///
/// # Errors
///
/// Returns home, profile initialization, process-spawn, or reconciliation failures.
pub fn run_plugin(invocation: &PluginInvocation) -> anyhow::Result<i32> {
    let configured = std::env::var_os(SEEKDEEP_HOME_ENV);
    let home = resolve_process_seekdeep_home(configured.as_deref())?;
    let cwd = std::env::current_dir()?;
    run_plugin_at(invocation, &home, &install_anchor(&home), &cwd)
}

fn run_plugin_at(
    invocation: &PluginInvocation,
    home: &Path,
    anchor: &Path,
    cwd: &Path,
) -> anyhow::Result<i32> {
    ensure_builtin_profile_bundles(home)?;
    let profile_dir = resolve_profile_dir(invocation.profile.as_str(), home)?;
    if !profile_dir.join("package.json").exists() {
        let bundles = PROFILE_TEMPLATES
            .get(invocation.profile.as_str())
            .copied()
            .unwrap_or(DEFAULT_PROFILE_BUNDLES);
        init_profile(&profile_dir, bundles)?;
        eprintln!(
            "{NAME}: initialized profile {} at {}",
            invocation.profile,
            profile_dir.display()
        );
    }
    let before = read_profile_manifest(NAME, &profile_dir)?;
    let arguments = invocation
        .args
        .iter()
        .map(|argument| anchor_path_spec(argument, cwd))
        .collect::<Vec<_>>();
    let (executable, arguments) = pnpm_spawn_spec(&arguments, cfg!(windows));
    let status = match Command::new(executable)
        .args(arguments)
        .current_dir(&profile_dir)
        .status()
    {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{NAME}: pnpm not found on PATH — install pnpm to manage profile plugins");
            return Ok(127);
        }
        Err(error) => return Err(error.into()),
    };
    let code = status.code().unwrap_or(1);
    if code == 0 {
        reconcile_plugins_with_warning_sink(&before, &profile_dir, anchor, |package_name| {
            eprintln!(
                "{NAME}: warning: {package_name} declares no seekdeep.bundle — installed as a plain dependency, not a profile layer (a later update that gains one activates it automatically)"
            );
        })?;
    } else {
        eprintln!(
            "{NAME}: pnpm failed in profile directory {}",
            profile_dir.display()
        );
        if invocation.args.iter().any(|argument| is_git_spec(argument)) {
            eprintln!(
                "{NAME}: git-hosted plugins build on install via their prepare script, which pnpm blocks until allowed — add the exact key pnpm printed above under allowBuilds in {}, then re-run",
                profile_dir.join("pnpm-workspace.yaml").display()
            );
        }
    }
    Ok(code)
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_git_spec(argument: &str) -> bool {
    argument.starts_with("git+")
        || argument.starts_with("github:")
        || argument.contains(".git#")
        || argument.ends_with(".git")
}

#[cfg(test)]
mod tests {
    use seekdeep_app_boot::{
        SeekDeepBundleManifest, SeekDeepManifestSection, SeekDeepProfileManifest,
    };
    use serde_json::Map;

    use super::*;

    #[test]
    fn anchors_only_relative_filesystem_specs_without_changing_prefix_semantics() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("work/plugin");
        assert_eq!(anchor_path_spec(".", &cwd), cwd.to_string_lossy());
        assert_eq!(
            anchor_path_spec("../other", &cwd),
            root.path().join("work/other").to_string_lossy()
        );
        assert_eq!(
            anchor_path_spec("file:./local", &cwd),
            format!("file:{}", cwd.join("local").display())
        );
        assert_eq!(
            anchor_path_spec("link:../shared", &cwd),
            format!("link:{}", root.path().join("work/shared").display())
        );
        for unchanged in [
            "package",
            "@scope/package",
            "/absolute",
            "file:/absolute",
            "add",
        ] {
            assert_eq!(anchor_path_spec(unchanged, &cwd), unchanged);
        }
    }

    #[test]
    fn windows_uses_the_source_shell_boundary_for_the_pnpm_command_shim() {
        let arguments = vec!["add".to_owned(), "./plugin".to_owned()];
        let (unix_program, unix_arguments) = pnpm_spawn_spec(&arguments, false);
        assert_eq!(unix_program, "pnpm");
        assert_eq!(
            unix_arguments,
            [OsString::from("add"), OsString::from("./plugin")]
        );
        let (windows_program, windows_arguments) = pnpm_spawn_spec(&arguments, true);
        assert_eq!(windows_program, "cmd.exe");
        assert_eq!(
            windows_arguments,
            ["/d", "/v:off", "/s", "/c", "pnpm", "add", "./plugin"].map(OsString::from)
        );
    }

    fn manifest(bundles: &[&str], dependencies: &[(&str, &str)]) -> ProfileManifest {
        ProfileManifest {
            name: Some("seekdeep-profile-test".to_owned()),
            dependencies: Some(
                dependencies
                    .iter()
                    .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
                    .collect(),
            ),
            seekdeep: Some(SeekDeepManifestSection {
                profile: Some(SeekDeepProfileManifest {
                    bundles: Some(bundles.iter().map(|value| (*value).to_owned()).collect()),
                    ..SeekDeepProfileManifest::default()
                }),
                ..SeekDeepManifestSection::default()
            }),
            ..ProfileManifest::default()
        }
    }

    fn package(root: &Path, name: &str, bundle: bool) {
        let directory = root.join("node_modules").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let seekdeep = bundle.then(|| SeekDeepManifestSection {
            bundle: Some(SeekDeepBundleManifest {
                patch: "./cordis.patch.yml".to_owned(),
                extra: Map::new(),
            }),
            ..SeekDeepManifestSection::default()
        });
        std::fs::write(
            directory.join("package.json"),
            serde_json::to_string(&ProfileManifest {
                name: Some(name.to_owned()),
                seekdeep,
                ..ProfileManifest::default()
            })
            .unwrap(),
        )
        .unwrap();
        if bundle {
            std::fs::write(directory.join("cordis.patch.yml"), "[]\n").unwrap();
        }
    }

    #[test]
    fn reconciliation_adds_updates_and_removes_only_dependency_managed_bundles() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        package(&profile, "bundle-a", true);
        package(&profile, "plain-a", false);
        let before = manifest(&["shipped"], &[]);
        write_profile_manifest(
            &profile,
            &manifest(&["shipped"], &[("bundle-a", "1"), ("plain-a", "1")]),
        )
        .unwrap();
        let warnings =
            reconcile_plugins(&before, &profile, &root.path().join("anchor.json")).unwrap();
        assert_eq!(warnings, ["plain-a"]);
        let after = read_profile_manifest(NAME, &profile).unwrap();
        assert_eq!(
            after
                .seekdeep
                .as_ref()
                .unwrap()
                .profile
                .as_ref()
                .unwrap()
                .bundles
                .as_ref()
                .unwrap(),
            &["shipped".to_owned(), "bundle-a".to_owned()]
        );

        package(&profile, "bundle-a", false);
        let before_update = after.clone();
        write_profile_manifest(
            &profile,
            &manifest(
                &["shipped", "bundle-a"],
                &[("bundle-a", "2"), ("plain-a", "1")],
            ),
        )
        .unwrap();
        assert!(
            reconcile_plugins(&before_update, &profile, &root.path().join("anchor.json"))
                .unwrap()
                .is_empty()
        );
        let updated = read_profile_manifest(NAME, &profile).unwrap();
        assert_eq!(
            updated.seekdeep.unwrap().profile.unwrap().bundles.unwrap(),
            ["shipped"]
        );

        package(&profile, "bundle-a", true);
        let before_gain = read_profile_manifest(NAME, &profile).unwrap();
        assert!(
            reconcile_plugins(&before_gain, &profile, &root.path().join("anchor.json"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_profile_manifest(NAME, &profile)
                .unwrap()
                .seekdeep
                .unwrap()
                .profile
                .unwrap()
                .bundles
                .unwrap(),
            ["shipped", "bundle-a"]
        );
    }

    #[test]
    fn reconciliation_propagates_a_malformed_installed_manifest() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let installed = profile.join("node_modules/broken");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("package.json"), "{not json\n").unwrap();
        let before = manifest(&[], &[("broken", "1")]);
        write_profile_manifest(&profile, &before).unwrap();
        let error =
            reconcile_plugins(&before, &profile, &root.path().join("anchor.json")).unwrap_err();
        assert!(error.to_string().contains("key must be a string"));
    }
}
