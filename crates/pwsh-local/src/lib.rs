//! Local `PowerShell` executor over the managed subprocess capability.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, PluginFiber};
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SettingsSectionSource, install_settings_section};
use seekdeep_shell::{
    CollectedOutput, ProcessSignal, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellSandboxInfo,
    ShellService, shell_settings_namespace,
};
use seekdeep_subprocess::{
    SUBPROCESS, SubprocessCollect, SubprocessCollectedOutputs, SubprocessEnvironment,
    SubprocessHandleRef, SubprocessOutputMode, SubprocessOutputReaderHandle, SubprocessSpawnSpec,
    SubprocessSpill, SubprocessStdinMode, SubprocessStdio,
};
use seekdeep_util::timeout::{MAX_TIMER_DELAY_MS, clamp_timeout, deadline, timeout_of};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

/// Cordis plugin name.
pub const NAME: &str = "pwsh-local";
/// Required capability seats.
pub const INJECT: &[&str] = &["subprocess"];

/// Model-friendly child environment layered before caller and trusted values.
pub const ENV_OVERRIDES: [(&str, &str); 3] =
    [("NO_COLOR", "1"), ("PAGER", "cat"), ("GIT_PAGER", "cat")];

/// UTF-8 output pinning prepended to every `PowerShell` command.
pub const ENCODING_PREAMBLE: &str = "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); ";

/// Default `SIGTERM` to `SIGKILL` grace period.
pub const DEFAULT_GRACE_MS: f64 = 3_000.0;
/// Default complete-stream spill cap for each output stream.
pub const DEFAULT_MAX_SPILL_BYTES: f64 = 64.0 * 1024.0 * 1024.0;

/// Local `PowerShell` provider configuration after defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Default working directory; ambient process cwd when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Default foreground timeout in milliseconds.
    pub timeout_ms: f64,
    /// Maximum caller-selected foreground timeout.
    pub max_timeout_ms: f64,
    /// Per-stream in-memory output tail size.
    pub max_output_bytes: f64,
    /// Per-stream complete spill-file cap.
    pub max_spill_bytes: f64,
    /// Tree-termination escalation grace period.
    pub grace_ms: f64,
    /// Explicit `PowerShell` executable; resolved from the platform when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pwsh_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cwd: None,
            timeout_ms: 120_000.0,
            max_timeout_ms: 600_000.0,
            max_output_bytes: 64_000.0,
            max_spill_bytes: DEFAULT_MAX_SPILL_BYTES,
            grace_ms: DEFAULT_GRACE_MS,
            pwsh_path: None,
        }
    }
}

/// Returns the source-compatible configuration/settings schema.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("cwd", Schema::string()),
        (
            "timeoutMs",
            Schema::number().with_default(Config::default().timeout_ms),
        ),
        (
            "maxTimeoutMs",
            Schema::number().with_default(Config::default().max_timeout_ms),
        ),
        (
            "maxOutputBytes",
            Schema::number().with_default(Config::default().max_output_bytes),
        ),
        (
            "maxSpillBytes",
            Schema::number().with_default(DEFAULT_MAX_SPILL_BYTES),
        ),
        ("graceMs", Schema::number().with_default(DEFAULT_GRACE_MS)),
        ("pwshPath", Schema::string()),
    ])
}

/// Platform branch used by the pure `PowerShell` executable resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwshPlatform {
    /// Windows well-known locations and semicolon-separated `PATH` entries.
    Windows,
    /// Any non-Windows platform, where `pwsh` is resolved through `PATH`.
    Other,
}

/// Returns the source-compatible Windows `PowerShell` executable candidates.
#[must_use]
pub fn candidate_pwsh_paths(environment: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let program_files = environment
        .get("ProgramFiles")
        .map_or("C:\\Program Files", String::as_str);
    let system_root = environment
        .get("SystemRoot")
        .map_or("C:\\Windows", String::as_str);
    let mut candidates = vec![
        Path::new(program_files)
            .join("PowerShell")
            .join("7")
            .join("pwsh.exe"),
    ];
    if let Some(path) = environment.get("PATH") {
        candidates.extend(path.split(';').filter_map(|entry| {
            let entry = entry.trim().trim_matches('"');
            (!entry.is_empty()).then(|| Path::new(entry).join("pwsh.exe"))
        }));
    }
    candidates.push(
        Path::new(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
    );
    candidates
}

fn candidate_exists(candidate: &Path) -> bool {
    fs::symlink_metadata(candidate).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_file() || file_type.is_symlink()
    })
}

