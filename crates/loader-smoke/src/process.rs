//! Rust-native example launch selection and isolated subprocess ownership.

use std::{
    collections::BTreeMap, ffi::OsString, path::PathBuf, process::Stdio, sync::Arc, time::Duration,
};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

/// Environment variable selecting the development or publish-shaped artifact.
pub const EXAMPLE_MODE_ENV: &str = "SEEKDEEP_EXAMPLE_MODE";
/// Isolated Agent skill/configuration home used by example smokes.
pub const SEEKDEEP_AGENTS_HOME_ENV: &str = "SEEKDEEP_AGENTS_HOME";
const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
/// Test deadline leaving room around the subprocess-owned diagnostic timeout.
pub const LOADER_SMOKE_TEST_TIMEOUT_MS: u64 = 45_000;

/// Which compiled Rust artifact an example smoke boots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExampleMode {
    /// Fast development artifact selected by the `src` wire value.
    #[default]
    #[serde(rename = "src")]
    Source,
    /// Publish-shaped artifact selected by the `lib` wire value.
    #[serde(rename = "lib")]
    Library,
}

impl ExampleMode {
    /// Parses the compatibility wire value, defaulting absent or empty to `src`.
    ///
    /// # Errors
    ///
    /// Rejects every value except empty, `src`, and `lib`.
    pub fn resolve(raw: Option<&str>) -> anyhow::Result<Self> {
        match raw {
            None | Some("" | "src") => Ok(Self::Source),
            Some("lib") => Ok(Self::Library),
            Some(raw) => anyhow::bail!("{EXAMPLE_MODE_ENV} must be 'src' or 'lib', got {raw:?}."),
        }
    }

    /// Reads the mode from the process environment.
    ///
    /// # Errors
    ///
    /// Returns non-Unicode or invalid-value diagnostics.
    pub fn from_environment() -> anyhow::Result<Self> {
        match std::env::var(EXAMPLE_MODE_ENV) {
            Ok(raw) => Self::resolve(Some(&raw)),
            Err(std::env::VarError::NotPresent) => Self::resolve(None),
            Err(error) => {
                anyhow::bail!("{EXAMPLE_MODE_ENV} must contain Unicode: {error}")
            }
        }
    }
}

/// Resolves the current process's example mode.
///
/// # Errors
///
/// Returns non-Unicode or invalid-value diagnostics.
pub fn resolve_example_mode() -> anyhow::Result<ExampleMode> {
    ExampleMode::from_environment()
}

/// Inputs to Rust-native example launch selection.
#[derive(Clone, Debug)]
pub struct ExampleLaunchOptions {
    /// Development artifact built from the example's Rust binary target.
    pub source_bin: PathBuf,
    /// Explicit publish-shaped artifact; required in `lib` mode.
    pub library_bin: Option<PathBuf>,
    /// Arguments passed after the selected binary.
    pub config_args: Vec<OsString>,
    /// Explicit mode, or the environment-selected mode when absent.
    pub mode: Option<ExampleMode>,
    /// Caller environment layered after isolated homes.
    pub environment: BTreeMap<OsString, OsString>,
}

/// Resolved Rust executable, arguments, and environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExampleLaunch {
    /// Exact compiled Rust executable.
    pub command: PathBuf,
    /// Exact caller arguments.
    pub args: Vec<OsString>,
    /// Target environment overlays.
    pub environment: BTreeMap<OsString, OsString>,
}

impl ExampleLaunchOptions {
    /// Selects a compiled Rust example artifact without a Node or TypeScript runtime.
    ///
    /// # Errors
    ///
    /// Rejects `lib` mode without an explicit publish-shaped binary.
    pub fn resolve(self) -> anyhow::Result<ExampleLaunch> {
        let mode = match self.mode {
            Some(mode) => mode,
            None => ExampleMode::from_environment()?,
        };
        let command = match mode {
            ExampleMode::Source => self.source_bin,
            ExampleMode::Library => self.library_bin.ok_or_else(|| {
                anyhow::anyhow!(
                    "resolveExampleLaunch: 'lib' mode needs libraryBin for the compiled Rust artifact."
                )
            })?,
        };
        Ok(ExampleLaunch {
            command,
            args: self.config_args,
            environment: self.environment,
        })
    }
}

/// Resolves one Rust-native example launch.
///
/// # Errors
///
/// Returns invalid environment mode or missing compiled-artifact diagnostics.
pub fn resolve_example_launch(options: ExampleLaunchOptions) -> anyhow::Result<ExampleLaunch> {
    options.resolve()
}

