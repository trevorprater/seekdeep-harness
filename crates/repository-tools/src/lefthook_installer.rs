//! Worktree-local Lefthook and bilingual merge-driver installation.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, Metadata, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use path_clean::PathClean as _;
use regex::Regex;
use serde::{Deserialize, Serialize};

const MINIMUM_GIT: [u64; 3] = [2, 26, 0];
const HOOKS_DIRECTORY: &str = "seekdeep-hooks";
const OWNERSHIP_MARKER: &str = ".seekdeep-lefthook-owned";
const OWNERSHIP_MARKER_VERSION: u64 = 1;
const OWNERSHIP_MARKER_OWNER: &str = "seekdeep-harness worktree-local lefthook hooks";
const INSTALL_LOCK: &str = "seekdeep-lefthook-install.lock";
const ALLOW_HOOKS_PATH_OVERRIDE: &str = "SEEKDEEP_LEFTHOOK_ALLOW_HOOKS_PATH_OVERRIDE";
const PAIRING_DRIVER_NAME_KEY: &str = "merge.seekdeep-translation-pairing.name";
const PAIRING_DRIVER_NAME: &str = "SeekDeep Harness bilingual pairing records";
const PAIRING_DRIVER_COMMAND_KEY: &str = "merge.seekdeep-translation-pairing.driver";
const PAIRING_DRIVER_COMMAND: &str = "scripts/merge-translation-pairing-driver.sh %O %A %B %P";

/// Timing bounds for the cross-process installer lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallerLockTiming {
    /// Maximum wait for a live installer to release its lock.
    pub wait_timeout: Duration,
    /// Grace period for a just-created lock whose record is still being written.
    pub initialization_timeout: Duration,
    /// Poll interval while another installer owns or initializes the lock.
    pub poll_interval: Duration,
}

impl Default for InstallerLockTiming {
    fn default() -> Self {
        Self {
            wait_timeout: Duration::from_secs(30),
            initialization_timeout: Duration::from_secs(1),
            poll_interval: Duration::from_millis(50),
        }
    }
}

/// Explicit process and runtime dependencies for one installation.
#[derive(Clone, Debug)]
pub struct LefthookInstallOptions {
    /// Lefthook executable or platform shim.
    pub lefthook: PathBuf,
    /// Compiled pairing merge-driver executable to probe before publication.
    pub pairing_driver: PathBuf,
    /// Complete child-process environment, including isolated Git configuration.
    pub environment: BTreeMap<OsString, OsString>,
    /// Cross-process lock timing.
    pub lock_timing: InstallerLockTiming,
}

