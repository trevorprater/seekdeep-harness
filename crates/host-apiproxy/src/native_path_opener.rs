//! Cross-platform, shell-free native path and text-document openers.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, OnceLock},
};

use futures::future::BoxFuture;
use regex::Regex;
use seekdeep_llm::AbortSignal;
use seekdeep_util::native_command::{NativeCommandOutput, run_native_command};

/// Testable no-shell command boundary.
pub type PathOpenerRunner = Arc<
    dyn Fn(
            String,
            Vec<String>,
            AbortSignal,
        ) -> BoxFuture<'static, anyhow::Result<NativeCommandOutput>>
        + Send
        + Sync,
>;

/// Platform vocabulary used by the native adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativePlatform {
    /// macOS (`process.platform === "darwin"`).
    Darwin,
    /// Windows.
    Windows,
    /// Linux, including WSL.
    Linux,
    /// Unsupported platform retained for an actionable diagnostic.
    Other(String),
}

impl NativePlatform {
    fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::Darwin,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Injectable platform facts for deterministic adapter tests.
#[derive(Clone, Default)]
pub struct PathOpenerInternals {
    /// Platform override; absent samples the build host.
    pub platform: Option<NativePlatform>,
    /// Kernel release override used to identify WSL.
    pub os_release: Option<String>,
    /// Environment override; `Some(empty)` deliberately hides ambient variables.
    pub env: Option<HashMap<String, String>>,
    /// No-shell command runner override.
    pub run: Option<PathOpenerRunner>,
}

impl fmt::Debug for PathOpenerInternals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PathOpenerInternals")
            .field("platform", &self.platform)
            .field("os_release", &self.os_release)
            .field("env", &self.env)
            .field("has_runner", &self.run.is_some())
            .finish()
    }
}

#[derive(Clone, Copy)]
enum PathOpenIntent {
    Default,
    TextEditor,
}

/// Whether handing a path to the native opener can plausibly reach a desktop.
#[must_use]
pub fn can_open_native_path(internals: &PathOpenerInternals) -> bool {
    let platform = internals
        .platform
        .clone()
        .unwrap_or_else(NativePlatform::current);
    match platform {
        NativePlatform::Darwin | NativePlatform::Windows => true,
        NativePlatform::Linux => {
            let env = environment(internals);
            is_wsl(internals, &env)
                || present(env.get("DISPLAY"))
                || present(env.get("WAYLAND_DISPLAY"))
        }
        NativePlatform::Other(_) => false,
    }
}

/// Opens a path with its default application, preferring a named browser for
/// browser-renderable documents.
///
/// # Errors
///
/// Returns unsupported-platform, cancellation, translation, or native-command failures.
pub async fn open_native_path(
    path: &str,
    signal: &AbortSignal,
    internals: &PathOpenerInternals,
) -> anyhow::Result<()> {
    open_with_intent(path, signal, PathOpenIntent::Default, internals).await
}

/// Opens a text document for editing; macOS bypasses file-type associations.
///
/// # Errors
///
/// Returns unsupported-platform, cancellation, translation, or native-command failures.
pub async fn open_native_text_file(
    path: &str,
    signal: &AbortSignal,
    internals: &PathOpenerInternals,
) -> anyhow::Result<()> {
    open_with_intent(path, signal, PathOpenIntent::TextEditor, internals).await
}

async fn open_with_intent(
    path: &str,
    signal: &AbortSignal,
    intent: PathOpenIntent,
    internals: &PathOpenerInternals,
) -> anyhow::Result<()> {
    let platform = internals
        .platform
        .clone()
        .unwrap_or_else(NativePlatform::current);
    let env = environment(internals);
    let runner = internals.run.clone().unwrap_or_else(default_runner);
    let wsl = platform == NativePlatform::Linux && is_wsl(internals, &env);

    if !wsl
        && matches!(intent, PathOpenIntent::Default)
        && is_browser_document(path)
        && open_in_browser(path, signal, &platform, &runner, &env).await?
    {
        return Ok(());
    }

    match platform {
        NativePlatform::Darwin => {
            let args = if matches!(intent, PathOpenIntent::TextEditor) {
                vec!["-t".to_owned(), path.to_owned()]
            } else {
                vec![path.to_owned()]
            };
            runner("open".to_owned(), args, signal.clone()).await?;
        }
        NativePlatform::Windows => open_windows_path(path, signal, &runner).await?,
        NativePlatform::Linux if wsl => open_wsl_path(path, signal, &runner).await?,
        NativePlatform::Linux => {
            runner("xdg-open".to_owned(), vec![path.to_owned()], signal.clone()).await?;
        }
        NativePlatform::Other(platform) => {
            anyhow::bail!("native path opener is unsupported on {platform}")
        }
    }
    Ok(())
}