/// Asynchronous setup or inspection hook over the isolated working directory.
pub type LoaderSmokeHook =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Inputs that vary between real Loader example smokes.
#[derive(Clone)]
pub struct LoaderSmokeOptions {
    /// Human-readable example name used in failure diagnostics.
    pub label: String,
    /// Prefix for the isolated temporary process working directory.
    pub temp_dir_prefix: String,
    /// Complete resolved launch.
    pub launch: ExampleLaunch,
    /// Process deadline.
    pub process_timeout: Duration,
    /// Optional world-state setup before process start.
    pub prepare: Option<LoaderSmokeHook>,
    /// Optional world-state assertion before cleanup.
    pub inspect: Option<LoaderSmokeHook>,
    /// Exact expected exit code.
    pub expected_exit_code: i32,
}

impl std::fmt::Debug for LoaderSmokeOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoaderSmokeOptions")
            .field("label", &self.label)
            .field("temp_dir_prefix", &self.temp_dir_prefix)
            .field("launch", &self.launch)
            .field("process_timeout", &self.process_timeout)
            .field("prepare", &self.prepare.as_ref().map(|_| "<hook>"))
            .field("inspect", &self.inspect.as_ref().map(|_| "<hook>"))
            .field("expected_exit_code", &self.expected_exit_code)
            .finish()
    }
}

impl LoaderSmokeOptions {
    /// Creates a 30-second, zero-exit smoke around one resolved launch.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        temp_dir_prefix: impl Into<String>,
        launch: ExampleLaunch,
    ) -> Self {
        Self {
            label: label.into(),
            temp_dir_prefix: temp_dir_prefix.into(),
            launch,
            process_timeout: DEFAULT_PROCESS_TIMEOUT,
            prepare: None,
            inspect: None,
            expected_exit_code: 0,
        }
    }
}

/// Captured output from a Loader smoke with the declared exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoaderSmokeResult {
    /// Complete standard output.
    pub stdout: String,
    /// Complete standard error.
    pub stderr: String,
}

/// Boots one real Loader tree from an isolated working directory.
///
/// Standard input is closed immediately. The helper kills and joins a timed-out
/// child, captures both output streams, invokes inspection only after the
/// declared exit, and removes the working directory on every return path.
///
/// # Errors
///
/// Returns setup, spawn, I/O, timeout, exit-code, inspection, or cleanup errors.
pub async fn run_loader_smoke(options: LoaderSmokeOptions) -> anyhow::Result<LoaderSmokeResult> {
    let temporary = tempfile::Builder::new()
        .prefix(&options.temp_dir_prefix)
        .tempdir()?;
    let cwd = temporary.path().to_path_buf();
    if let Some(prepare) = &options.prepare {
        prepare(cwd.clone()).await?;
    }

    let mut command = tokio::process::Command::new(&options.launch.command);
    command
        .args(&options.launch.args)
        .current_dir(&cwd)
        .env(
            seekdeep_util::home_paths::SEEKDEEP_HOME_ENV,
            cwd.join(".seekdeep"),
        )
        .env(SEEKDEEP_AGENTS_HOME_ENV, cwd.join(".agents"))
        .envs(&options.launch.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("{} stdout was not captured", options.label))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("{} stderr was not captured", options.label))?;
    let stdout_task = tokio::spawn(read_all(stdout));
    let stderr_task = tokio::spawn(read_all(stderr));
    let status =
        if let Ok(status) = tokio::time::timeout(options.process_timeout, child.wait()).await {
            status?
        } else {
            child.kill().await?;
            let _ = child.wait().await;
            let stdout = collect_output(stdout_task).await?;
            let stderr = collect_output(stderr_task).await?;
            anyhow::bail!(
                "{} did not exit within {}. stdout:\n{}stderr:\n{}",
                options.label,
                duration_label(options.process_timeout),
                stdout,
                stderr
            );
        };
    let stdout = collect_output(stdout_task).await?;
    let stderr = collect_output(stderr_task).await?;
    if status.code() != Some(options.expected_exit_code) {
        anyhow::bail!(
            "{} exited {} (expected {}). stdout:\n{}stderr:\n{}",
            options.label,
            status
                .code()
                .map_or_else(|| "null".to_owned(), |code| code.to_string()),
            options.expected_exit_code,
            stdout,
            stderr
        );
    }
    if let Some(inspect) = &options.inspect {
        inspect(cwd).await?;
    }
    temporary.close()?;
    Ok(LoaderSmokeResult { stdout, stderr })
}

async fn read_all(mut input: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn collect_output(
    task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> anyhow::Result<String> {
    Ok(String::from_utf8(task.await??)?)
}

fn duration_label(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        let seconds = duration.as_secs_f64();
        format!("{seconds}s")
    }
}