impl LefthookInstallOptions {
    /// Creates options from the current process environment and production timing.
    #[must_use]
    pub fn current(lefthook: PathBuf, pairing_driver: PathBuf) -> Self {
        Self {
            lefthook,
            pairing_driver,
            environment: std::env::vars_os().collect(),
            lock_timing: InstallerLockTiming::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedCommand {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigEntry {
    origin: String,
    scope: Option<String>,
    value: String,
    name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorktreeConfigMigration {
    version: u64,
    extension_enabled: bool,
    direct_bare: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnedHooksDirectory {
    marker_path: PathBuf,
    recorded_hooks_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnershipMarkerRecord {
    version: u64,
    owner: String,
    hooks_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

struct InstallLock {
    path: PathBuf,
    record: String,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InitializingLock {
    identity: FileIdentity,
    deadline: Instant,
}

/// Runs postinstall discovery and installs only in an interactive Git checkout.
///
/// Automated jobs, non-repository directories, and checkouts without the
/// Lefthook shim are accepted without mutation.
///
/// # Errors
///
/// Returns repository discovery, executable discovery, configuration, locking,
/// ownership, probe, Lefthook, rollback, or lock-release diagnostics.
pub fn run_lefthook_postinstall() -> anyhow::Result<()> {
    let environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    if environment_value(&environment, "CI") == Some("true")
        || environment_value(&environment, "GITHUB_ACTIONS") == Some("true")
    {
        return Ok(());
    }
    let current = std::env::current_dir()?;
    let Some(root) = discover_repository_root(&current, &environment)? else {
        return Ok(());
    };
    let lefthook = root
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) {
            "lefthook.cmd"
        } else {
            "lefthook"
        });
    if !lefthook.exists() {
        return Ok(());
    }
    let pairing_driver = sibling_binary("merge-translation-pairing")?;
    let options = LefthookInstallOptions {
        lefthook,
        pairing_driver,
        environment,
        lock_timing: InstallerLockTiming::default(),
    };
    install_lefthook(&root, &options)
}

/// Installs hooks and pairing-driver configuration for one exact worktree.
///
/// # Errors
///
/// Returns Git version/configuration, repository-format migration, lock,
/// ownership, driver-probe, Lefthook, rollback, or lock-release failures.
pub fn install_lefthook(root: &Path, options: &LefthookInstallOptions) -> anyhow::Result<()> {
    let discovery = Execution::new(root, &options.environment);
    let canonical_root = PathBuf::from(strip_git_line_terminator(
        &discovery
            .git_success(
                &["rev-parse", "--show-toplevel"],
                "locating the repository root",
            )?
            .stdout,
    ));
    let execution = Execution::new(&canonical_root, &options.environment);
    assert_supported_git(&execution)?;
    let git_directory = PathBuf::from(strip_git_line_terminator(
        &execution
            .git_success(
                &["rev-parse", "--absolute-git-dir"],
                "locating the worktree Git directory",
            )?
            .stdout,
    ));
    let common_output = PathBuf::from(strip_git_line_terminator(
        &execution
            .git_success(
                &["rev-parse", "--git-common-dir"],
                "locating the common Git directory",
            )?
            .stdout,
    ));
    let common_directory = if common_output.is_absolute() {
        common_output.clean()
    } else {
        canonical_root.join(common_output).clean()
    };
    let common_config = common_directory.join("config");
    let worktree_config = git_directory.join("config.worktree");
    let hooks_path = git_directory.join(HOOKS_DIRECTORY);
    let lock = acquire_install_lock(
        &common_directory,
        options,
        environment_delay(
            &options.environment,
            "SEEKDEEP_TEST_LEFTHOOK_LOCK_WRITE_DELAY_MS",
        )?,
    )?;
    let installation = install_locked(
        &execution,
        options,
        &common_directory,
        &common_config,
        &worktree_config,
        &hooks_path,
    );
    let release = lock.release();
    match (installation, release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(installation), Ok(())) => Err(installation),
        (Ok(()), Err(release)) => Err(release),
        (Err(installation), Err(release)) => anyhow::bail!(
            "Lefthook installation failed: {installation}; installer lock release also failed: {release}"
        ),
    }
}

fn install_locked(
    execution: &Execution<'_>,
    options: &LefthookInstallOptions,
    common_directory: &Path,
    common_config: &Path,
    worktree_config: &Path,
    hooks_path: &Path,
) -> anyhow::Result<()> {
    assert_common_config_file(common_config)?;
    assert_worktree_config_files(execution, common_directory, common_config, worktree_config)?;
    let worktree_entries =
        included_file_config_entries(execution, worktree_config, "core.hooksPath")?;
    if let Some(entry) = worktree_entries
        .iter()
        .find(|entry| !origin_is_file(&entry.origin, execution.root, worktree_config))
    {
        return refuse_scoped_hooks_path(&ConfigEntry {
            scope: Some("worktree".to_owned()),
            ..entry.clone()
        });
    }
    let worktree_path = assert_single(
        worktree_entries
            .iter()
            .map(|entry| entry.value.clone())
            .collect(),
        "worktree core.hooksPath",
    )?
    .map(PathBuf::from);
    let ownership = inspect_existing_ownership(
        common_directory,
        hooks_path,
        worktree_path.as_deref(),
        worktree_config,
    )?;
    assert_effective_hooks_path(
        execution,
        worktree_config,
        worktree_path.as_deref(),
        hooks_path,
        &ownership,
    )?;
    let migration = plan_worktree_config_migration(execution, common_config)?;
    let owned_hooks = ensure_owned_hooks_directory(hooks_path)?;
    if let Some(worktree_path) = &worktree_path
        && worktree_path != hooks_path
        && owned_hooks.recorded_hooks_path != *worktree_path
        && !ownership.copied_path_is_owned
    {
        anyhow::bail!(
            "hooks directory ownership changed while relocating {}",
            json_path(worktree_path)
        );
    }
    apply_worktree_config_migration(execution, common_config, migration)?;
    publish_integrations(
        execution,
        options,
        worktree_config,
        hooks_path,
        worktree_path.as_deref(),
        &owned_hooks,
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ExistingOwnership {
    reserved: Option<OwnedHooksDirectory>,
    copied_path_is_owned: bool,
}

fn inspect_existing_ownership(
    common_directory: &Path,
    hooks_path: &Path,
    worktree_path: Option<&Path>,
    worktree_config: &Path,
) -> anyhow::Result<ExistingOwnership> {
    let mut ownership = ExistingOwnership::default();
    let Some(worktree_path) = worktree_path else {
        return Ok(ownership);
    };
    if worktree_path == hooks_path {
        return Ok(ownership);
    }
    ownership.reserved = inspect_owned_hooks_directory(hooks_path)?;
    let path_is_relocated = ownership
        .reserved
        .as_ref()
        .is_some_and(|owned| owned.recorded_hooks_path == worktree_path);
    ownership.copied_path_is_owned =
        !path_is_relocated && is_registered_owned_hooks_path(common_directory, worktree_path)?;
    if !path_is_relocated && !ownership.copied_path_is_owned {
        return refuse_scoped_hooks_path(&ConfigEntry {
            origin: format!("file:{}", worktree_config.display()),
            scope: Some("worktree".to_owned()),
            value: worktree_path.to_string_lossy().into_owned(),
            name: None,
        })
        .map(|()| ownership);
    }
    Ok(ownership)
}

fn assert_effective_hooks_path(
    execution: &Execution<'_>,
    worktree_config: &Path,
    worktree_path: Option<&Path>,
    hooks_path: &Path,
    ownership: &ExistingOwnership,
) -> anyhow::Result<()> {
    let direct_path_is_owned = worktree_path.is_some_and(|worktree_path| {
        worktree_path == hooks_path
            || ownership
                .reserved
                .as_ref()
                .is_some_and(|owned| owned.recorded_hooks_path == worktree_path)
            || ownership.copied_path_is_owned
    });
    let Some(effective) = effective_config_entry(execution, "core.hooksPath")? else {
        return Ok(());
    };
    let effective_path_is_owned = effective.scope.as_deref() == Some("worktree")
        && worktree_path.is_some_and(|path| effective.value == path.to_string_lossy())
        && direct_path_is_owned
        && origin_is_file(&effective.origin, execution.root, worktree_config);
    if effective_path_is_owned {
        return Ok(());
    }
    if matches!(
        effective.scope.as_deref(),
        Some("system" | "global" | "local")
    ) {
        if environment_value(execution.environment, ALLOW_HOOKS_PATH_OVERRIDE) == Some("1") {
            Ok(())
        } else {
            anyhow::bail!(
                "refusing to replace user-owned core.hooksPath ({}). Chain those hooks through lefthook.yml, or, if this inherited path may remain active only in other worktrees, rerun with {ALLOW_HOOKS_PATH_OVERRIDE}=1",
                config_source(&effective)
            )
        }
    } else {
        refuse_scoped_hooks_path(&effective)
    }
}

fn publish_integrations(
    execution: &Execution<'_>,
    options: &LefthookInstallOptions,
    worktree_config: &Path,
    hooks_path: &Path,
    previous_hooks_path: Option<&Path>,
    owned_hooks: &OwnedHooksDirectory,
) -> anyhow::Result<()> {
    probe_pairing_driver(execution, &options.pairing_driver)?;
    let added_driver_keys = install_pairing_merge_driver(execution, worktree_config)?;
    let mut path_changed = false;
    let installation = (|| -> anyhow::Result<()> {
        execution.git_success_owned(
            &[
                "config".into(),
                "--worktree".into(),
                "core.hooksPath".into(),
                hooks_path.as_os_str().to_owned(),
            ],
            "installing worktree-local core.hooksPath",
        )?;
        path_changed = previous_hooks_path != Some(hooks_path);
        let installed = effective_config_entry(execution, "core.hooksPath")?;
        if installed.as_ref().is_none_or(|entry| {
            entry.scope.as_deref() != Some("worktree")
                || entry.value != hooks_path.to_string_lossy()
                || !origin_is_file(&entry.origin, execution.root, worktree_config)
        }) {
            anyhow::bail!(
                "new worktree-local core.hooksPath did not become the effective direct worktree value"
            );
        }
        run_lefthook(execution, options)?;
        update_ownership_marker(&owned_hooks.marker_path, hooks_path)?;
        Ok(())
    })();
    if let Err(error) = installation {
        let mut rollback_errors = Vec::new();
        if path_changed && let Err(rollback) = rollback_hooks_path(execution, previous_hooks_path) {
            rollback_errors.push(rollback.to_string());
        }
        if let Err(rollback) = rollback_pairing_merge_driver(execution, &added_driver_keys) {
            rollback_errors.push(rollback.to_string());
        }
        if rollback_errors.is_empty() {
            return Err(error);
        }
        anyhow::bail!(
            "Lefthook installation failed: {error}; worktree integration rollback also failed: {}",
            rollback_errors.join("; ")
        );
    }
    Ok(())
}

fn rollback_hooks_path(execution: &Execution<'_>, previous: Option<&Path>) -> anyhow::Result<()> {
    let mut args = vec![OsString::from("config"), OsString::from("--worktree")];
    if let Some(previous) = previous {
        args.extend([
            OsString::from("core.hooksPath"),
            previous.as_os_str().to_owned(),
        ]);
    } else {
        args.extend([
            OsString::from("--unset-all"),
            OsString::from("core.hooksPath"),
        ]);
    }
    execution.git_success_owned(&args, "rolling back worktree-local core.hooksPath")?;
    Ok(())
}

fn install_pairing_merge_driver(
    execution: &Execution<'_>,
    worktree_config: &Path,
) -> anyhow::Result<Vec<String>> {
    let mut added = Vec::new();
    for (key, expected) in [
        (PAIRING_DRIVER_NAME_KEY, PAIRING_DRIVER_NAME),
        (PAIRING_DRIVER_COMMAND_KEY, PAIRING_DRIVER_COMMAND),
    ] {
        let result = install_pairing_config_entry(execution, worktree_config, key, expected);
        if let Err(error) = result {
            if let Err(rollback) = rollback_pairing_merge_driver(execution, &added) {
                anyhow::bail!(
                    "Pairing merge-driver configuration failed: {error}; rollback also failed: {rollback}"
                );
            }
            return Err(error);
        }
        if result? {
            added.push(key.to_owned());
        }
    }
    Ok(added)
}

fn install_pairing_config_entry(
    execution: &Execution<'_>,
    worktree_config: &Path,
    key: &str,
    expected: &str,
) -> anyhow::Result<bool> {
    let entries = included_file_config_entries(execution, worktree_config, key)?;
    if let Some(included) = entries
        .iter()
        .find(|entry| !origin_is_file(&entry.origin, execution.root, worktree_config))
    {
        anyhow::bail!(
            "refusing pairing merge-driver config from an included worktree file ({})",
            config_source(included)
        );
    }
    let existing = assert_single(
        entries.iter().map(|entry| entry.value.clone()).collect(),
        &format!("worktree {key}"),
    )?;
    let effective_before = effective_config_entry(execution, key)?;
    if let Some(entry) = effective_before
        .as_ref()
        .filter(|entry| entry.scope.as_deref() == Some("command"))
    {
        anyhow::bail!(
            "refusing command-scoped {key} ({}); transient configuration cannot be replaced by the worktree installer",
            config_source(entry)
        );
    }
    if existing.is_none()
        && let Some(effective) = &effective_before
        && effective.value != expected
    {
        anyhow::bail!(
            "refusing to mask inherited {key} ({}); remove or integrate the custom pairing merge driver explicitly",
            config_source(effective)
        );
    }
    if let Some(existing) = &existing
        && existing != expected
    {
        anyhow::bail!(
            "refusing to replace worktree {key} value {}; remove or integrate the custom pairing merge driver explicitly",
            serde_json::to_string(existing)?
        );
    }
    let added = existing.is_none();
    if added {
        execution.git_success(
            &["config", "--worktree", key, expected],
            &format!("installing worktree-local {key}"),
        )?;
    }
    let installed = included_file_config_entries(execution, worktree_config, key)?;
    if installed.len() != 1
        || installed[0].value != expected
        || !origin_is_file(&installed[0].origin, execution.root, worktree_config)
    {
        anyhow::bail!("new worktree-local {key} did not become the direct worktree value");
    }
    let effective_after = effective_config_entry(execution, key)?;
    if effective_after.as_ref().is_none_or(|entry| {
        entry.scope.as_deref() != Some("worktree")
            || entry.value != expected
            || !origin_is_file(&entry.origin, execution.root, worktree_config)
    }) {
        anyhow::bail!(
            "new worktree-local {key} did not become the effective direct worktree value"
        );
    }
    Ok(added)
}

fn rollback_pairing_merge_driver(execution: &Execution<'_>, keys: &[String]) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for key in keys.iter().rev() {
        if let Err(error) = execution.git_success(
            &["config", "--worktree", "--unset-all", key],
            &format!("rolling back worktree-local {key}"),
        ) {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("{}", failures.join("; "));
    }
    Ok(())
}

fn probe_pairing_driver(execution: &Execution<'_>, executable: &Path) -> anyhow::Result<()> {
    let output = execution.capture(executable.as_os_str(), &[OsString::from("--probe")], None)?;
    if !output.status.success() {
        anyhow::bail!(
            "{} --probe failed: {}",
            executable.display(),
            command_failure_detail(&output)
        );
    }
    Ok(())
}

fn run_lefthook(execution: &Execution<'_>, options: &LefthookInstallOptions) -> anyhow::Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd.exe");
        command.arg("/C").arg(&options.lefthook);
        command
    } else {
        Command::new(&options.lefthook)
    };
    command
        .args(["install", "--force"])
        .current_dir(execution.root)
        .env_clear()
        .envs(filtered_lefthook_environment(execution.environment))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command.status().map_err(|error| {
        anyhow::anyhow!(
            "{} install --force failed: {error}",
            options.lefthook.display()
        )
    })?;
    if !status.success() {
        anyhow::bail!(
            "{} install --force failed: exit status {}",
            options.lefthook.display(),
            status_text(status)
        );
    }
    Ok(())
}

fn filtered_lefthook_environment(
    environment: &BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    environment
        .iter()
        .filter(|(key, _)| !is_command_git_config_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_command_git_config_key(key: &OsStr) -> bool {
    static INDEXED: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^GIT_CONFIG_(?:KEY|VALUE)_\d+$").expect("static Git-config variable regex")
    });
    let normalized = key.to_string_lossy().to_uppercase();
    normalized == "GIT_CONFIG_PARAMETERS"
        || normalized == "GIT_CONFIG_COUNT"
        || INDEXED.is_match(&normalized)
}

fn assert_supported_git(execution: &Execution<'_>) -> anyhow::Result<()> {
    static VERSION: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"git version (\d+)\.(\d+)(?:\.(\d+))?").expect("static Git-version regex")
    });
    let output = execution.git_success(&["--version"], "reading Git version")?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let captures = VERSION
        .captures(&version)
        .ok_or_else(|| anyhow::anyhow!("cannot determine Git version from {version:?}"))?;
    let actual = [
        captures[1].parse::<u64>()?,
        captures[2].parse::<u64>()?,
        captures
            .get(3)
            .map_or(Ok(0), |capture| capture.as_str().parse::<u64>())?,
    ];
    if actual < MINIMUM_GIT {
        anyhow::bail!("Git 2.26 or newer is required for worktree-local hooks; found {version}");
    }
    Ok(())
}

fn assert_common_config_file(path: &Path) -> anyhow::Result<()> {
    let metadata = symlink_metadata_if_present(path)?;
    if metadata
        .as_ref()
        .is_none_or(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!(
            "refusing common repository config {} because it is not a regular file",
            json_path(path)
        );
    }
    Ok(())
}

fn assert_worktree_config_files(
    execution: &Execution<'_>,
    common_directory: &Path,
    common_config: &Path,
    current_config: &Path,
) -> anyhow::Result<()> {
    let extension_enabled = worktree_config_extension_enabled(execution, common_config)?;
    for config in registered_worktree_config_paths(common_directory)? {
        let Some(metadata) = symlink_metadata_if_present(&config)? else {
            continue;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            let state = if extension_enabled {
                "active"
            } else {
                "dormant"
            };
            anyhow::bail!(
                "refusing {state} worktree config {} because it is not a regular file; replace it with a regular worktree config or remove it before retrying",
                json_path(&config)
            );
        }
        if extension_enabled || !has_direct_config_entries(execution, &config)? {
            continue;
        }
        let owner = if normalized_path(&config) == normalized_path(current_config) {
            "current"
        } else {
            "sibling"
        };
        anyhow::bail!(
            "cannot enable extensions.worktreeConfig while {owner} dormant worktree config {} contains user-owned settings that enabling the extension would activate; inspect and migrate those settings, then enable the extension explicitly or remove them before retrying",
            json_path(&config)
        );
    }
    Ok(())
}

fn plan_worktree_config_migration(
    execution: &Execution<'_>,
    common_config: &Path,
) -> anyhow::Result<WorktreeConfigMigration> {
    let version_text = assert_single(
        direct_file_config_values(execution, common_config, "core.repositoryFormatVersion")?,
        "core.repositoryFormatVersion",
    )?
    .ok_or_else(|| anyhow::anyhow!("unsupported core.repositoryFormatVersion: undefined"))?;
    let version = version_text.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("unsupported core.repositoryFormatVersion: {version_text:?}")
    })?;
    if version == 0
        && let Some(extension) =
            direct_file_config_matching_entries(execution, common_config, r"^extensions\.")?.first()
    {
        anyhow::bail!(
            "cannot upgrade core.repositoryFormatVersion from 0 while dormant repository extension {} is configured ({}); audit and migrate it, then set repository format 1 explicitly before retrying",
            extension.name.as_deref().unwrap_or("extensions.unknown"),
            config_source(extension)
        );
    }
    let extension_enabled = worktree_config_extension_enabled(execution, common_config)?;
    if let Some(worktree) = assert_single(
        direct_file_config_values(execution, common_config, "core.worktree")?,
        "core.worktree",
    )? {
        anyhow::bail!(
            "cannot enable extensions.worktreeConfig while core.worktree is in the common config (file:{}: {}); move it to the main worktree config first",
            common_config.display(),
            serde_json::to_string(&worktree)?
        );
    }
    let direct_bare_text = assert_single(
        direct_file_config_values(execution, common_config, "core.bare")?,
        "core.bare",
    )?;
    let direct_bare = direct_bare_text
        .as_deref()
        .map(|value| parse_git_boolean(value, "core.bare"))
        .transpose()?;
    if direct_bare == Some(true) {
        anyhow::bail!(
            "cannot enable extensions.worktreeConfig for a common config with core.bare=true (file:{}: {})",
            common_config.display(),
            serde_json::to_string(&direct_bare_text)?
        );
    }
    Ok(WorktreeConfigMigration {
        version,
        extension_enabled,
        direct_bare,
    })
}

fn apply_worktree_config_migration(
    execution: &Execution<'_>,
    common_config: &Path,
    migration: WorktreeConfigMigration,
) -> anyhow::Result<()> {
    if migration.version == 0 {
        execution.git_success_owned(
            &[
                "config".into(),
                "--file".into(),
                common_config.as_os_str().to_owned(),
                "core.repositoryFormatVersion".into(),
                "1".into(),
            ],
            "upgrading the repository format",
        )?;
    }
    if !migration.extension_enabled {
        execution.git_success_owned(
            &[
                "config".into(),
                "--file".into(),
                common_config.as_os_str().to_owned(),
                "extensions.worktreeConfig".into(),
                "true".into(),
            ],
            "enabling worktree configuration",
        )?;
    }
    if migration.direct_bare == Some(false) {
        execution.git_success_owned(
            &[
                "config".into(),
                "--file".into(),
                common_config.as_os_str().to_owned(),
                "--unset-all".into(),
                "core.bare".into(),
            ],
            "moving core.bare out of the common configuration",
        )?;
    }
    Ok(())
}

fn worktree_config_extension_enabled(
    execution: &Execution<'_>,
    common_config: &Path,
) -> anyhow::Result<bool> {
    let value = assert_single(
        direct_file_config_values(execution, common_config, "extensions.worktreeConfig")?,
        "extensions.worktreeConfig",
    )?;
    value
        .as_deref()
        .map(|value| parse_git_boolean(value, "extensions.worktreeConfig"))
        .transpose()
        .map(|value| value.unwrap_or(false))
}

fn parse_git_boolean(value: &str, key: &str) -> anyhow::Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => anyhow::bail!("invalid Boolean value for {key}: {value:?}"),
    }
}

