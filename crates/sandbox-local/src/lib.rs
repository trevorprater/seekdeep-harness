//! Native local sandbox ladder: bwrap, Landlock, Seatbelt, and Windows ACL invocation.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_landlock_run::{
    LAUNCHER_BIN, LAUNCHER_FAILURE_EXIT, LandlockEnforcement, LauncherGrants,
    grant_args as landlock_grant_args, launcher_path as landlock_launcher_path,
    probe as probe_landlock,
};
use seekdeep_sandbox::{
    ConfinedArgv, ConfinedSandboxMode, RunnerFailureRule, SandboxEnforcement, SandboxPolicy,
    SandboxProvider, SandboxService, SandboxUnavailableError, writable_roots,
};
#[cfg(windows)]
use seekdeep_sandbox_windows_acl::{AclWriteGrant, GrantBindings};
use seekdeep_sandbox_windows_acl::{
    assert_temp_root_outside_workspace, temp_write_sid, workspace_write_sid,
};
use serde::{Deserialize, Serialize};

pub mod invariant;

/// Cordis plugin name.
pub const NAME: &str = "sandbox-local";
/// This provider has no mandatory services.
pub const INJECT: &[&str] = &[];
const WINDOWS_ACL_RUNNER_FAILURE_EXIT: i32 = 127;

fn default_probe_timeout_ms() -> f64 {
    5_000.0
}

/// Local provider configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalSandboxConfig {
    /// Operator-selected bwrap-compatible runner prefix; empty selects the platform ladder.
    pub runner_command: Vec<String>,
    /// Runner-owned fatal diagnostic substrings required with a custom runner.
    pub runner_failure_signatures: Vec<String>,
    /// Positive finite safe-integer bound for each functional probe.
    #[serde(default = "default_probe_timeout_ms")]
    pub probe_timeout_ms: f64,
}

impl Default for LocalSandboxConfig {
    fn default() -> Self {
        Self {
            runner_command: Vec::new(),
            runner_failure_signatures: Vec::new(),
            probe_timeout_ms: default_probe_timeout_ms(),
        }
    }
}

impl LocalSandboxConfig {
    /// Validates dependent config fields and resolves the timeout.
    ///
    /// # Errors
    ///
    /// Returns source-compatible configuration diagnostics.
    pub fn resolve(&self) -> anyhow::Result<ResolvedLocalSandboxConfig> {
        anyhow::ensure!(
            !self.runner_command.is_empty() || self.runner_failure_signatures.is_empty(),
            "sandbox-local: runnerFailureSignatures requires runnerCommand"
        );
        anyhow::ensure!(
            self.runner_command.is_empty() || !self.runner_failure_signatures.is_empty(),
            "sandbox-local: runnerCommand requires at least one runnerFailureSignatures entry"
        );
        anyhow::ensure!(
            self.runner_failure_signatures
                .iter()
                .all(|signature| !signature.trim().is_empty() && !signature.contains(['\r', '\n'])),
            "sandbox-local: runnerFailureSignatures entries must be non-empty single-line strings"
        );
        anyhow::ensure!(
            self.probe_timeout_ms.is_finite()
                && self.probe_timeout_ms > 0.0
                && self.probe_timeout_ms.fract() == 0.0
                && self.probe_timeout_ms <= 9_007_199_254_740_991.0,
            "sandbox-local: probeTimeoutMs must be a positive finite number"
        );
        Ok(ResolvedLocalSandboxConfig {
            runner_command: (!self.runner_command.is_empty()).then(|| self.runner_command.clone()),
            runner_failure_signatures: self.runner_failure_signatures.clone(),
            probe_timeout: Duration::try_from_secs_f64(self.probe_timeout_ms / 1_000.0).map_err(
                |_| {
                    anyhow::anyhow!(
                        "sandbox-local: probeTimeoutMs must be a positive finite number"
                    )
                },
            )?,
        })
    }
}

