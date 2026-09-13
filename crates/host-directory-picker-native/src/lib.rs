//! Native host-display directory picker with platform and process seams.

pub mod win32_dialog;

use std::{fmt, sync::Arc};

use futures::future::BoxFuture;
use seekdeep_cordis::Plugin;
use seekdeep_host_directory_picker::{DirectoryPickerCapability, DirectoryPickerService};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use seekdeep_util::native_command::{NativeCommandCode, NativeCommandOutput, run_native_command};

use crate::win32_dialog::pick_win32_directory;

/// Stable plugin name.
pub const NAME: &str = "host-directory-picker-native";
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-directory-picker-native-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-host-directory-picker-native";

/// Host platform spelling used by the source adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostPlatform {
    /// macOS (`process.platform === 'darwin'`).
    Darwin,
    /// Windows (`win32`).
    Win32,
    /// Linux.
    Linux,
    /// Preserved unsupported platform literal.
    Other(String),
}

impl HostPlatform {
    /// Detects the current host.
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Darwin,
            "windows" => Self::Win32,
            "linux" => Self::Linux,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Source-shaped command failure used by injected runners.
#[derive(Debug)]
pub struct PickerCommandError {
    /// Numeric exit status or named OS failure code.
    pub code: NativeCommandCode,
    /// Captured standard error.
    pub stderr: String,
    source: anyhow::Error,
}

impl PickerCommandError {
    /// Creates an injected command failure.
    #[must_use]
    pub fn new(code: NativeCommandCode, stderr: impl Into<String>, source: anyhow::Error) -> Self {
        Self {
            code,
            stderr: stderr.into(),
            source,
        }
    }
}

impl fmt::Display for PickerCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for PickerCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

/// No-shell command adapter used by macOS and Linux.
pub type DirectoryPickerRunner = Arc<
    dyn Fn(
            String,
            Vec<String>,
            AbortSignal,
        ) -> BoxFuture<'static, anyhow::Result<NativeCommandOutput>>
        + Send
        + Sync,
>;

/// Win32 dialog adapter.
pub type Win32Picker =
    Arc<dyn Fn(AbortSignal) -> BoxFuture<'static, anyhow::Result<Option<String>>> + Send + Sync>;

/// Injectable platform facts for deterministic adapter tests.
#[derive(Clone)]
pub struct DirectoryPickerInternals {
    /// Selected platform.
    pub platform: HostPlatform,
    /// Native command runner.
    pub run: DirectoryPickerRunner,
    /// Modern Win32 dialog driver.
    pub pick_win32_dialog: Win32Picker,
}

impl fmt::Debug for DirectoryPickerInternals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectoryPickerInternals")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl Default for DirectoryPickerInternals {
    fn default() -> Self {
        Self {
            platform: HostPlatform::current(),
            run: Arc::new(|command, args, signal| {
                Box::pin(async move {
                    run_native_command(command, &args, &signal)
                        .await
                        .map_err(|error| {
                            anyhow::Error::new(PickerCommandError::new(
                                error.code.clone(),
                                error.stderr.clone(),
                                anyhow::Error::new(error),
                            ))
                        })
                })
            }),
            pick_win32_dialog: Arc::new(|signal| Box::pin(pick_win32_directory(signal, None))),
        }
    }
}

/// Opens the selected platform's native single-directory chooser.
///
/// # Errors
///
/// Preserves command/dialog failures and rejects unsupported platforms.
pub async fn pick_native_directory(
    signal: AbortSignal,
    internals: &DirectoryPickerInternals,
) -> anyhow::Result<Option<String>> {
    match &internals.platform {
        HostPlatform::Darwin => {
            let result = (internals.run)(
                "osascript".to_owned(),
                vec![
                    "-e".to_owned(),
                    "set selectedFolder to choose folder with prompt \"Select Workspace Directory\""
                        .to_owned(),
                    "-e".to_owned(),
                    "POSIX path of selectedFolder".to_owned(),
                ],
                signal.clone(),
            )
            .await;
            match result {
                Ok(output) => Ok(output_path(&output.stdout)),
                Err(error)
                    if !signal.is_aborted()
                        && command_code(&error) == Some(&NativeCommandCode::Exit(1))
                        && macos_cancelled(command_stderr(&error)) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }
        HostPlatform::Win32 => (internals.pick_win32_dialog)(signal).await,
        HostPlatform::Linux => {
            let zenity = (internals.run)(
                "zenity".to_owned(),
                vec![
                    "--file-selection".to_owned(),
                    "--directory".to_owned(),
                    "--title=Select Workspace Directory".to_owned(),
                ],
                signal.clone(),
            )
            .await;
            match zenity {
                Ok(output) => return Ok(output_path(&output.stdout)),
                Err(error) => {
                    if signal.is_aborted() {
                        return Err(error);
                    }
                    if command_code(&error) == Some(&NativeCommandCode::Exit(1)) {
                        return Ok(None);
                    }
                    if command_code(&error) != Some(&NativeCommandCode::Named("ENOENT")) {
                        return Err(error);
                    }
                }
            }
            let kdialog = (internals.run)(
                "kdialog".to_owned(),
                vec![
                    "--getexistingdirectory".to_owned(),
                    ".".to_owned(),
                    "--title".to_owned(),
                    "Select Workspace Directory".to_owned(),
                ],
                signal.clone(),
            )
            .await;
            match kdialog {
                Ok(output) => Ok(output_path(&output.stdout)),
                Err(error) => {
                    if signal.is_aborted() {
                        Err(error)
                    } else if command_code(&error) == Some(&NativeCommandCode::Exit(1)) {
                        Ok(None)
                    } else if command_code(&error) == Some(&NativeCommandCode::Named("ENOENT")) {
                        anyhow::bail!(
                            "no supported native directory picker found (install zenity or kdialog)"
                        )
                    } else {
                        Err(error)
                    }
                }
            }
        }
        HostPlatform::Other(platform) => {
            anyhow::bail!("native directory picker is unsupported on {platform}")
        }
    }
}

/// Builds the native directory-picker Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    plugin_with_internals(DirectoryPickerInternals::default())
}

/// Builds the native plugin around deterministic platform adapters.
#[must_use]
pub fn plugin_with_internals(internals: DirectoryPickerInternals) -> Plugin {
    Plugin::new(NAME, std::iter::empty::<String>(), move |context, _| {
        let internals = internals.clone();
        Box::pin(async move {
            let service = DirectoryPickerService::new(DirectoryPickerCapability::Native {
                pick: Arc::new(move |signal| {
                    let internals = internals.clone();
                    Box::pin(async move { pick_native_directory(signal, &internals).await })
                }),
            });
            service.provide(&context)?;
            Ok(())
        })
    })
}

/// Registers the stateless native-backend invariant.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn output_path(stdout: &str) -> Option<String> {
    let path = stdout.trim_end_matches(['\r', '\n']);
    (!path.is_empty()).then(|| path.to_owned())
}

fn command_code(error: &anyhow::Error) -> Option<&NativeCommandCode> {
    error
        .downcast_ref::<PickerCommandError>()
        .map(|error| &error.code)
}

fn command_stderr(error: &anyhow::Error) -> &str {
    error
        .downcast_ref::<PickerCommandError>()
        .map_or("", |error| error.stderr.as_str())
}

fn macos_cancelled(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("user canceled") || stderr.contains("-128")
}
