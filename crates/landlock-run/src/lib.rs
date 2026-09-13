//! CLI contract and process-independent entry API for the Rust `landlock-run` launcher.

use std::{
    ffi::{OsStr, OsString},
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::error::Error as _;

/// Launcher file name and fatal-diagnostic prefix.
pub const LAUNCHER_BIN: &str = "landlock-run";
/// Exit status reserved for launcher-level failures.
pub const LAUNCHER_FAILURE_EXIT: i32 = 125;
/// Fatal line prefix paired with [`LAUNCHER_FAILURE_EXIT`].
pub const FATAL_PREFIX: &str = "landlock-run: ";
/// Notice emitted before exec when the kernel only supports an older ABI.
pub const PARTIAL_NOTICE: &str = "landlock-run: partial enforcement (older Landlock ABI)";

/// Result of the functional launcher probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandlockEnforcement {
    /// Every filesystem access known to launcher ABI 5 is enforced.
    Full,
    /// An older ABI enforces the subset it understands.
    Partial,
    /// The binary is missing, timed out, failed, or could not enforce Landlock.
    Unusable,
}

/// Ordered filesystem grants for one launcher invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherGrants {
    /// Roots granted read and execute access.
    pub read_only: Vec<PathBuf>,
    /// Roots granted every filesystem access supported by launcher ABI 5.
    pub read_write: Vec<PathBuf>,
}

/// Builds the private `--ro`/`--rw` CLI vocabulary in source order.
#[must_use]
pub fn grant_args(grants: &LauncherGrants) -> Vec<OsString> {
    grants
        .read_only
        .iter()
        .flat_map(|root| [OsString::from("--ro"), root.as_os_str().to_owned()])
        .chain(
            grants
                .read_write
                .iter()
                .flat_map(|root| [OsString::from("--rw"), root.as_os_str().to_owned()]),
        )
        .collect()
}

/// Resolves the installed launcher beside the currently running executable.
///
/// The ambient environment never participates in selection. The returned
/// path is absolute even when the sibling is absent; [`probe`] is the only
/// availability signal.
///
/// # Errors
///
/// Returns when the operating system cannot identify the current executable.
pub fn launcher_path() -> std::io::Result<PathBuf> {
    launcher_path_from(&std::env::current_exe()?)
}

/// Deterministic form of [`launcher_path`] used by packaging tests.
///
/// # Errors
///
/// Returns when `current_executable` has no parent directory.
pub fn launcher_path_from(current_executable: &Path) -> std::io::Result<PathBuf> {
    let current_executable = if current_executable.is_absolute() {
        current_executable.to_owned()
    } else {
        std::env::current_dir()?.join(current_executable)
    };
    let directory = current_executable.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "current executable has no parent directory",
        )
    })?;
    Ok(directory.join(LAUNCHER_BIN))
}

/// Runs the bounded functional probe and parses its one-line report.
#[must_use]
pub fn probe(launcher: &Path, timeout: Duration) -> LandlockEnforcement {
    let Ok(mut child) = Command::new(launcher)
        .arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return LandlockEnforcement::Unusable;
    };
    let deadline = Instant::now().checked_add(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return LandlockEnforcement::Unusable;
            }
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            return LandlockEnforcement::Unusable;
        }
        thread::sleep(Duration::from_millis(5));
    };
    if !status.success() {
        return LandlockEnforcement::Unusable;
    }
    let mut stdout = String::new();
    if child
        .stdout
        .take()
        .is_none_or(|mut output| output.read_to_string(&mut stdout).is_err())
    {
        return LandlockEnforcement::Unusable;
    }
    if stdout.contains("partially enforced") {
        LandlockEnforcement::Partial
    } else {
        LandlockEnforcement::Full
    }
}

/// Parsed launcher request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherRequest {
    /// Functional kernel-enforcement probe.
    Probe,
    /// Apply ordered grants, then replace the launcher with the command.
    Run {
        /// Ordered grants.
        grants: LauncherGrants,
        /// Non-empty argv after the `--` separator.
        command: Vec<OsString>,
    },
}

/// Fail-closed CLI parsing or restriction failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LauncherError {
    /// Invalid CLI grammar.
    #[error("usage error: {0}")]
    Usage(String),
    /// Ruleset setup, restriction, or exec failure.
    #[error("{0}")]
    Fatal(String),
}

impl LauncherError {
    /// Exact launcher-owned stderr line without its trailing newline.
    #[must_use]
    pub fn diagnostic(&self) -> String {
        format!("{FATAL_PREFIX}{self}")
    }
}