/// Validated provider configuration.
#[derive(Clone, Debug)]
pub struct ResolvedLocalSandboxConfig {
    runner_command: Option<Vec<String>>,
    runner_failure_signatures: Vec<String>,
    probe_timeout: Duration,
}

/// Closed local runner vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalSandboxRunner {
    /// Linux bubblewrap mount namespace.
    Bwrap,
    /// Linux Landlock self-restricting launcher.
    Landlock,
    /// macOS Seatbelt through `sandbox-exec`.
    Seatbelt,
    /// Windows restricted token and ACL runner.
    WindowsAcl,
}

type BoolProbe = Arc<dyn Fn() -> bool + Send + Sync>;
type LandlockProbe = Arc<dyn Fn(&Path) -> LandlockEnforcement + Send + Sync>;

/// Provider-local abstraction over one standing or revocable ACL capability.
///
/// The native implementation delegates to the compiled Rust Win32 adapter;
/// the public seam lets portable differential tests drive the same ownership
/// state machine without making real ACL changes.
pub trait LocalAclWriteGrant: std::fmt::Debug + Send {
    /// Adds the capability ACE to one path.
    ///
    /// # Errors
    ///
    /// Returns the native or injected ACL mutation failure.
    fn add(&mut self, path: &Path, standing: bool) -> anyhow::Result<()>;

    /// Revokes revocable ACEs and releases the parsed SID allocation.
    ///
    /// # Errors
    ///
    /// Returns aggregate native or injected cleanup failures.
    fn dispose(self: Box<Self>) -> anyhow::Result<()>;
}

/// Injectable constructor for a provider-owned ACL write grant.
pub type AclGrantFactory =
    Arc<dyn Fn(&str) -> anyhow::Result<Box<dyn LocalAclWriteGrant>> + Send + Sync>;
/// Injectable random private-temp directory constructor.
pub type TempDirFactory = Arc<dyn Fn(&Path) -> anyhow::Result<PathBuf> + Send + Sync>;
/// Injectable provider-owned private-temp directory remover.
pub type TempDirRemover = Arc<dyn Fn(&Path) -> anyhow::Result<()> + Send + Sync>;
/// Injectable cleanup-warning sink.
pub type CleanupWarning = Arc<dyn Fn(&str) + Send + Sync>;

/// Deterministic platform and probe seams for differential tests.
#[derive(Clone, Default)]
pub struct SandboxInternals {
    /// Source platform spelling (`linux`, `darwin`, `win32`).
    pub platform: Option<String>,
    /// Replaces the platform ladder wholesale.
    pub chain: Option<Vec<LocalSandboxRunner>>,
    /// Replaces the bwrap functional probe.
    pub probe_bwrap: Option<BoolProbe>,
    /// Replaces the Landlock launcher probe.
    pub probe_landlock: Option<LandlockProbe>,
    /// Replaces the Seatbelt functional probe.
    pub probe_seatbelt: Option<BoolProbe>,
    /// Replaces the Windows ACL functional probe.
    pub probe_windows_acl: Option<BoolProbe>,
    /// Replaces the Landlock launcher path.
    pub landlock_launcher: Option<PathBuf>,
    /// Replaces `sandbox-exec`.
    pub seatbelt_exec: Option<String>,
    /// Replaces the Windows ACL runner argv prefix.
    pub windows_acl_runner_args: Option<Vec<String>>,
    /// Replaces native ACL-grant construction for portable lifecycle tests.
    pub acl_grant_factory: Option<AclGrantFactory>,
    /// Replaces random private-temp creation for failure-path tests.
    pub create_temp_dir: Option<TempDirFactory>,
    /// Replaces recursive private-temp removal for failure-path tests.
    pub remove_temp_dir: Option<TempDirRemover>,
    /// Receives teardown warnings; the default writes them to stderr.
    pub cleanup_warning: Option<CleanupWarning>,
}

impl std::fmt::Debug for SandboxInternals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxInternals")
            .field("platform", &self.platform)
            .field("chain", &self.chain)
            .field("landlock_launcher", &self.landlock_launcher)
            .field("seatbelt_exec", &self.seatbelt_exec)
            .field("windows_acl_runner_args", &self.windows_acl_runner_args)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