/// Resolves an explicit executable or the first usable platform candidate.
#[must_use]
pub fn resolve_pwsh_path(
    configured: Option<&str>,
    environment: &BTreeMap<String, String>,
    platform: PwshPlatform,
) -> String {
    if let Some(configured) = configured.filter(|path| !path.is_empty()) {
        return configured.to_owned();
    }
    if platform == PwshPlatform::Windows {
        for candidate in candidate_pwsh_paths(environment) {
            if candidate_exists(&candidate) {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "pwsh".to_owned()
}

fn resolve_current_pwsh_path(configured: Option<&str>) -> String {
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let platform = if cfg!(windows) {
        PwshPlatform::Windows
    } else {
        PwshPlatform::Other
    };
    resolve_pwsh_path(configured, &environment, platform)
}

/// Rejects a resolved configuration the executor cannot service.
///
/// # Errors
///
/// Names the first non-positive/non-finite field or oversized timer grace.
pub fn assert_serviceable_pwsh_config(config: &Config) -> anyhow::Result<()> {
    for (name, value) in [
        ("timeoutMs", config.timeout_ms),
        ("maxTimeoutMs", config.max_timeout_ms),
        ("maxOutputBytes", config.max_output_bytes),
        ("maxSpillBytes", config.max_spill_bytes),
        ("graceMs", config.grace_ms),
    ] {
        anyhow::ensure!(
            value.is_finite() && value > 0.0,
            "pwsh-local: {name} must be a positive finite number"
        );
    }
    anyhow::ensure!(
        config.grace_ms <= MAX_TIMER_DELAY_MS,
        "pwsh-local: graceMs must be no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(())
}

fn validate_config_value(value: &Value) -> anyhow::Result<Value> {
    let resolved = config_schema()
        .resolve(value)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let config: Config = serde_json::from_value(resolved.clone())?;
    assert_serviceable_pwsh_config(&config)?;
    Ok(resolved)
}

/// Local `PowerShell` service provider with dynamically layered settings.
pub struct LocalPwshExecutor {
    subprocess: Arc<seekdeep_subprocess::SubprocessService>,
    source: SettingsSectionSource,
    resolved_pwsh: Mutex<ResolvedPwshPath>,
    _settings_fiber: Arc<PluginFiber>,
}

#[derive(Debug)]
struct ResolvedPwshPath {
    declared: Option<String>,
    executable: String,
}

impl std::fmt::Debug for LocalPwshExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalPwshExecutor")
            .field("config", &self.config())
            .finish_non_exhaustive()
    }
}

impl LocalPwshExecutor {
    /// Reads the currently authoritative settings or composition config.
    ///
    /// # Panics
    ///
    /// Panics only if the settings service violates its registered schema and validator.
    #[must_use]
    pub fn config(&self) -> Config {
        serde_json::from_value(self.source.get())
            .expect("pwsh-local settings validation keeps the source well formed")
    }

    /// Returns the executable selected from the current settings layer.
    #[must_use]
    pub fn pwsh_path(&self) -> String {
        let declared = self.config().pwsh_path;
        let mut resolved = self.resolved_pwsh.lock();
        if resolved.declared != declared {
            resolved.executable = resolve_current_pwsh_path(declared.as_deref());
            resolved.declared = declared;
        }
        resolved.executable.clone()
    }

    /// Builds the exact `PowerShell` argv for one resolved shell request.
    #[must_use]
    pub fn argv(&self, spec: &ShellExecSpec) -> Vec<String> {
        vec![
            self.pwsh_path(),
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            format!("{ENCODING_PREAMBLE}{}", spec.command),
        ]
    }

    fn collected(
        handle: &SubprocessHandleRef,
    ) -> anyhow::Result<(SubprocessOutputReaderHandle, SubprocessOutputReaderHandle)> {
        let SubprocessCollectedOutputs { stdout, stderr } = handle.collected();
        Ok((
            stdout.ok_or_else(|| {
                anyhow::anyhow!(
                    "pwsh-local: subprocess implementation dropped a requested collect stream"
                )
            })?,
            stderr.ok_or_else(|| {
                anyhow::anyhow!(
                    "pwsh-local: subprocess implementation dropped a requested collect stream"
                )
            })?,
        ))
    }

    fn spawn_spec(
        &self,
        spec: &ShellExecSpec,
        argv: Vec<String>,
        stdout_max_bytes: f64,
        signal: Option<seekdeep_llm::AbortSignal>,
    ) -> SubprocessSpawnSpec {
        let config = self.config();
        let collect = |max_bytes| {
            SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes,
                spill: Some(SubprocessSpill {
                    max_bytes: config.max_spill_bytes,
                }),
            })
        };
        let mut env = SubprocessEnvironment::new();
        for (key, value) in ENV_OVERRIDES {
            env.insert(key.to_owned(), Some(value.to_owned()));
        }
        for (key, value) in spec.env.iter().flatten() {
            env.insert(key.clone(), Some(value.clone()));
        }
        for (key, value) in spec
            .seekdeep_env
            .as_ref()
            .into_iter()
            .flat_map(seekdeep_subprocess::SeekDeepEnvironment::iter)
        {
            env.insert(key.to_owned(), Some(value.to_owned()));
        }
        SubprocessSpawnSpec {
            argv,
            cwd: spec.workdir.clone(),
            stdio: SubprocessStdio {
                stdin: spec
                    .stdin
                    .as_ref()
                    .map_or(SubprocessStdinMode::Ignore, |value| {
                        SubprocessStdinMode::Data(value.clone())
                    }),
                stdout: collect(stdout_max_bytes),
                stderr: collect(config.max_output_bytes),
            },
            grace_ms: config.grace_ms,
            signal,
            env: Some(env),
        }
    }

    async fn run_argv(
        &self,
        spec: ShellExecSpec,
        argv: Vec<String>,
    ) -> anyhow::Result<ShellRunResult> {
        let deadline = deadline(spec.signal.as_ref(), spec.timeout_ms, "BASH_TIMEOUT")?;
        let handle = self.subprocess.spawn(self.spawn_spec(
            &spec,
            argv,
            spec.stdout_max_bytes,
            Some(deadline.signal.clone()),
        ))?;
        let outcome = handle.done().await?;
        let (stdout, stderr) = Self::collected(&handle)?;
        let timed_out = timeout_of(&deadline.signal, Some("BASH_TIMEOUT")).is_some();
        let aborted = deadline.signal.is_aborted() && !timed_out;
        Ok(ShellRunResult {
            exit_code: outcome.exit_code,
            signal: outcome.signal,
            timed_out,
            aborted,
            timeout_ms: spec.timeout_ms,
            stdout: final_output(&stdout),
            stderr: final_output(&stderr),
            sandbox: None,
        })
    }

    fn start_argv(
        &self,
        spec: &ShellExecSpec,
        argv: Vec<String>,
    ) -> anyhow::Result<ShellProcessHandle> {
        let config = self.config();
        let signal = spec.signal.clone();
        let running = self.subprocess.spawn(self.spawn_spec(
            spec,
            argv,
            config.max_output_bytes,
            signal.clone(),
        ))?;
        let (stdout, stderr) = Self::collected(&running)?;
        let process = Arc::new(LocalPwshProcess::new(running, stdout, stderr));
        LocalPwshProcess::observe(process.clone(), signal);
        Ok(process)
    }
}