fn registered_worktree_config_paths(common_directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = vec![common_directory.join("config.worktree")];
    let linked = common_directory.join("worktrees");
    let mut entries = match fs::read_dir(linked) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error.into()),
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    paths.extend(
        entries
            .into_iter()
            .map(|entry| entry.path().join("config.worktree")),
    );
    Ok(paths)
}

fn inspect_owned_hooks_directory(path: &Path) -> anyhow::Result<Option<OwnedHooksDirectory>> {
    let Some(metadata) = symlink_metadata_if_present(path)? else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to use non-directory or symlinked hooks path {}",
            path.display()
        );
    }
    let marker_path = path.join(OWNERSHIP_MARKER);
    let Some(marker_metadata) = symlink_metadata_if_present(&marker_path)? else {
        anyhow::bail!(
            "refusing to overwrite unowned hooks directory {}",
            path.display()
        );
    };
    let marker = if marker_metadata.is_file()
        && !marker_metadata.file_type().is_symlink()
        && link_count(&marker_metadata) == 1
    {
        parse_ownership_marker(&fs::read_to_string(&marker_path)?)
    } else {
        None
    };
    let Some(recorded_hooks_path) = marker else {
        anyhow::bail!(
            "refusing to overwrite hooks directory with an invalid ownership marker: {}",
            path.display()
        );
    };
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == OsStr::new(OWNERSHIP_MARKER) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || link_count(&metadata) != 1 {
            anyhow::bail!(
                "refusing to overwrite non-regular or multiply linked hook entry {}",
                json_path(&entry.path())
            );
        }
    }
    Ok(Some(OwnedHooksDirectory {
        marker_path,
        recorded_hooks_path,
    }))
}

