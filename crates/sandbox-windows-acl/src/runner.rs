//! Stable `windows-acl-run` argv parsing and pre-native validation.

use std::path::{Path, PathBuf};

use crate::{
    AclSandboxMode, assert_temp_root_outside_workspace, temp_write_sid, workspace_write_sid,
};

/// Runner-owned fatal stderr signature.
pub const RUNNER_SIGNATURE: &str = "windows-acl-run";
/// Runner-owned fatal exit status.
pub const RUNNER_FAILURE_EXIT: i32 = 127;

/// Parsed stable runner invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRunnerArgs {
    /// Existing workspace root.
    pub workspace: PathBuf,
    /// Existing ambient temp root or caller-owned private directory.
    pub temp: PathBuf,
    /// File-effect mode.
    pub mode: AclSandboxMode,
    /// Optional seam-owned workspace SID.
    pub write_sid: Option<String>,
    /// Optional seam-owned private-temp SID.
    pub temp_write_sid: Option<String>,
    /// Non-empty command argv.
    pub command: Vec<String>,
}

/// Runner boundary failure printed with [`RUNNER_SIGNATURE`].
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{detail}")]
pub struct RunnerFailure {
    detail: String,
}

impl RunnerFailure {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Exact stderr line without its trailing newline.
    #[must_use]
    pub fn diagnostic(&self) -> String {
        format!("{RUNNER_SIGNATURE}: {}", self.detail)
    }
}

/// Parses the stable runner grammar without consulting the filesystem.
///
/// # Errors
///
/// Returns exact source runner diagnostics.
pub fn parse_runner_args(raw: &[String]) -> Result<ParsedRunnerArgs, RunnerFailure> {
    let mut workspace = None;
    let mut temp = None;
    let mut mode = None;
    let mut write_sid = None;
    let mut temp_write_sid = None;
    let mut index = 0;
    while index < raw.len() {
        let token = &raw[index];
        if token == "--" {
            index += 1;
            break;
        }
        index += 1;
        let value = raw
            .get(index)
            .ok_or_else(|| RunnerFailure::new(format!("missing value after {token}")))?;
        match token.as_str() {
            "--workspace" => workspace = Some(PathBuf::from(value)),
            "--temp" => temp = Some(PathBuf::from(value)),
            "--mode" => mode = Some(value.clone()),
            "--write-sid" => write_sid = Some(value.clone()),
            "--temp-write-sid" => temp_write_sid = Some(value.clone()),
            _ => return Err(RunnerFailure::new(format!("unknown argument: {token}"))),
        }
        index += 1;
    }
    let workspace = workspace.ok_or_else(|| RunnerFailure::new("missing --workspace"))?;
    let temp = temp.ok_or_else(|| RunnerFailure::new("missing --temp"))?;
    let mode = match mode.as_deref() {
        Some("read-only") => AclSandboxMode::ReadOnly,
        Some("workspace-write") => AclSandboxMode::WorkspaceWrite,
        other => {
            return Err(RunnerFailure::new(format!(
                "unknown mode: {}",
                other.unwrap_or("undefined")
            )));
        }
    };
    let command = raw[index..].to_vec();
    if command.is_empty() {
        return Err(RunnerFailure::new("missing command after --"));
    }
    Ok(ParsedRunnerArgs {
        workspace,
        temp,
        mode,
        write_sid,
        temp_write_sid,
        command,
    })
}

/// Validates directories, SID pairing, boundary separation, and SID ownership.
///
/// # Errors
///
/// Returns one runner-owned fail-closed diagnostic.
pub fn validate_runner_args(parsed: &ParsedRunnerArgs) -> Result<(), RunnerFailure> {
    require_directory("--workspace", &parsed.workspace)?;
    require_directory("--temp", &parsed.temp)?;
    let seam_managed = parsed.write_sid.is_some() || parsed.temp_write_sid.is_some();
    if parsed.mode == AclSandboxMode::ReadOnly && seam_managed {
        return Err(RunnerFailure::new(
            "read-only does not accept --write-sid or --temp-write-sid",
        ));
    }
    if parsed.mode == AclSandboxMode::WorkspaceWrite
        && (parsed.write_sid.is_some() != parsed.temp_write_sid.is_some())
    {
        return Err(RunnerFailure::new(
            "workspace-write requires --write-sid and --temp-write-sid together",
        ));
    }
    if parsed.mode == AclSandboxMode::WorkspaceWrite {
        assert_temp_root_outside_workspace(&parsed.workspace, &parsed.temp)
            .map_err(|error| RunnerFailure::new(error.to_string()))?;
        if seam_managed {
            let workspace = path_string(&parsed.workspace)?;
            let temp = path_string(&parsed.temp)?;
            if parsed.write_sid.as_deref() != Some(workspace_write_sid(workspace).as_str()) {
                return Err(RunnerFailure::new("--write-sid does not match --workspace"));
            }
            if parsed.temp_write_sid.as_deref() != Some(temp_write_sid(temp).as_str()) {
                return Err(RunnerFailure::new("--temp-write-sid does not match --temp"));
            }
        }
    }
    Ok(())
}

fn require_directory(label: &str, path: &Path) -> Result<(), RunnerFailure> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(RunnerFailure::new(format!(
            "{label} is not an existing directory: {}",
            path.display()
        )))
    }
}

fn path_string(path: &Path) -> Result<&str, RunnerFailure> {
    path.to_str()
        .ok_or_else(|| RunnerFailure::new("runner path is not valid Unicode"))
}