#[async_trait]
impl ShellExecutor for LocalPwshExecutor {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        let config = self.config();
        let timeout_ms = clamp_timeout(
            request.timeout_ms,
            config.timeout_ms,
            config.max_timeout_ms,
            "pwsh-local: request.timeoutMs",
        )?;
        let stdout_max_bytes = request.stdout_max_bytes.unwrap_or(config.max_output_bytes);
        anyhow::ensure!(
            stdout_max_bytes.is_finite() && stdout_max_bytes > 0.0,
            "pwsh-local: request.stdoutMaxBytes must be a positive finite number"
        );
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request.workdir.unwrap_or_else(|| {
                config.cwd.map_or_else(
                    || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    PathBuf::from,
                )
            }),
            timeout_ms,
            stdout_max_bytes,
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        let argv = self.argv(&spec);
        self.run_argv(spec, argv).await
    }

    fn start(&self, spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        let argv = self.argv(&spec);
        self.start_argv(&spec, argv)
    }
}

fn final_output(reader: &SubprocessOutputReaderHandle) -> CollectedOutput {
    let read = reader.read_from(0);
    CollectedOutput {
        text: read.text,
        truncated: read.lossy,
        spill_path: read.spill_path,
    }
}

#[derive(Clone, Debug)]
struct ProcessState {
    status: ShellProcessStatus,
    exit_code: Option<i32>,
    signal: Option<ProcessSignal>,
    spawn_failure_note: Option<String>,
    settled: bool,
}

#[derive(Debug, Default)]
struct ReadOffsets {
    stdout: u64,
    stderr: u64,
}

#[derive(Debug)]
struct LocalPwshProcess {
    running: SubprocessHandleRef,
    stdout: SubprocessOutputReaderHandle,
    stderr: SubprocessOutputReaderHandle,
    state: Mutex<ProcessState>,
    offsets: Mutex<ReadOffsets>,
    settled: Notify,
}

