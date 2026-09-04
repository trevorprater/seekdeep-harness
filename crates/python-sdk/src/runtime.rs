//! Runtime-carrier resolution without acquisition, downloading, or implicit Node fallback.

use std::path::{Path, PathBuf};

use path_clean::PathClean as _;
use serde_json::Value;

use crate::{Error, ErrorKind, Result};

/// Metadata file required at the installed runtime package root.
pub const PACKAGE_METADATA_FILENAME: &str = "seekdeep-harness-runtime.json";
/// Explicit runtime-carrier selector.
pub const RUNTIME_MODE_ENV_VAR: &str = "SEEKDEEP_RUNTIME_MODE";

const ACQUISITION_HINT: &str = "Two ways to get the executable: run `cargo run --locked -p seekdeep-python-release --bin build-exe-for-python-sdk --` in a seekdeep-harness checkout, or install the matching `seekdeep-harness-runtime-bin` platform wheel retained by the `build-exe-for-python-sdk` CI workflow. For local development against a repo source build, explicitly select the dev-only node carrier with SEEKDEEP_RUNTIME_MODE=node (or resolve_bundled_launch_args('node')).";

/// Selected carrier; the Node path is always explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Native executable, including automatic resolution.
    Exe,
    /// Development-only Node launch binding.
    Node,
}

/// Resolves an explicit mode before the environment value.
///
/// # Errors
/// Rejects every value other than None, "exe", or "node", including an empty string.
pub fn selected_mode(explicit: &Value, environment: Option<&str>) -> Result<RuntimeMode> {
    selected_mode_with(
        explicit.clone(),
        || Ok(environment.map_or(Value::Null, |value| Value::String(value.to_owned()))),
        |value| Ok(value.is_null()),
        |value, expected| Ok(value.as_str() == Some(expected)),
        |value| Ok(crate::values::python_repr(value)),
    )
}

/// Resolves a mode through foreign-value primitives without serializing the caller's object.
///
/// # Errors
/// Propagates interpreter comparison/representation errors and rejects unsupported modes.
pub fn selected_mode_with<T>(
    explicit: T,
    environment: impl FnOnce() -> Result<T>,
    is_none: impl Fn(&T) -> Result<bool>,
    equals: impl Fn(&T, &str) -> Result<bool>,
    representation: impl Fn(&T) -> Result<String>,
) -> Result<RuntimeMode> {
    let selected = if is_none(&explicit)? {
        environment()?
    } else {
        explicit
    };
    if is_none(&selected)? || equals(&selected, "exe")? {
        return Ok(RuntimeMode::Exe);
    }
    if equals(&selected, "node")? {
        return Ok(RuntimeMode::Node);
    }
    Err(Error::new(
        ErrorKind::Value,
        format!(
            "unsupported SeekDeep Harness runtime mode {}: expected 'exe' or 'node' (explicit argument or ${RUNTIME_MODE_ENV_VAR})",
            representation(&selected)?
        ),
    ))
}

/// Returns the source platform/architecture tag from injected host facts.
///
/// # Errors
/// Unsupported hosts produce the source's acquisition-oriented `FileNotFoundError`.
pub fn platform_tag(platform: &str, machine: &str) -> Result<String> {
    let platform_tag = match platform {
        "linux" => Some("linux"),
        "darwin" => Some("macos"),
        _ => None,
    };
    let lower = machine.to_lowercase();
    let arch = match lower.as_str() {
        "x86_64" | "amd64" => Some("x64"),
        "arm64" | "aarch64" => Some("arm64"),
        _ => None,
    };
    match (platform_tag, arch) {
        (Some(platform), Some(arch)) => Ok(format!("{platform}-{arch}")),
        _ => Err(Error::new(
            ErrorKind::FileNotFound,
            format!(
                "no bundled seekdeep-jsonrpc-agent executable exists for this platform (sys.platform={}, machine={}); supported: linux/macos on x64/arm64. {ACQUISITION_HINT}",
                crate::values::python_repr(&Value::String(platform.to_owned())),
                crate::values::python_repr(&Value::String(machine.to_owned()))
            ),
        )),
    }
}