fn ensure_owned_hooks_directory(path: &Path) -> anyhow::Result<OwnedHooksDirectory> {
    if let Some(owned) = inspect_owned_hooks_directory(path)? {
        return Ok(owned);
    }
    create_private_directory(path)?;
    let marker_path = path.join(OWNERSHIP_MARKER);
    write_new_private_file(&marker_path, ownership_marker_content(path)?.as_bytes())?;
    Ok(OwnedHooksDirectory {
        marker_path,
        recorded_hooks_path: path.to_owned(),
    })
}

fn is_registered_owned_hooks_path(
    common_directory: &Path,
    hooks_path: &Path,
) -> anyhow::Result<bool> {
    let registered = registered_worktree_config_paths(common_directory)?
        .iter()
        .any(|config| {
            normalized_path(&config.parent().unwrap_or(config).join(HOOKS_DIRECTORY))
                == normalized_path(hooks_path)
        });
    if !registered {
        return Ok(false);
    }
    Ok(inspect_owned_hooks_directory(hooks_path)?
        .is_some_and(|owned| owned.recorded_hooks_path == hooks_path))
}

fn ownership_marker_content(hooks_path: &Path) -> anyhow::Result<String> {
    Ok(format!(
        "{}\n",
        serde_json::to_string(&OwnershipMarkerRecord {
            version: OWNERSHIP_MARKER_VERSION,
            owner: OWNERSHIP_MARKER_OWNER.to_owned(),
            hooks_path: hooks_path.to_owned(),
        })?
    ))
}