impl LocalPwshProcess {
    fn new(
        running: SubprocessHandleRef,
        stdout: SubprocessOutputReaderHandle,
        stderr: SubprocessOutputReaderHandle,
    ) -> Self {
        Self {
            running,
            stdout,
            stderr,
            state: Mutex::new(ProcessState {
                status: ShellProcessStatus::Running,
                exit_code: None,
                signal: None,
                spawn_failure_note: None,
                settled: false,
            }),
            offsets: Mutex::new(ReadOffsets::default()),
            settled: Notify::new(),
        }
    }

    fn observe(process: Arc<Self>, execution_signal: Option<seekdeep_llm::AbortSignal>) {
        tokio::spawn(async move {
            match process.running.done().await {
                Ok(outcome) => {
                    let mut state = process.state.lock();
                    if state.status == ShellProcessStatus::Running {
                        state.status = if execution_signal
                            .as_ref()
                            .is_some_and(seekdeep_llm::AbortSignal::is_aborted)
                            || outcome.signal.is_some()
                        {
                            ShellProcessStatus::Killed
                        } else {
                            ShellProcessStatus::Completed
                        };
                    }
                    state.exit_code = outcome.exit_code;
                    state.signal = outcome.signal;
                    state.settled = true;
                }
                Err(error) => {
                    let mut state = process.state.lock();
                    state.status = ShellProcessStatus::Killed;
                    state.spawn_failure_note = Some(format!("spawn failed: {error}"));
                    state.settled = true;
                }
            }
            process.settled.notify_waiters();
        });
    }

    async fn wait(&self) {
        loop {
            let notified = self.settled.notified();
            if self.state.lock().settled {
                return;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl ShellProcess for LocalPwshProcess {
    fn status(&self) -> ShellProcessStatus {
        self.state.lock().status
    }

    fn exit_code(&self) -> Option<i32> {
        self.state.lock().exit_code
    }

    fn signal(&self) -> Option<ProcessSignal> {
        self.state.lock().signal.clone()
    }

    fn sandbox(&self) -> Option<ShellSandboxInfo> {
        None
    }

    async fn done(&self) {
        self.wait().await;
    }

    fn read_output(&self) -> ShellProcessRead {
        let mut offsets = self.offsets.lock();
        let stdout = self.stdout.read_from(offsets.stdout);
        let stderr = self.stderr.read_from(offsets.stderr);
        offsets.stdout = stdout.next_offset;
        offsets.stderr = stderr.next_offset;
        let spawn_failure = self.state.lock().spawn_failure_note.take();
        let error = if stderr.text.is_empty() {
            spawn_failure.unwrap_or_default()
        } else {
            stderr.text
        };
        let separator = if !stdout.text.is_empty() && !stdout.text.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let delta = if error.is_empty() {
            stdout.text
        } else {
            format!("{}{separator}[stderr]\n{error}", stdout.text)
        };
        ShellProcessRead {
            delta,
            lossy: stdout.lossy || stderr.lossy,
            stdout_spill_path: stdout.spill_path,
            stderr_spill_path: stderr.spill_path,
        }
    }

    fn kill(&self) -> bool {
        let mut state = self.state.lock();
        if state.status != ShellProcessStatus::Running {
            return false;
        }
        state.status = ShellProcessStatus::Killed;
        drop(state);
        self.running.terminate();
        true
    }
}

/// Installs settings layering and publishes the local `PowerShell` provider.
///
/// # Errors
///
/// Returns invalid configuration, missing subprocess, settings, or service-seat failures.
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<LocalPwshExecutor>> {
    assert_serviceable_pwsh_config(&config)?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("pwsh-local requires subprocess"))?;
    let validator = Arc::new(|value: &Value| {
        let config: Config = serde_json::from_value(value.clone())?;
        assert_serviceable_pwsh_config(&config)
    });
    let installed = install_settings_section(
        context,
        &shell_settings_namespace(),
        config_schema(),
        serde_json::to_value(config)?,
        Some(validator),
        Arc::new(|| Ok(())),
    )?;
    installed.fiber.await_settled().await?;
    let declared = serde_json::from_value::<Config>(installed.source.get())?.pwsh_path;
    let executable = resolve_current_pwsh_path(declared.as_deref());
    let executor = Arc::new(LocalPwshExecutor {
        subprocess,
        source: installed.source,
        resolved_pwsh: Mutex::new(ResolvedPwshPath {
            declared,
            executable,
        }),
        _settings_fiber: installed.fiber,
    });
    let erased: Arc<dyn ShellExecutor> = executor.clone();
    ShellService::new(erased).provide(context)?;
    Ok(executor)
}

/// Builds the Loader-compatible provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let resolved = validate_config_value(&config)?;
            let config: Config = serde_json::from_value(resolved)?;
            apply(&context, config).await?;
            Ok(())
        })
    })
    .with_config_validator(validate_config_value)
}