struct SelectedRunner {
    runner: LocalSandboxRunner,
    enforcement: SandboxEnforcement,
}

#[derive(Clone, Copy, Debug)]
enum CachedVerdict {
    Selected(SelectedRunner),
    Unavailable,
}

#[derive(Debug)]
struct AclTempCapability {
    dir: PathBuf,
    write_sid: String,
    grant: Box<dyn LocalAclWriteGrant>,
}

#[derive(Debug, Default)]
struct WindowsAclState {
    workspace_grants: HashMap<PathBuf, Box<dyn LocalAclWriteGrant>>,
    temp_capabilities: HashMap<(String, PathBuf), AclTempCapability>,
}

#[cfg(windows)]
#[derive(Debug)]
struct SafeAclWriteGrant(Option<AclWriteGrant>);

#[cfg(windows)]
impl LocalAclWriteGrant for SafeAclWriteGrant {
    fn add(&mut self, path: &Path, standing: bool) -> anyhow::Result<()> {
        self.0
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ACL write grant was already disposed"))?
            .add(path, standing)?;
        Ok(())
    }

    fn dispose(mut self: Box<Self>) -> anyhow::Result<()> {
        self.0
            .take()
            .ok_or_else(|| anyhow::anyhow!("ACL write grant was already disposed"))?
            .dispose()?;
        Ok(())
    }
}

/// Native local process-sandbox provider.
#[derive(Debug)]
pub struct LocalSandboxProvider {
    config: ResolvedLocalSandboxConfig,
    internals: Mutex<SandboxInternals>,
    selected: Mutex<Option<CachedVerdict>>,
    windows_acl: Mutex<WindowsAclState>,
}

