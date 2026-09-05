//! Sandbox-consuming `PowerShell` executor over the local process mechanics.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_pwsh_local::{
    LocalPwshExecutor, PwshProcessObserver, PwshProcessSettlement, PwshSpawnFailure,
    build as build_local, resolve_config_value,
};
use seekdeep_sandbox::{
    ConfinedArgv, ConfinedSandboxMode, RunnerFailureRule, SANDBOX, SandboxExecutionPolicy,
    SandboxMode, SandboxPolicy, SandboxService, SandboxUnavailableError,
};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest, SandboxPolicyService};
use seekdeep_shell::{
    ProcessSignal, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellSandboxInfo,
    ShellService,
};

/// Cordis plugin name.
pub const NAME: &str = "pwsh-sandbox";
/// Required capability seats.
pub const INJECT: &[&str] = &["subprocess", "sandbox", "sandboxPolicy"];

/// The sandboxing executor accepts the local provider's configuration verbatim.
pub type Config = seekdeep_pwsh_local::Config;

/// Fatal runner evidence retained for the infrastructure error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerFailureMatch {
    /// Original stderr line that matched a fatal signature.
    pub detail: String,
}

/// Matches a nonzero exit against case-insensitive backend denial signatures.
#[must_use]
pub fn matches_signature(exit_code: Option<i32>, stderr: &str, signatures: &[String]) -> bool {
    if exit_code.is_none() || exit_code == Some(0) {
        return false;
    }
    let stderr = stderr.to_lowercase();
    signatures
        .iter()
        .any(|signature| stderr.contains(&signature.to_lowercase()))
}

/// Classifies a foreground denial using the selected backend dialect.
#[must_use]
pub fn classify_denial(result: &ShellRunResult, signatures: &[String]) -> bool {
    matches_signature(result.exit_code, &result.stderr.text, signatures)
}

/// Returns the first fatal runner line admitted by the structured rules.
#[must_use]
pub fn classify_runner_failure(
    exit_code: Option<i32>,
    stderr: &str,
    rules: &[RunnerFailureRule],
) -> Option<RunnerFailureMatch> {
    let exit_code = exit_code.filter(|code| *code != 0)?;
    let lines = stderr.lines().collect::<Vec<_>>();
    for rule in rules {
        if rule
            .allowed_exit_codes
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&exit_code))
        {
            continue;
        }
        let informational = rule
            .informational_lines
            .as_ref()
            .into_iter()
            .flatten()
            .map(|line| line.to_lowercase())
            .collect::<Vec<_>>();
        let fatal = rule
            .fatal_signatures
            .iter()
            .filter(|signature| !signature.trim().is_empty())
            .map(|signature| signature.to_lowercase())
            .collect::<Vec<_>>();
        for line in &lines {
            let lowered = line.to_lowercase();
            if informational.contains(&lowered) {
                continue;
            }
            if fatal.iter().any(|signature| lowered.contains(signature)) {
                return Some(RunnerFailureMatch {
                    detail: (*line).to_owned(),
                });
            }
        }
    }
    None
}

fn is_usable_workdir(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Attributes executable-resolution or permission failures to a known runner.
#[must_use]
pub fn is_runner_spawn_failure(
    error: &anyhow::Error,
    runner_program: Option<&str>,
    workdir: &Path,
) -> bool {
    runner_program.is_some()
        && is_usable_workdir(workdir)
        && error.downcast_ref::<PwshSpawnFailure>().is_some()
}

/// Sandboxing `PowerShell` provider that occupies `ctx.shell`.
pub struct SandboxPwshExecutor {
    local: Arc<LocalPwshExecutor>,
    sandbox: Arc<SandboxService>,
    policy: Arc<SandboxPolicyService>,
}

impl std::fmt::Debug for SandboxPwshExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxPwshExecutor")
            .field("local", &self.local)
            .field("default_mode", &self.policy.default_mode)
            .finish_non_exhaustive()
    }
}

impl SandboxPwshExecutor {
    fn policy_for(
        &self,
        request: Option<SandboxExecutionPolicy>,
    ) -> anyhow::Result<SandboxExecutionPolicy> {
        request.map_or_else(|| self.policy.resolve(SandboxPolicyRequest::default()), Ok)
    }

    fn confine(
        &self,
        spec: &ShellExecSpec,
        policy: SandboxExecutionPolicy,
    ) -> anyhow::Result<(ConfinedSandboxMode, ConfinedArgv)> {
        let policy = SandboxPolicy::try_from(policy)?;
        let mode = policy.mode;
        let confined = self.sandbox.confine(&self.local.argv(spec), &policy)?;
        Ok((mode, confined))
    }
}

#[derive(Debug)]
struct SandboxPwshProcess {
    inner: ShellProcessHandle,
    sandbox: Arc<Mutex<Option<ShellSandboxInfo>>>,
}

#[async_trait]
impl ShellProcess for SandboxPwshProcess {
    fn status(&self) -> ShellProcessStatus {
        self.inner.status()
    }

    fn exit_code(&self) -> Option<i32> {
        self.inner.exit_code()
    }

    fn signal(&self) -> Option<ProcessSignal> {
        self.inner.signal()
    }

    fn sandbox(&self) -> Option<ShellSandboxInfo> {
        self.sandbox.lock().clone()
    }