/// Locates package data beside a module, requiring the metadata marker.
///
/// # Errors
/// Returns an operating-system failure or `FileNotFoundError` for missing metadata.
pub fn bundled_package_dir(module: &Path, cwd: &Path) -> Result<PathBuf> {
    let module = resolve_path(module, cwd)?;
    let root = module.parent().unwrap_or(&module).to_owned();
    let metadata = root.join(PACKAGE_METADATA_FILENAME);
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::FileNotFound,
            format!(
                "seekdeep-harness-runtime-bin is missing {}",
                metadata.display()
            ),
        ));
    }
    Ok(root)
}

/// Resolves the checked-in default configuration without selecting an executable.
///
/// # Errors
/// Missing configuration is a `FileNotFoundError`.
pub fn bundled_default_config_path(root: &Path) -> Result<PathBuf> {
    let path = root.join("runtime/cordis.yml");
    if !path.is_file() {
        return Err(Error::new(
            ErrorKind::FileNotFound,
            format!(
                "seekdeep-harness-runtime-bin is missing the default runtime config at {}",
                path.display()
            ),
        ));
    }
    Ok(path)
}

/// Resolves the native executable and, on macOS, its mandatory helper sidecar.
///
/// # Errors
/// Missing artifacts are `FileNotFoundError`; this lookup does not require executable permissions.
pub fn bundled_runtime_path(root: &Path, tag: &str) -> Result<PathBuf> {
    let path = root
        .join("runtime")
        .join(format!("seekdeep-jsonrpc-agent-pkg-{tag}"));
    if !path.is_file() {
        return Err(Error::new(
            ErrorKind::FileNotFound,
            format!(
                "seekdeep-harness-runtime-bin is missing the runtime executable at {}. {ACQUISITION_HINT}",
                path.display()
            ),
        ));
    }
    if tag.starts_with("macos-") {
        let mut helper = path.as_os_str().to_owned();
        helper.push("-spawn-helper");
        let helper = PathBuf::from(helper);
        if !helper.is_file() {
            return Err(Error::new(
                ErrorKind::FileNotFound,
                format!(
                    "seekdeep-harness-runtime-bin is missing the node-pty spawn helper at {}. {ACQUISITION_HINT}",
                    helper.display()
                ),
            ));
        }
    }
    Ok(path)
}

/// Resolves the development entry before looking for a system Node executable.
///
/// # Errors
/// Missing generated entry or Node binary is a `FileNotFoundError`.
pub fn node_launch_args(
    root: &Path,
    find_node: impl FnOnce() -> Result<Option<String>>,
) -> Result<Vec<String>> {
    let node_root = root.join("runtime/node");
    let entry =
        node_root.join("node_modules/@seekdeep-ai/seekdeep-sdk-jsonrpc-demo/lib/packaged-bin.js");
    if !entry.is_file() {
        return Err(Error::new(
            ErrorKind::FileNotFound,
            format!(
                "the dev-only node runtime closure is missing at {} (no {}); run `cargo run --locked -p seekdeep-python-release --bin build-exe-for-python-sdk --` in a seekdeep-harness checkout, which builds and copies the native development carrier here. The node carrier is for repo-local development only — production uses the single-file exe.",
                node_root.display(),
                entry.display()
            ),
        ));
    }
    let node = find_node()?.ok_or_else(|| Error::new(ErrorKind::FileNotFound, "the node runtime mode needs a system `node` (>=22.19) on PATH; install Node.js or use the exe mode"))?;
    Ok(vec![node, entry.to_string_lossy().into_owned()])
}

/// Resolves existing symlinks while allowing a not-yet-created final path.
///
/// # Errors
/// Returns non-NotFound filesystem failures encountered while resolving an existing prefix.
pub fn resolve_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut prefix = path.as_path();
    let mut suffix = Vec::new();
    loop {
        match prefix.canonicalize() {
            Ok(mut resolved) => {
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved.clean());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = prefix.file_name() else {
                    return Ok(path.clean());
                };
                suffix.push(name.to_owned());
                let Some(parent) = prefix.parent() else {
                    return Ok(path.clean());
                };
                prefix = parent;
            }
            Err(error) => return Err(Error::io(&error, Some(path.to_string_lossy().into_owned()))),
        }
    }
}