impl LocalSandboxProvider {
    /// Creates an unregistered provider after early configuration validation.
    ///
    /// # Errors
    ///
    /// Returns invalid dependent config or probe bounds.
    pub fn new(config: &LocalSandboxConfig) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            config: config.resolve()?,
            internals: Mutex::new(SandboxInternals::default()),
            selected: Mutex::new(None),
            windows_acl: Mutex::new(WindowsAclState::default()),
        }))
    }

    /// Replaces test internals without changing an already cached verdict.
    pub fn set_internals(&self, internals: SandboxInternals) {
        *self.internals.lock() = internals;
    }

    fn selected_runner(&self, mode: ConfinedSandboxMode) -> anyhow::Result<SelectedRunner> {
        let verdict = {
            let mut selected = self.selected.lock();
            *selected.get_or_insert_with(|| self.chain_verdict())
        };
        match verdict {
            CachedVerdict::Selected(selected) => Ok(selected),
            CachedVerdict::Unavailable => {
                Err(anyhow::Error::new(SandboxUnavailableError::new(mode, None)))
            }
        }
    }

    fn chain_verdict(&self) -> CachedVerdict {
        let internals = self.internals.lock().clone();
        let chain = internals
            .chain
            .unwrap_or_else(|| platform_chain(internals.platform.as_deref()));
        let Some((&first, rest)) = chain.split_first() else {
            return CachedVerdict::Unavailable;
        };
        if rest.is_empty() {
            return CachedVerdict::Selected(SelectedRunner {
                runner: first,
                enforcement: static_enforcement(first),
            });
        }
        chain
            .into_iter()
            .find_map(|runner| {
                self.probe_runner(runner).map(|enforcement| {
                    CachedVerdict::Selected(SelectedRunner {
                        runner,
                        enforcement,
                    })
                })
            })
            .unwrap_or(CachedVerdict::Unavailable)
    }

    fn probe_runner(&self, runner: LocalSandboxRunner) -> Option<SandboxEnforcement> {
        let internals = self.internals.lock().clone();
        match runner {
            LocalSandboxRunner::Bwrap => internals
                .probe_bwrap
                .map_or_else(
                    || default_probe_bwrap(self.config.probe_timeout),
                    |probe| probe(),
                )
                .then_some(SandboxEnforcement::Full),
            LocalSandboxRunner::Landlock => {
                let launcher = Self::landlock_launcher(&internals);
                let verdict = internals.probe_landlock.map_or_else(
                    || probe_landlock(&launcher, self.config.probe_timeout),
                    |probe| probe(&launcher),
                );
                match verdict {
                    LandlockEnforcement::Full => Some(SandboxEnforcement::Full),
                    LandlockEnforcement::Partial => Some(SandboxEnforcement::Partial),
                    LandlockEnforcement::Unusable => None,
                }
            }
            LocalSandboxRunner::Seatbelt => internals
                .probe_seatbelt
                .map_or_else(
                    || {
                        default_probe_seatbelt(
                            internals.seatbelt_exec.as_deref().unwrap_or("sandbox-exec"),
                            self.config.probe_timeout,
                        )
                    },
                    |probe| probe(),
                )
                .then_some(SandboxEnforcement::Full),
            LocalSandboxRunner::WindowsAcl => internals
                .probe_windows_acl
                .clone()
                .map_or_else(
                    || {
                        default_probe_windows_acl(
                            &Self::windows_acl_invocation(&internals),
                            self.config.probe_timeout,
                        )
                    },
                    |probe| probe(),
                )
                .then_some(SandboxEnforcement::Partial),
        }
    }

    fn landlock_launcher(internals: &SandboxInternals) -> PathBuf {
        internals.landlock_launcher.clone().unwrap_or_else(|| {
            landlock_launcher_path().unwrap_or_else(|_| PathBuf::from(LAUNCHER_BIN))
        })
    }

    fn windows_acl_invocation(internals: &SandboxInternals) -> Vec<String> {
        if let Some(invocation) = &internals.windows_acl_runner_args {
            return invocation.clone();
        }
        std::env::current_exe()
            .ok()
            .and_then(|executable| executable.parent().map(Path::to_owned))
            .map_or_else(
                || vec!["windows-acl-run".to_owned()],
                |directory| {
                    vec![
                        directory
                            .join("windows-acl-run")
                            .to_string_lossy()
                            .into_owned(),
                    ]
                },
            )
    }

    #[cfg_attr(windows, allow(clippy::unnecessary_wraps))]
    fn acl_grant_factory(internals: &SandboxInternals) -> anyhow::Result<AclGrantFactory> {
        if let Some(factory) = &internals.acl_grant_factory {
            return Ok(factory.clone());
        }
        #[cfg(windows)]
        {
            let bindings: Arc<dyn GrantBindings> =
                Arc::new(seekdeep_sandbox_windows_acl_native::WindowsBindings);
            Ok(Arc::new(move |write_sid| {
                Ok(Box::new(SafeAclWriteGrant(Some(AclWriteGrant::create(
                    write_sid,
                    bindings.clone(),
                )?))))
            }))
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!(
                "sandbox-local: windows-acl session grants are unavailable until the native windows-acl provider is installed"
            )
        }
    }

    fn create_temp_dir(internals: &SandboxInternals, root: &Path) -> anyhow::Result<PathBuf> {
        if let Some(create) = &internals.create_temp_dir {
            return create(root);
        }
        Ok(tempfile::Builder::new()
            .prefix("seekdeep-")
            .tempdir_in(root)?
            .keep())
    }

    fn remove_temp_dir(internals: &SandboxInternals, path: &Path) -> anyhow::Result<()> {
        if let Some(remove) = &internals.remove_temp_dir {
            return remove(path);
        }
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn aggregate_materialization_error(
        primary: anyhow::Error,
        cleanup: Vec<anyhow::Error>,
        message: &str,
    ) -> anyhow::Error {
        if cleanup.is_empty() {
            return primary;
        }
        let mut failures = vec![format!("{primary:#}")];
        failures.extend(cleanup.into_iter().map(|error| format!("{error:#}")));
        anyhow::anyhow!("{message}: {}", failures.join("; "))
    }

    fn materialize_acl_grant(
        &self,
        session_id: &str,
        workspace_root: &Path,
        internals: &SandboxInternals,
    ) -> anyhow::Result<(PathBuf, String)> {
        let temp_root = std::env::temp_dir();
        assert_temp_root_outside_workspace(workspace_root, &temp_root)?;
        let factory = Self::acl_grant_factory(internals)?;
        let workspace_text = path_string(workspace_root)?;
        let workspace_sid = workspace_write_sid(&workspace_text);
        let mut state = self.windows_acl.lock();

        if !state.workspace_grants.contains_key(workspace_root) {
            let mut grant = factory(&workspace_sid)?;
            if let Err(error) = grant.add(workspace_root, true) {
                let cleanup = grant.dispose().err().into_iter().collect();
                return Err(Self::aggregate_materialization_error(
                    error,
                    cleanup,
                    "sandbox-local windows-acl workspace grant failed and its cleanup also failed",
                ));
            }
            state
                .workspace_grants
                .insert(workspace_root.to_owned(), grant);
        }

        let key = (session_id.to_owned(), workspace_root.to_owned());
        if let Some(existing) = state.temp_capabilities.get(&key) {
            return Ok((existing.dir.clone(), existing.write_sid.clone()));
        }

        let temp_dir = Self::create_temp_dir(internals, &temp_root)?;
        let temp_text = match path_string(&temp_dir) {
            Ok(path) => path,
            Err(error) => {
                let cleanup = Self::remove_temp_dir(internals, &temp_dir)
                    .err()
                    .into_iter()
                    .collect();
                return Err(Self::aggregate_materialization_error(
                    error,
                    cleanup,
                    "sandbox-local windows-acl temp grant materialization failed and its cleanup also failed",
                ));
            }
        };
        let temp_sid = temp_write_sid(&temp_text);
        let mut grant = match factory(&temp_sid) {
            Ok(grant) => Some(grant),
            Err(error) => {
                let cleanup = Self::remove_temp_dir(internals, &temp_dir)
                    .err()
                    .into_iter()
                    .collect();
                return Err(Self::aggregate_materialization_error(
                    error,
                    cleanup,
                    "sandbox-local windows-acl temp grant materialization failed and its cleanup also failed",
                ));
            }
        };
        if let Err(error) = grant
            .as_mut()
            .expect("grant was just created")
            .add(&temp_dir, false)
        {
            let mut cleanup = Vec::new();
            if let Some(grant) = grant.take()
                && let Err(error) = grant.dispose()
            {
                cleanup.push(error);
            }
            if let Err(error) = Self::remove_temp_dir(internals, &temp_dir) {
                cleanup.push(error);
            }
            return Err(Self::aggregate_materialization_error(
                error,
                cleanup,
                "sandbox-local windows-acl temp grant materialization failed and its cleanup also failed",
            ));
        }
        state.temp_capabilities.insert(
            key,
            AclTempCapability {
                dir: temp_dir.clone(),
                write_sid: temp_sid.clone(),
                grant: grant.expect("successful grant remains owned"),
            },
        );
        Ok((temp_dir, temp_sid))
    }

    fn revoke_acl_grants(&self) {
        let internals = self.internals.lock().clone();
        let state = std::mem::take(&mut *self.windows_acl.lock());
        if state.workspace_grants.is_empty() && state.temp_capabilities.is_empty() {
            return;
        }
        let mut failures = Vec::new();
        let mut temp_dirs = Vec::with_capacity(state.temp_capabilities.len());
        for grant in state.workspace_grants.into_values() {
            if let Err(error) = grant.dispose() {
                failures.push(format!("{error:#}"));
            }
        }
        for capability in state.temp_capabilities.into_values() {
            temp_dirs.push(capability.dir);
            if let Err(error) = capability.grant.dispose() {
                failures.push(format!("{error:#}"));
            }
        }
        for dir in temp_dirs {
            if let Err(error) = Self::remove_temp_dir(&internals, &dir) {
                failures.push(format!("{error:#}"));
            }
        }
        if !failures.is_empty() {
            let warning = internals
                .cleanup_warning
                .unwrap_or_else(|| Arc::new(|message| eprintln!("{message}")));
            warning(&format!(
                "sandbox-local: windows-acl grant cleanup completed with {} failure(s)",
                failures.len()
            ));
            for failure in failures {
                warning(&failure);
            }
        }
    }

    fn runner_argv(
        &self,
        selected: SelectedRunner,
        policy: &SandboxPolicy,
    ) -> anyhow::Result<Vec<String>> {
        let internals = self.internals.lock().clone();
        match selected.runner {
            LocalSandboxRunner::Bwrap => Ok(std::iter::once("bwrap".to_owned())
                .chain(bwrap_profile_args(policy)?)
                .collect()),
            LocalSandboxRunner::Landlock => Ok(std::iter::once(
                Self::landlock_launcher(&internals)
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("landlock launcher path is not valid Unicode"))?
                    .to_owned(),
            )
            .chain(landlock_profile_args(policy)?)
            .collect()),
            LocalSandboxRunner::Seatbelt => Ok(std::iter::once(
                internals
                    .seatbelt_exec
                    .unwrap_or_else(|| "sandbox-exec".to_owned()),
            )
            .chain(seatbelt_profile_args(policy)?)
            .collect()),
            LocalSandboxRunner::WindowsAcl => {
                let root = path_string(&policy.workspace_root)?;
                let mut invocation = Self::windows_acl_invocation(&internals);
                let (temp, capability_args) = if policy.mode == ConfinedSandboxMode::WorkspaceWrite
                    && let Some(session_id) = &policy.session_id
                {
                    let (temp, temp_sid) = self.materialize_acl_grant(
                        session_id.as_str(),
                        &policy.workspace_root,
                        &internals,
                    )?;
                    (
                        path_string(&temp)?,
                        vec![
                            "--write-sid".to_owned(),
                            workspace_write_sid(&root),
                            "--temp-write-sid".to_owned(),
                            temp_sid,
                        ],
                    )
                } else {
                    (
                        std::env::temp_dir().to_string_lossy().into_owned(),
                        Vec::new(),
                    )
                };
                invocation.extend([
                    "--workspace".to_owned(),
                    root,
                    "--temp".to_owned(),
                    temp,
                    "--mode".to_owned(),
                    policy.mode.as_str().to_owned(),
                ]);
                invocation.extend(capability_args);
                Ok(invocation)
            }
        }
    }
}