/// Parses all arguments after argv[0].
///
/// # Errors
///
/// Returns the source-compatible usage diagnostic before any restriction or exec.
pub fn parse_args<I, S>(args: I) -> Result<LauncherRequest, LauncherError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == OsStr::new("--probe")) {
        return if args.len() == 1 {
            Ok(LauncherRequest::Probe)
        } else {
            Err(LauncherError::Usage(
                "--probe takes no other arguments".to_owned(),
            ))
        };
    }
    let mut grants = LauncherGrants::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--ro" | "--rw") => {
                let flag = args[index].to_string_lossy();
                let Some(path) = args.get(index + 1) else {
                    return Err(LauncherError::Usage(format!("{flag} requires a path")));
                };
                if flag == "--ro" {
                    grants.read_only.push(PathBuf::from(path));
                } else {
                    grants.read_write.push(PathBuf::from(path));
                }
                index += 2;
            }
            Some("--") => {
                let command = args[index + 1..].to_vec();
                if command.is_empty() {
                    return Err(LauncherError::Usage(
                        "missing `-- <argv>...` command".to_owned(),
                    ));
                }
                return Ok(LauncherRequest::Run { grants, command });
            }
            _ => {
                return Err(LauncherError::Usage(format!(
                    "unknown argument: {}",
                    args[index].to_string_lossy()
                )));
            }
        }
    }
    Err(LauncherError::Usage(
        "missing `-- <argv>...` command".to_owned(),
    ))
}

/// Applies the request, printing only the source-compatible report/notice.
///
/// A successful run request replaces the process and does not return.
///
/// # Errors
///
/// Returns before exec whenever confinement cannot be enforced, a grant is
/// unusable, or process replacement fails.
pub fn execute(request: LauncherRequest) -> Result<(), LauncherError> {
    match request {
        LauncherRequest::Probe => {
            let partial = restrict(&LauncherGrants {
                read_only: vec![PathBuf::from("/")],
                read_write: Vec::new(),
            })?;
            println!(
                "landlock: {}",
                if partial {
                    "partially enforced (older ABI)"
                } else {
                    "fully enforced"
                }
            );
            Ok(())
        }
        LauncherRequest::Run { grants, command } => {
            let partial = restrict(&grants)?;
            if partial {
                eprintln!("{PARTIAL_NOTICE}");
            }
            exec(&command)
        }
    }
}

#[cfg(target_os = "linux")]
fn restrict(grants: &LauncherGrants) -> Result<bool, LauncherError> {
    use landlock::{
        ABI, Access, AccessFs, Compatible as _, PathBeneath, PathFd, Ruleset, RulesetAttr as _,
        RulesetCreatedAttr as _, RulesetStatus,
    };

    let abi = ABI::V5;
    let mut created = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .and_then(Ruleset::create)
        .map_err(|error| LauncherError::Fatal(format!("landlock ruleset error: {error}")))?;
    for (paths, access) in [
        (&grants.read_only, AccessFs::from_read(abi)),
        (&grants.read_write, AccessFs::from_all(abi)),
    ] {
        for path in paths {
            let fd = PathFd::new(path).map_err(|error| {
                let detail = error
                    .source()
                    .map_or_else(|| error.to_string(), ToString::to_string);
                LauncherError::Fatal(format!(
                    "cannot open rule path: {}: {detail}",
                    path.display()
                ))
            })?;
            let access = if path.is_file() {
                access
                    & (AccessFs::Execute
                        | AccessFs::WriteFile
                        | AccessFs::ReadFile
                        | AccessFs::Truncate
                        | AccessFs::IoctlDev)
            } else {
                access
            };
            created = created
                .add_rule(PathBeneath::new(fd, access))
                .map_err(|error| {
                    LauncherError::Fatal(format!("landlock ruleset error: {error}"))
                })?;
        }
    }
    let status = created
        .set_compatibility(landlock::CompatLevel::BestEffort)
        .restrict_self()
        .map_err(|error| LauncherError::Fatal(format!("landlock ruleset error: {error}")))?;
    match status.ruleset {
        RulesetStatus::FullyEnforced => Ok(false),
        RulesetStatus::PartiallyEnforced => Ok(true),
        RulesetStatus::NotEnforced => Err(LauncherError::Fatal(
            "landlock is not enforced by this kernel (ABI unsupported or disabled)".to_owned(),
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn restrict(_grants: &LauncherGrants) -> Result<bool, LauncherError> {
    Err(LauncherError::Fatal(
        "landlock is not enforced by this kernel (ABI unsupported or disabled)".to_owned(),
    ))
}

#[cfg(unix)]
fn exec(command: &[OsString]) -> Result<(), LauncherError> {
    use std::os::unix::process::CommandExt as _;

    let error = Command::new(&command[0]).args(&command[1..]).exec();
    Err(LauncherError::Fatal(format!("exec failed: {error}")))
}

#[cfg(not(unix))]
fn exec(_command: &[OsString]) -> Result<(), LauncherError> {
    Err(LauncherError::Fatal(
        "exec failed: process replacement is unavailable on this platform".to_owned(),
    ))
}