async fn open_in_browser(
    path: &str,
    signal: &AbortSignal,
    platform: &NativePlatform,
    runner: &PathOpenerRunner,
    env: &HashMap<String, String>,
) -> anyhow::Result<bool> {
    match platform {
        NativePlatform::Darwin => {
            let Ok(output) = runner(
                "defaults".to_owned(),
                vec![
                    "read".to_owned(),
                    "com.apple.LaunchServices/com.apple.launchservices.secure".to_owned(),
                ],
                signal.clone(),
            )
            .await
            else {
                return Ok(false);
            };
            let Some(bundle) = mac_bundle_for_https(&output.stdout) else {
                return Ok(false);
            };
            runner(
                "open".to_owned(),
                vec!["-b".to_owned(), bundle, path.to_owned()],
                signal.clone(),
            )
            .await?;
            Ok(true)
        }
        NativePlatform::Linux => {
            let Some(browser) = env.get("BROWSER").filter(|value| !value.is_empty()) else {
                return Ok(false);
            };
            runner(browser.clone(), vec![path.to_owned()], signal.clone()).await?;
            Ok(true)
        }
        NativePlatform::Windows | NativePlatform::Other(_) => Ok(false),
    }
}

async fn open_windows_path(
    path: &str,
    signal: &AbortSignal,
    runner: &PathOpenerRunner,
) -> anyhow::Result<()> {
    runner(
        "powershell.exe".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("Invoke-Item -LiteralPath {}", powershell_literal(path)),
        ],
        signal.clone(),
    )
    .await?;
    Ok(())
}

async fn open_wsl_path(
    path: &str,
    signal: &AbortSignal,
    runner: &PathOpenerRunner,
) -> anyhow::Result<()> {
    let translated = runner(
        "wslpath".to_owned(),
        vec!["-w".to_owned(), path.to_owned()],
        signal.clone(),
    )
    .await?;
    ensure_not_aborted(signal)?;
    let windows_path = translated.stdout.trim_end_matches(['\r', '\n']).to_owned();
    anyhow::ensure!(!windows_path.is_empty(), "wslpath returned no Windows path");
    open_windows_path(&windows_path, signal, runner).await
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if !signal.is_aborted() {
        return Ok(());
    }
    let message = signal.reason().map_or_else(
        || "This operation was aborted".to_owned(),
        |reason| {
            reason
                .as_str()
                .map_or_else(|| reason.to_string(), str::to_owned)
        },
    );
    anyhow::bail!(message)
}

fn powershell_literal(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

fn present(value: Option<&String>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn is_wsl(internals: &PathOpenerInternals, env: &HashMap<String, String>) -> bool {
    present(env.get("WSL_DISTRO_NAME"))
        || present(env.get("WSL_INTEROP"))
        || kernel_release(internals)
            .to_ascii_lowercase()
            .contains("microsoft")
}

fn kernel_release(internals: &PathOpenerInternals) -> String {
    internals.os_release.clone().unwrap_or_else(|| {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

fn environment(internals: &PathOpenerInternals) -> HashMap<String, String> {
    internals
        .env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect())
}

fn is_browser_document(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    let Some(dot) = basename.rfind('.') else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    matches!(
        basename[dot..].to_ascii_lowercase().as_str(),
        ".html" | ".htm" | ".xhtml" | ".svg"
    )
}

fn mac_bundle_for_https(plist: &str) -> Option<String> {
    static VERSIONS: OnceLock<Regex> = OnceLock::new();
    static HTTPS_BLOCK: OnceLock<Regex> = OnceLock::new();
    static ROLE: OnceLock<Regex> = OnceLock::new();
    let versions = VERSIONS.get_or_init(|| {
        Regex::new(r"LSHandlerPreferredVersions\s*=\s*\{[^}]*\};")
            .expect("static LaunchServices versions regex")
    });
    let block = HTTPS_BLOCK.get_or_init(|| {
        Regex::new(r#"\{[^{}]*LSHandlerURLScheme\s*=\s*"?https"?;[^{}]*\}"#)
            .expect("static LaunchServices block regex")
    });
    let role = ROLE.get_or_init(|| {
        Regex::new(r#"LSHandlerRoleAll\s*=\s*"?([\w.-]+)"?;"#)
            .expect("static LaunchServices role regex")
    });
    let stripped = versions.replace_all(plist, "");
    let block = block.find(&stripped)?.as_str();
    role.captures(block)?
        .get(1)
        .map(|value| value.as_str().to_owned())
}

fn default_runner() -> PathOpenerRunner {
    Arc::new(|command, args, signal| {
        Box::pin(async move { Ok(run_native_command(command, &args, &signal).await?) })
    })
}