impl SandboxProvider for LocalSandboxProvider {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        if let Some(runner) = &self.config.runner_command {
            let mut wrapped = runner.clone();
            wrapped.extend(bwrap_profile_args(policy)?);
            wrapped.push("--".to_owned());
            wrapped.extend_from_slice(argv);
            return Ok(ConfinedArgv {
                argv: wrapped,
                enforcement: SandboxEnforcement::Full,
                denial_signatures: vec![
                    "read-only file system".to_owned(),
                    "permission denied".to_owned(),
                ],
                runner_failure_rules: vec![RunnerFailureRule {
                    allowed_exit_codes: None,
                    fatal_signatures: self.config.runner_failure_signatures.clone(),
                    informational_lines: None,
                }],
            });
        }
        let selected = self.selected_runner(policy.mode)?;
        let mut wrapped = self.runner_argv(selected, policy)?;
        wrapped.push("--".to_owned());
        wrapped.extend_from_slice(argv);
        Ok(ConfinedArgv {
            argv: wrapped,
            enforcement: selected.enforcement,
            denial_signatures: denial_signatures(selected.runner),
            runner_failure_rules: runner_failure_rules(selected.runner),
        })
    }
}

fn platform_chain(override_platform: Option<&str>) -> Vec<LocalSandboxRunner> {
    match override_platform.unwrap_or(match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        platform => platform,
    }) {
        "linux" => vec![LocalSandboxRunner::Bwrap, LocalSandboxRunner::Landlock],
        "darwin" => vec![LocalSandboxRunner::Seatbelt],
        "win32" => vec![LocalSandboxRunner::WindowsAcl],
        _ => Vec::new(),
    }
}