fn parse_ownership_marker(content: &str) -> Option<PathBuf> {
    let marker = serde_json::from_str::<OwnershipMarkerRecord>(content).ok()?;
    (marker.version == OWNERSHIP_MARKER_VERSION
        && marker.owner == OWNERSHIP_MARKER_OWNER
        && marker.hooks_path.is_absolute())
    .then_some(marker.hooks_path)
}

fn update_ownership_marker(marker_path: &Path, hooks_path: &Path) -> anyhow::Result<()> {
    fs::write(marker_path, ownership_marker_content(hooks_path)?)?;
    set_private_file_permissions(marker_path)?;
    Ok(())
}

fn acquire_install_lock(
    common_directory: &Path,
    options: &LefthookInstallOptions,
    write_delay: Duration,
) -> anyhow::Result<InstallLock> {
    let path = common_directory.join(INSTALL_LOCK);
    let deadline = Instant::now() + options.lock_timing.wait_timeout;
    let record = format!("{} {}\n", std::process::id(), lock_nonce());
    let mut initializing = None::<InitializingLock>;
    loop {
        match create_lock_file(&path, &record, write_delay) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let Some(first_metadata) = symlink_metadata_if_present(&path)? else {
            continue;
        };
        if !first_metadata.is_file() || first_metadata.file_type().is_symlink() {
            return Err(manual_lock_recovery_error(&path, "invalid"));
        }
        let existing = match fs::read(&path) {
            Ok(existing) => String::from_utf8_lossy(&existing).into_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Some(verified_metadata) = symlink_metadata_if_present(&path)? else {
            continue;
        };
        if !verified_metadata.is_file() || verified_metadata.file_type().is_symlink() {
            return Err(manual_lock_recovery_error(&path, "invalid"));
        }
        let first_identity = file_identity(&first_metadata)?;
        let verified_identity = file_identity(&verified_metadata)?;
        if first_identity != verified_identity {
            continue;
        }
        let Some(owner) = parse_install_lock(&existing) else {
            if !install_lock_record_may_be_incomplete(&existing) {
                return Err(manual_lock_recovery_error(&path, "invalid"));
            }
            let now = Instant::now();
            if initializing.is_none_or(|lock| lock.identity != first_identity) {
                initializing = Some(InitializingLock {
                    identity: first_identity,
                    deadline: now + options.lock_timing.initialization_timeout,
                });
            }
            if initializing.is_some_and(|lock| now >= lock.deadline) {
                return Err(manual_lock_recovery_error(&path, "invalid"));
            }
            std::thread::sleep(options.lock_timing.poll_interval);
            continue;
        };
        initializing = None;
        if !lock_owner_is_alive(owner)? {
            return Err(manual_lock_recovery_error(&path, "stale"));
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for Lefthook installer lock {}",
                path.display()
            );
        }
        std::thread::sleep(options.lock_timing.poll_interval);
    }
}