    async fn done(&self) {
        self.inner.done().await;
    }

    fn read_output(&self) -> ShellProcessRead {
        self.inner.read_output()
    }

    fn kill(&self) -> bool {
        self.inner.kill()
    }
}

#[async_trait]
impl ShellExecutor for SandboxPwshExecutor {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        Some(self.policy.default_mode)
    }

    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        let supplied_policy = request.sandbox_policy.clone();
        let mut spec = self.local.resolve(request)?;
        spec.sandbox_policy = Some(self.policy_for(supplied_policy)?);
        Ok(spec)
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        let policy = spec.sandbox_policy.clone().ok_or_else(|| {
            anyhow::anyhow!("pwsh-sandbox: resolved spec is missing sandboxPolicy")
        })?;
        if policy.mode == SandboxMode::DangerFullAccess {
            let mut result = self.local.run(spec).await?;
            result.sandbox = Some(ShellSandboxInfo {
                mode: SandboxMode::DangerFullAccess,
                denied: false,
                enforcement: None,
                runner_failed: None,
            });
            return Ok(result);
        }
        let (mode, confined) = self.confine(&spec, policy)?;
        let result = self
            .local
            .run_argv(spec.clone(), confined.argv.clone())
            .await;
        let mut result = match result {
            Ok(result) => result,
            Err(error) => {
                if spec
                    .signal
                    .as_ref()
                    .is_some_and(seekdeep_llm::AbortSignal::is_aborted)
                {
                    return Err(error);
                }
                if is_runner_spawn_failure(
                    &error,
                    confined.argv.first().map(String::as_str),
                    &spec.workdir,
                ) {
                    return Err(anyhow::Error::new(SandboxUnavailableError::new(
                        mode,
                        Some(&error.to_string()),
                    )));
                }
                return Err(error);
            }
        };
        if let Some(failure) = classify_runner_failure(
            result.exit_code,
            &result.stderr.text,
            &confined.runner_failure_rules,
        ) {
            return Err(anyhow::Error::new(SandboxUnavailableError::new(
                mode,
                Some(&failure.detail),
            )));
        }
        result.sandbox = Some(ShellSandboxInfo {
            mode: mode.into(),
            denied: classify_denial(&result, &confined.denial_signatures),
            enforcement: Some(confined.enforcement),
            runner_failed: None,
        });
        Ok(result)
    }

    fn start(&self, spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        let policy = spec.sandbox_policy.clone().ok_or_else(|| {
            anyhow::anyhow!("pwsh-sandbox: resolved spec is missing sandboxPolicy")
        })?;
        if policy.mode == SandboxMode::DangerFullAccess {
            return self.local.start(spec);
        }
        let (mode, confined) = self.confine(&spec, policy)?;
        let sandbox = Arc::new(Mutex::new(None));
        let settlement_sandbox = sandbox.clone();
        let workdir = spec.workdir.clone();
        let runner_program = confined.argv.first().cloned();
        let enforcement = confined.enforcement;
        let denial_signatures = confined.denial_signatures.clone();
        let runner_failure_rules = confined.runner_failure_rules.clone();
        let observer: PwshProcessObserver = Arc::new(move |settlement: &PwshProcessSettlement| {
            let runner_failed = (settlement.spawn_error.is_some()
                && runner_program.is_some()
                && is_usable_workdir(&workdir))
                || (settlement.spawn_error.is_none()
                    && classify_runner_failure(
                        settlement.exit_code,
                        &settlement.stderr,
                        &runner_failure_rules,
                    )
                    .is_some());
            *settlement_sandbox.lock() = Some(ShellSandboxInfo {
                mode: mode.into(),
                denied: !runner_failed
                    && matches_signature(
                        settlement.exit_code,
                        &settlement.stderr,
                        &denial_signatures,
                    ),
                enforcement: Some(enforcement),
                runner_failed: runner_failed.then_some(true),
            });
        });
        let inner = self
            .local
            .start_argv_observed(&spec, confined.argv, Some(observer))?;
        Ok(Arc::new(SandboxPwshProcess { inner, sandbox }))
    }
}

/// Installs the confining executor and publishes it as `ctx.shell`.
///
/// # Errors
///
/// Returns missing dependencies, invalid local configuration, or service registration failures.
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<SandboxPwshExecutor>> {
    let sandbox = context
        .get(SANDBOX)
        .ok_or_else(|| anyhow::anyhow!("pwsh-sandbox requires sandbox"))?;
    let policy = context
        .get(SANDBOX_POLICY)
        .ok_or_else(|| anyhow::anyhow!("pwsh-sandbox requires sandboxPolicy"))?;
    let local = build_local(context, config).await?;
    let executor = Arc::new(SandboxPwshExecutor {
        local,
        sandbox,
        policy,
    });
    let erased: Arc<dyn ShellExecutor> = executor.clone();
    ShellService::new(erased).provide(context)?;
    Ok(executor)
}

/// Builds the Loader-compatible sandboxing provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let resolved = resolve_config_value(&config)?;
            let config: Config = serde_json::from_value(resolved)?;
            apply(&context, config).await?;
            Ok(())
        })
    })
    .with_config_validator(resolve_config_value)
}