const fn static_enforcement(runner: LocalSandboxRunner) -> SandboxEnforcement {
    match runner {
        LocalSandboxRunner::Bwrap | LocalSandboxRunner::Landlock | LocalSandboxRunner::Seatbelt => {
            SandboxEnforcement::Full
        }
        LocalSandboxRunner::WindowsAcl => SandboxEnforcement::Partial,
    }
}

fn denial_signatures(runner: LocalSandboxRunner) -> Vec<String> {
    match runner {
        LocalSandboxRunner::Bwrap => vec!["read-only file system".to_owned()],
        LocalSandboxRunner::Landlock => vec!["permission denied".to_owned()],
        LocalSandboxRunner::Seatbelt => vec!["operation not permitted".to_owned()],
        LocalSandboxRunner::WindowsAcl => vec![
            "access is denied".to_owned(),
            "access to the path".to_owned(),
            "permission denied".to_owned(),
        ],
    }
}

fn runner_failure_rules(runner: LocalSandboxRunner) -> Vec<RunnerFailureRule> {
    match runner {
        LocalSandboxRunner::Bwrap => vec![RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: vec!["bwrap: ".to_owned()],
            informational_lines: None,
        }],
        LocalSandboxRunner::Landlock => vec![RunnerFailureRule {
            allowed_exit_codes: Some(vec![LAUNCHER_FAILURE_EXIT]),
            fatal_signatures: vec![format!("{LAUNCHER_BIN}: ")],
            informational_lines: Some(vec![format!(
                "{LAUNCHER_BIN}: partial enforcement (older Landlock ABI)"
            )]),
        }],
        LocalSandboxRunner::Seatbelt => vec![RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: vec!["sandbox-exec: ".to_owned()],
            informational_lines: None,
        }],
        LocalSandboxRunner::WindowsAcl => vec![RunnerFailureRule {
            allowed_exit_codes: Some(vec![WINDOWS_ACL_RUNNER_FAILURE_EXIT]),
            fatal_signatures: vec!["windows-acl-run: ".to_owned()],
            informational_lines: None,
        }],
    }
}