fn create_lock_file(
    path: &Path,
    record: &str,
    write_delay: Duration,
) -> std::io::Result<InstallLock> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let identity = file_identity(&file.metadata()?).map_err(std::io::Error::other)?;
    if !write_delay.is_zero() {
        std::thread::sleep(write_delay);
    }
    file.write_all(record.as_bytes())?;
    drop(file);
    let published = fs::symlink_metadata(path)?;
    if !published.is_file()
        || published.file_type().is_symlink()
        || file_identity(&published).map_err(std::io::Error::other)? != identity
    {
        return Err(std::io::Error::other(lock_ownership_changed_error(path)));
    }
    Ok(InstallLock {
        path: path.to_owned(),
        record: record.to_owned(),
        identity,
    })
}

impl InstallLock {
    fn release(self) -> anyhow::Result<()> {
        let current = symlink_metadata_if_present(&self.path)?;
        if current.as_ref().is_none_or(|metadata| {
            !metadata.is_file()
                || metadata.file_type().is_symlink()
                || file_identity(metadata).ok() != Some(self.identity)
        }) || fs::read_to_string(&self.path).ok().as_deref() != Some(&self.record)
        {
            return Err(lock_ownership_changed_error(&self.path));
        }
        fs::remove_file(&self.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                lock_ownership_changed_error(&self.path)
            } else {
                error.into()
            }
        })?;
        Ok(())
    }
}

fn parse_install_lock(record: &str) -> Option<u32> {
    static LOCK: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^([1-9]\d*) ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\n$")
            .expect("static installer-lock regex")
    });
    LOCK.captures(record)?.get(1)?.as_str().parse().ok()
}

fn install_lock_record_may_be_incomplete(record: &str) -> bool {
    static PARTIAL: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^[1-9]\d*(?: [0-9a-f-]*)?$").expect("static partial-lock regex")
    });
    record.is_empty() || (!record.ends_with('\n') && PARTIAL.is_match(record))
}

#[cfg(unix)]
fn lock_owner_is_alive(owner: u32) -> anyhow::Result<bool> {
    use nix::{errno::Errno, sys::signal, unistd::Pid};

    let owner = i32::try_from(owner)?;
    match signal::kill(Pid::from_raw(owner), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn lock_owner_is_alive(owner: u32) -> anyhow::Result<bool> {
    let output = Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {owner}"), "/FO", "CSV", "/NH"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("tasklist failed with status {}", status_text(output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{owner}\"")))
}

fn manual_lock_recovery_error(path: &Path, condition: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{condition} Lefthook installer lock {}. Confirm no Lefthook installer is running, remove it manually, and retry.",
        json_path(path)
    )
}

fn lock_ownership_changed_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "Lefthook installer lock ownership changed for {}; refusing to remove it",
        path.display()
    )
}

