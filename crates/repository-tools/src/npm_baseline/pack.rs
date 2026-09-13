use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use path_clean::PathClean as _;

use super::{
    BaselineCommit, BaselineRunner, BaselineVersion, RELEASE_MANIFEST_NAME, ReleaseBundle,
    ReleaseManifest, WorkspacePackageSet,
    bundle::{expect_string, parse_object, validate_base_version},
    normalize_registry, run_options, smoke_installed_bundle, strings,
};

/// Caller-selected inputs to one immutable pack plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaselinePackOptions {
    /// Git ref resolved before packing starts.
    pub reference: String,
    /// Registry recorded in the resulting manifest.
    pub registry: String,
    /// Absolute artifact parent; each version gets a fresh child directory.
    pub output_directory: PathBuf,
}

/// Commit and timestamp identity fixed before installation or compilation begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaselinePackPlan {
    /// Full resolved commit.
    pub commit: BaselineCommit,
    /// Unambiguous abbreviation with a requested minimum of ten characters.
    pub short_commit: String,
    /// UTC timestamp in `YYYYMMDDhhmmss` form.
    pub timestamp: String,
    /// Stable version read from the selected commit.
    pub base_version: BaselineVersion,
    /// Immutable commit-addressed release version.
    pub version: BaselineVersion,
    /// Development channel derived from the stable version.
    pub dist_tag: String,
    /// Validated registry URL.
    pub registry: String,
    /// Fresh directory reserved for this pack attempt.
    pub artifact_directory: PathBuf,
}

impl BaselinePackPlan {
    /// Displays the concrete plan and optionally waits for an empty confirmation line.
    ///
    /// # Errors
    /// Returns noninteractive or cancellation failures.
    pub fn confirm(
        &self,
        runner: &mut impl BaselineRunner,
        assume_yes: bool,
    ) -> anyhow::Result<()> {
        runner.log("publish-npm-baseline: planned pack");
        for line in [
            format!("  commit:    {}", self.commit),
            format!("  timestamp: {} UTC", self.timestamp),
            format!("  version:   {}", self.version),
            format!("  dist-tag:  {}", self.dist_tag),
            format!("  registry:  {}", self.registry),
            format!("  output:    {}", self.artifact_directory.display()),
        ] {
            runner.log(&line);
        }
        if !assume_yes {
            runner.confirm(
                "Press Enter to start packing or type anything to cancel: ",
                "pack requires an interactive terminal or --yes",
                "pack cancelled",
            )?;
        }
        Ok(())
    }
}

/// Resolves a ref and stable root version using read-only Git commands.
///
/// # Errors
/// Returns registry, Git, manifest/version, or existing-output failures.
pub fn plan_baseline(
    root: &Path,
    options: &BaselinePackOptions,
    now: DateTime<Utc>,
    runner: &mut impl BaselineRunner,
) -> anyhow::Result<BaselinePackPlan> {
    let timestamp = now.format("%Y%m%d%H%M%S").to_string();
    let registry = normalize_registry(&options.registry)?;
    let commit = runner.capture(
        "git",
        &strings(&[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", options.reference),
        ]),
        &run_options(root),
    )?;
    let short_commit = runner.capture(
        "git",
        &strings(&["rev-parse", "--short=10", &commit]),
        &run_options(root),
    )?;
    let root_path = format!("{commit}:package.json");
    let manifest = parse_object(
        &runner.capture("git", &strings(&["show", &root_path]), &run_options(root))?,
        &root_path,
    )?;
    let base_version = expect_string(&manifest, "version", &root_path)?;
    validate_base_version(&base_version, &root_path)?;
    let version = BaselineVersion::new(format!("{base_version}-{timestamp}-{short_commit}"));
    let dist_tag = format!("dev-{base_version}");
    let artifact_directory =
        std::path::absolute(options.output_directory.join(version.as_str()))?.clean();
    if artifact_directory.exists() {
        anyhow::bail!("output already exists: {}", artifact_directory.display());
    }
    Ok(BaselinePackPlan {
        commit: BaselineCommit::new(commit),
        short_commit,
        timestamp,
        base_version: BaselineVersion::new(base_version),
        version,
        dist_tag,
        registry,
        artifact_directory,
    })
}