fn path_string(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("sandbox policy path is not valid Unicode"))
}

/// Builds bwrap profile arguments before the command separator.
///
/// # Errors
///
/// Returns when the policy root is not valid Unicode.
pub fn bwrap_profile_args(policy: &SandboxPolicy) -> anyhow::Result<Vec<String>> {
    let mut args = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]
    .map(str::to_owned)
    .to_vec();
    if policy.mode == ConfinedSandboxMode::WorkspaceWrite {
        let root = path_string(&policy.workspace_root)?;
        args.extend([
            "--tmpfs".to_owned(),
            "/tmp".to_owned(),
            "--bind".to_owned(),
            root.clone(),
            root,
        ]);
    }
    Ok(args)
}

/// Builds `landlock-run` grant arguments before the command separator.
///
/// # Errors
///
/// Returns when a policy path is not valid Unicode.
pub fn landlock_profile_args(policy: &SandboxPolicy) -> anyhow::Result<Vec<String>> {
    let mut read_write = vec![PathBuf::from("/dev/null")];
    if policy.mode == ConfinedSandboxMode::WorkspaceWrite {
        read_write.extend([PathBuf::from("/tmp"), policy.workspace_root.clone()]);
    }
    landlock_grant_args(&LauncherGrants {
        read_only: vec![PathBuf::from("/")],
        read_write,
    })
    .into_iter()
    .map(|arg| {
        arg.into_string()
            .map_err(|_| anyhow::anyhow!("sandbox policy path is not valid Unicode"))
    })
    .collect()
}