fn lock_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = u128::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let value = timestamp ^ (u128::from(std::process::id()) << 64) ^ counter;
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (value >> 96) & 0xffff_ffff,
        (value >> 80) & 0xffff,
        (value >> 68) & 0x0fff,
        (value >> 56) & 0x0fff,
        value & 0xffff_ffff_ffff
    )
}

fn direct_file_config_values(
    execution: &Execution<'_>,
    config: &Path,
    key: &str,
) -> anyhow::Result<Vec<String>> {
    let output = execution.git_allow_owned(
        &[
            "config".into(),
            "--file".into(),
            config.as_os_str().to_owned(),
            "--no-includes".into(),
            "--null".into(),
            "--get-all".into(),
            key.into(),
        ],
        &[1],
        &format!("reading direct {key} values"),
    )?;
    nul_values(&output)
}

fn included_file_config_entries(
    execution: &Execution<'_>,
    config: &Path,
    key: &str,
) -> anyhow::Result<Vec<ConfigEntry>> {
    let output = execution.git_allow_owned(
        &[
            "config".into(),
            "--file".into(),
            config.as_os_str().to_owned(),
            "--includes".into(),
            "--null".into(),
            "--show-origin".into(),
            "--get-all".into(),
            key.into(),
        ],
        &[1],
        &format!("reading included {key} values"),
    )?;
    let fields = nul_values(&output)?;
    if fields.len() % 2 != 0 {
        anyhow::bail!("git config returned invalid file entries for {key}");
    }
    Ok(fields
        .chunks_exact(2)
        .map(|fields| ConfigEntry {
            origin: fields[0].clone(),
            scope: None,
            value: fields[1].clone(),
            name: None,
        })
        .collect())
}

fn direct_file_config_matching_entries(
    execution: &Execution<'_>,
    config: &Path,
    pattern: &str,
) -> anyhow::Result<Vec<ConfigEntry>> {
    let output = execution.git_allow_owned(
        &[
            "config".into(),
            "--file".into(),
            config.as_os_str().to_owned(),
            "--no-includes".into(),
            "--null".into(),
            "--show-origin".into(),
            "--get-regexp".into(),
            pattern.into(),
        ],
        &[1],
        &format!("reading configuration matching {pattern}"),
    )?;
    let fields = nul_values(&output)?;
    if fields.len() % 2 != 0 {
        anyhow::bail!("git config returned invalid matching file entries for {pattern}");
    }
    fields
        .chunks_exact(2)
        .map(|fields| {
            let (name, value) = fields[1].split_once('\n').ok_or_else(|| {
                anyhow::anyhow!("git config returned an invalid name and value for {pattern}")
            })?;
            Ok(ConfigEntry {
                origin: fields[0].clone(),
                scope: None,
                value: value.to_owned(),
                name: Some(name.to_owned()),
            })
        })
        .collect()
}

fn effective_config_entry(
    execution: &Execution<'_>,
    key: &str,
) -> anyhow::Result<Option<ConfigEntry>> {
    let output = execution.git_allow(
        &[
            "config",
            "--null",
            "--show-scope",
            "--show-origin",
            "--get",
            key,
        ],
        &[1],
        &format!("reading effective {key}"),
    )?;
    let fields = nul_values(&output)?;
    if fields.is_empty() {
        return Ok(None);
    }
    if fields.len() != 3 {
        anyhow::bail!("git config returned an invalid scoped value for {key}");
    }
    Ok(Some(ConfigEntry {
        scope: Some(fields[0].clone()),
        origin: fields[1].clone(),
        value: fields[2].clone(),
        name: None,
    }))
}

fn has_direct_config_entries(execution: &Execution<'_>, config: &Path) -> anyhow::Result<bool> {
    let output = execution.git_success_owned(
        &[
            "config".into(),
            "--file".into(),
            config.as_os_str().to_owned(),
            "--no-includes".into(),
            "--null".into(),
            "--list".into(),
        ],
        "checking direct worktree configuration",
    )?;
    Ok(!output.stdout.is_empty())
}

fn nul_values(output: &CapturedCommand) -> anyhow::Result<Vec<String>> {
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    let output = String::from_utf8(output.stdout.clone())?;
    if output.is_empty() {
        return Ok(vec![String::new()]);
    }
    Ok(output
        .strip_suffix('\0')
        .unwrap_or(&output)
        .split('\0')
        .map(str::to_owned)
        .collect())
}

fn assert_single(values: Vec<String>, key: &str) -> anyhow::Result<Option<String>> {
    if values.len() > 1 {
        anyhow::bail!("multiple {key} values are not supported");
    }
    Ok(values.into_iter().next())
}

fn config_source(entry: &ConfigEntry) -> String {
    format!(
        "{}: {}",
        entry.origin,
        serde_json::to_string(&entry.value).unwrap_or_else(|_| "\"\"".to_owned())
    )
}

fn config_origin_path(origin: &str, root: &Path) -> Option<PathBuf> {
    let path = Path::new(origin.strip_prefix("file:")?);
    Some(if path.is_absolute() {
        path.clean()
    } else {
        root.join(path).clean()
    })
}

fn origin_is_file(origin: &str, root: &Path, config: &Path) -> bool {
    config_origin_path(origin, root)
        .is_some_and(|origin| normalized_path(&origin) == normalized_path(config))
}

fn refuse_scoped_hooks_path(entry: &ConfigEntry) -> anyhow::Result<()> {
    match entry.scope.as_deref() {
        Some("command") => anyhow::bail!(
            "refusing to replace command-scoped core.hooksPath ({}); {ALLOW_HOOKS_PATH_OVERRIDE} cannot override transient command configuration",
            config_source(entry)
        ),
        Some("worktree") => anyhow::bail!(
            "refusing to replace worktree-scoped core.hooksPath ({}); a worktree-specific custom path must be integrated or removed explicitly",
            config_source(entry)
        ),
        scope => anyhow::bail!(
            "refusing to replace core.hooksPath from unsupported {} scope ({})",
            scope.unwrap_or("unknown"),
            config_source(entry)
        ),
    }
}