/// Builds and probes a baseline in a temporary detached worktree, preserving the caller's checkout.
///
/// # Errors
/// Returns worktree, package, installation, build, lint, tarball, or installed-smoke failures.
pub fn pack_baseline(
    root: &Path,
    plan: &BaselinePackPlan,
    parent_environment: &BTreeMap<OsString, OsString>,
    runner: &mut impl BaselineRunner,
) -> anyhow::Result<ReleaseBundle> {
    if plan.artifact_directory.exists() {
        anyhow::bail!(
            "output already exists: {}",
            plan.artifact_directory.display()
        );
    }
    let temporary = tempfile::Builder::new()
        .prefix("seekdeep-npm-baseline-")
        .tempdir()?;
    let worktree = temporary.path().join("worktree");
    runner.run(
        "git",
        &strings(&[
            "worktree",
            "add",
            "--detach",
            &worktree.to_string_lossy(),
            plan.commit.as_str(),
        ]),
        &run_options(root),
    )?;
    let mut artifact_created = false;
    let result = pack_worktree(
        root,
        &worktree,
        plan,
        parent_environment,
        runner,
        &mut artifact_created,
    );
    let cleanup = runner.result(
        "git",
        &strings(&["worktree", "remove", "--force", &worktree.to_string_lossy()]),
        &run_options(root),
    );
    if let Ok(cleanup) = &cleanup
        && cleanup.status != Some(0)
    {
        runner.warn(&format!(
            "publish-npm-baseline: could not remove worktree {}",
            worktree.display()
        ));
        if !cleanup.stderr.trim().is_empty() {
            runner.warn(cleanup.stderr.trim());
        }
    }
    drop(temporary);
    if artifact_created {
        std::fs::remove_dir_all(&plan.artifact_directory)?;
    }
    cleanup?;
    result
}

fn pack_worktree(
    root: &Path,
    worktree: &Path,
    plan: &BaselinePackPlan,
    environment: &BTreeMap<OsString, OsString>,
    runner: &mut impl BaselineRunner,
    artifact_created: &mut bool,
) -> anyhow::Result<ReleaseBundle> {
    let package_set = WorkspacePackageSet::discover(worktree)?;
    if package_set.base_version != plan.base_version {
        anyhow::bail!(
            "workspace package version {} does not match root version {} at {}",
            package_set.base_version,
            plan.base_version,
            plan.commit
        );
    }
    runner.log(&format!(
        "publish-npm-baseline: installing detached worktree {}",
        plan.short_commit
    ));
    let options = run_options(worktree);
    runner.run(
        "pnpm",
        &strings(&["install", "--frozen-lockfile"]),
        &options,
    )?;
    runner.run("pnpm", &strings(&["run", "constraints"]), &options)?;
    package_set.stage(worktree, &plan.version)?;
    std::fs::create_dir_all(&plan.artifact_directory)?;
    *artifact_created = true;
    runner.log(&format!(
        "publish-npm-baseline: building {} packages as {}",
        package_set.packages.len(),
        plan.version
    ));
    for script in ["build", "publint", "verify-built-package-invariants"] {
        runner.run("pnpm", &strings(&["run", script]), &options)?;
    }
    runner.run(
        "pnpm",
        &strings(&[
            "--filter",
            "./vendor/**",
            "--filter",
            "./packages/**",
            "--filter",
            "./apps/**",
            "--recursive",
            "pack",
            "--pack-destination",
            &plan.artifact_directory.to_string_lossy(),
        ]),
        &options,
    )?;
    let bundle = ReleaseBundle::create(
        &plan.artifact_directory,
        &package_set.packages,
        ReleaseManifest {
            schema_version: 1,
            commit: plan.commit.clone(),
            version: plan.version.clone(),
            dist_tag: plan.dist_tag.clone(),
            registry: plan.registry.clone(),
            packages: Vec::new(),
        },
        runner,
    )?;
    smoke_installed_bundle(&bundle, environment, runner)?;
    *artifact_created = false;
    runner.log(&format!(
        "publish-npm-baseline: packed {} packages",
        bundle.manifest.packages.len()
    ));
    runner.log(&format!("  version:  {}", bundle.manifest.version));
    runner.log(&format!("  dist-tag: {}", bundle.manifest.dist_tag));
    let manifest = bundle.directory.join(RELEASE_MANIFEST_NAME);
    runner.log(&format!("  manifest: {}", manifest.display()));
    runner.log(&format!(
        "  publish:  {}",
        copyable_command(
            "pnpm",
            &strings(&[
                "--dir",
                &root.to_string_lossy(),
                "run",
                "publish:npm-baseline",
                "publish",
                "--manifest",
                &manifest.to_string_lossy(),
                "--yes"
            ])
        )
    ));
    Ok(bundle)
}

fn copyable_command(command: &str, arguments: &[String]) -> String {
    std::iter::once(command)
        .chain(arguments.iter().map(String::as_str))
        .map(|value| {
            if !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@=+-".contains(&byte))
            {
                value.to_owned()
            } else {
                format!("'{}'", value.replace('\'', "'\"'\"'"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