fn sbpl_string(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Builds `sandbox-exec -p <SBPL>` arguments before the command separator.
///
/// # Errors
///
/// Returns when a canonical writable root is not valid Unicode.
pub fn seatbelt_profile_args(policy: &SandboxPolicy) -> anyhow::Result<Vec<String>> {
    let mut forms = vec![
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        "(deny file-write*)".to_owned(),
        format!("(allow file-write* (literal {}))", sbpl_string("/dev/null")),
    ];
    let roots = writable_roots(&seekdeep_sandbox::SandboxExecutionPolicy {
        mode: policy.mode.into(),
        workspace_root: policy.workspace_root.clone(),
        session_id: policy.session_id.clone(),
    });
    if !roots.is_empty() {
        let grants = roots
            .iter()
            .map(|root| {
                Ok(format!(
                    "(subpath {})",
                    sbpl_string(root.to_str().ok_or_else(|| anyhow::anyhow!(
                        "sandbox policy path is not valid Unicode"
                    ))?)
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        forms.push(format!("(allow file-write* {})", grants.join(" ")));
    }
    Ok(vec!["-p".to_owned(), forms.join(" ")])
}

fn probe_command(program: &str, args: &[String], timeout: Duration) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now().checked_add(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn default_probe_bwrap(timeout: Duration) -> bool {
    probe_command(
        "bwrap",
        &[
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--die-with-parent",
            "--",
            "true",
        ]
        .map(str::to_owned),
        timeout,
    )
}

fn default_probe_seatbelt(executable: &str, timeout: Duration) -> bool {
    let policy = SandboxPolicy {
        mode: ConfinedSandboxMode::ReadOnly,
        workspace_root: PathBuf::from("/"),
        session_id: None,
    };
    let Ok(mut args) = seatbelt_profile_args(&policy) else {
        return false;
    };
    args.extend(["--".to_owned(), "true".to_owned()]);
    probe_command(executable, &args, timeout)
}

fn default_probe_windows_acl(invocation: &[String], timeout: Duration) -> bool {
    let Some((program, prefix)) = invocation.split_first() else {
        return false;
    };
    let mut args = prefix.to_vec();
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    args.extend([
        "--workspace".to_owned(),
        temp.clone(),
        "--temp".to_owned(),
        temp,
        "--mode".to_owned(),
        "read-only".to_owned(),
        "--".to_owned(),
        "cmd".to_owned(),
        "/c".to_owned(),
        "exit".to_owned(),
        "0".to_owned(),
    ]);
    probe_command(program, &args, timeout)
}

/// Installed provider and its reversible Cordis registration.
pub struct LocalSandboxInstallation {
    /// Exact installed provider.
    pub provider: Arc<LocalSandboxProvider>,
    effect: EffectHandle,
    cleanup: EffectHandle,
}

impl LocalSandboxInstallation {
    /// Removes only this provider contribution.
    ///
    /// # Errors
    ///
    /// Returns Cordis teardown failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let cleanup = self.cleanup.dispose().await;
        let service = self.effect.dispose().await;
        match (cleanup, service) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(cleanup), Err(service)) => Err(anyhow::anyhow!(
                "sandbox-local cleanup failed: {cleanup:#}\nsandbox service cleanup failed: {service:#}"
            )),
        }
    }
}

/// Installs the configured provider.
///
/// # Errors
///
/// Returns configuration, duplicate service, or ownership failures.
pub fn install(
    context: &Context,
    config: &LocalSandboxConfig,
) -> anyhow::Result<LocalSandboxInstallation> {
    let provider = LocalSandboxProvider::new(config)?;
    let service = SandboxService::new(provider.clone());
    let effect = service.provide(context)?;
    let cleanup_provider = provider.clone();
    let cleanup = context.own(EffectHandle::synchronous(
        "sandbox-local.windows-acl-grants",
        move || {
            cleanup_provider.revoke_acl_grants();
            Ok(())
        },
    ))?;
    Ok(LocalSandboxInstallation {
        provider,
        effect,
        cleanup,
    })
}

fn validate_plugin_config(value: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let config: LocalSandboxConfig = serde_json::from_value(value.clone())?;
    config.resolve()?;
    Ok(value.clone())
}

/// Builds the Loader-compatible provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: LocalSandboxConfig = serde_json::from_value(config)?;
            install(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(validate_plugin_config)
}