struct Execution<'a> {
    root: &'a Path,
    environment: &'a BTreeMap<OsString, OsString>,
}

impl<'a> Execution<'a> {
    fn new(root: &'a Path, environment: &'a BTreeMap<OsString, OsString>) -> Self {
        Self { root, environment }
    }

    fn git_success(&self, args: &[&str], operation: &str) -> anyhow::Result<CapturedCommand> {
        let output = self.capture(
            OsStr::new("git"),
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            None,
        )?;
        require_success(output, "git", args, operation)
    }

    fn git_success_owned(
        &self,
        args: &[OsString],
        operation: &str,
    ) -> anyhow::Result<CapturedCommand> {
        let output = self.capture(OsStr::new("git"), args, None)?;
        require_success_os(output, "git", args, operation)
    }

    fn git_allow(
        &self,
        args: &[&str],
        allowed: &[i32],
        operation: &str,
    ) -> anyhow::Result<CapturedCommand> {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        self.git_allow_owned(&args, allowed, operation)
    }

    fn git_allow_owned(
        &self,
        args: &[OsString],
        allowed: &[i32],
        operation: &str,
    ) -> anyhow::Result<CapturedCommand> {
        let output = self.capture(OsStr::new("git"), args, None)?;
        if output.status.success()
            || output
                .status
                .code()
                .is_some_and(|code| allowed.contains(&code))
        {
            Ok(output)
        } else {
            anyhow::bail!(
                "{operation}: git {} failed: {}",
                display_args(args),
                command_failure_detail(&output)
            )
        }
    }

    fn capture(
        &self,
        command: &OsStr,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> anyhow::Result<CapturedCommand> {
        capture_command(command, args, cwd.unwrap_or(self.root), self.environment)
    }
}

fn discover_repository_root(
    cwd: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> anyhow::Result<Option<PathBuf>> {
    let output = capture_command(
        OsStr::new("git"),
        &["rev-parse".into(), "--show-toplevel".into()],
        cwd,
        environment,
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(strip_git_line_terminator(
        &output.stdout,
    ))))
}

fn capture_command(
    command: &OsStr,
    args: &[OsString],
    cwd: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> anyhow::Result<CapturedCommand> {
    let output = Command::new(command)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            anyhow::anyhow!(
                "{} {} failed: {error}",
                command.to_string_lossy(),
                display_args(args)
            )
        })?;
    Ok(CapturedCommand {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn require_success(
    output: CapturedCommand,
    command: &str,
    args: &[&str],
    operation: &str,
) -> anyhow::Result<CapturedCommand> {
    if output.status.success() {
        Ok(output)
    } else {
        anyhow::bail!(
            "{operation}: {command} {} failed: {}",
            args.join(" "),
            command_failure_detail(&output)
        )
    }
}

fn require_success_os(
    output: CapturedCommand,
    command: &str,
    args: &[OsString],
    operation: &str,
) -> anyhow::Result<CapturedCommand> {
    if output.status.success() {
        Ok(output)
    } else {
        anyhow::bail!(
            "{operation}: {command} {} failed: {}",
            display_args(args),
            command_failure_detail(&output)
        )
    }
}

fn command_failure_detail(output: &CapturedCommand) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("exit status {}", status_text(output.status))
    } else {
        stderr
    }
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_git_line_terminator(output: &[u8]) -> String {
    let mut output = String::from_utf8_lossy(output).into_owned();
    if output.ends_with('\n') {
        output.pop();
    }
    if cfg!(windows) && output.ends_with('\r') {
        output.pop();
    }
    output
}

fn sibling_binary(name: &str) -> anyhow::Result<PathBuf> {
    let mut path = std::env::current_exe()?;
    path.set_file_name(name);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    Ok(path)
}

fn environment_value<'a>(
    environment: &'a BTreeMap<OsString, OsString>,
    key: &str,
) -> Option<&'a str> {
    environment
        .iter()
        .find(|(candidate, _)| candidate.to_string_lossy().eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.to_str())
}

fn environment_delay(
    environment: &BTreeMap<OsString, OsString>,
    key: &str,
) -> anyhow::Result<Duration> {
    let Some(value) = environment_value(environment, key) else {
        return Ok(Duration::ZERO);
    };
    Ok(Duration::from_millis(value.parse()?))
}

fn symlink_metadata_if_present(path: &Path) -> anyhow::Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> anyhow::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if identity.inode == 0 {
        anyhow::bail!("file metadata has no stable inode");
    }
    Ok(identity)
}

#[cfg(windows)]
fn file_identity(metadata: &Metadata) -> anyhow::Result<FileIdentity> {
    use std::os::windows::fs::MetadataExt as _;

    Ok(FileIdentity {
        device: u64::from(
            metadata
                .volume_serial_number()
                .ok_or_else(|| anyhow::anyhow!("file metadata has no volume serial number"))?,
        ),
        inode: metadata
            .file_index()
            .ok_or_else(|| anyhow::anyhow!("file metadata has no file index"))?,
    })
}

#[cfg(unix)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;

    metadata.nlink()
}

#[cfg(windows)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt as _;

    metadata.number_of_links().map_or(0, u64::from)
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_new_private_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content)?;
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn normalized_path(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.clean()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
            .clean()
    };
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path
    }
}

fn json_path(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).unwrap_or_else(|_| "\"\"".to_owned())
}

fn status_text(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "null".to_owned(), |code| code.to_string())
}
