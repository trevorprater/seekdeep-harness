//! Windows restricted-token and ACL confinement contracts.

pub mod abi;
pub mod acl;
pub mod error;
pub mod grant;
pub mod invariant;
pub mod path_boundary;
pub mod runner;
pub mod sandbox;
pub mod spawn;
pub mod token;
pub mod workspace_sid;

pub use acl::{
    AclBindings, AclRead, AclWithPointer, NativeHandle, NativePointer, SetEntriesResult,
    build_explicit_access, grant_write, lock_file_path, revoke_write, with_path_lock,
};
pub use error::Win32Error;
pub use grant::{AclWriteGrant, GrantBindings, GrantDisposeError, ParsedSid};
pub use path_boundary::{assert_private_temp_disjoint, assert_temp_root_outside_workspace};
pub use runner::{
    ParsedRunnerArgs, RUNNER_FAILURE_EXIT, RUNNER_SIGNATURE, RunnerFailure, parse_runner_args,
    validate_runner_args,
};
pub use sandbox::{
    AclSandbox, AclSandboxChild, AclSandboxChildResult, AclSandboxError, AclTempDirState,
    SandboxStdio, WindowsAclBindings,
};
pub use spawn::{
    PeekResult, ProcessInfo, SpawnBindings, SpawnError, SpawnOptions, SpawnedInherited,
    SpawnedNative, StartupHandles, build_command_line, drain_pipe, quote_arg, spawn_sandboxed,
    spawn_sandboxed_inherited, wait_for_exit,
};
pub use token::{
    RestrictingSidSet, TokenBindings, TokenError, TokenSid, create_restricted_token,
    find_logon_sid, make_well_known_sid, open_current_process_token, set_token_default_dacl_grant,
};
pub use workspace_sid::{temp_write_sid, workspace_write_sid};

use std::path::{Path, PathBuf};

/// File-effect mode accepted by the Windows restricted-token runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AclSandboxMode {
    /// Carry no capability SID.
    ReadOnly,
    /// Carry the workspace and optional private-temp capability SIDs.
    WorkspaceWrite,
}

impl AclSandboxMode {
    /// Exact runner wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

/// Constructor inputs whose dependent shape is validated before any Win32 call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclSandboxOptions {
    /// Existing directories receiving the standing workspace grant.
    pub writable_dirs: Vec<PathBuf>,
    /// Explicit private temp directory, or `None` to disable temp writes.
    pub temp_dir: Option<PathBuf>,
    /// Distinguishes explicit `null` from an omitted temp option.
    pub temp_was_explicit: bool,
    /// Workspace capability SID.
    pub write_sid: Option<String>,
    /// Private-temp capability SID.
    pub temp_write_sid: Option<String>,
    /// Restricted-token capability list.
    pub mode: AclSandboxMode,
    /// Whether this instance owns DACL materialization and revocation.
    pub manage_dacls: bool,
}

/// Constructor-validated absolute paths and capability identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAclSandboxOptions {
    /// Absolute existing writable directories.
    pub writable_dirs: Vec<PathBuf>,
    /// Absolute existing private temp directory.
    pub temp_dir: Option<PathBuf>,
    /// Workspace capability SID.
    pub write_sid: Option<String>,
    /// Private-temp capability SID.
    pub temp_write_sid: Option<String>,
    /// Restricted-token capability list.
    pub mode: AclSandboxMode,
    /// DACL ownership boundary.
    pub manage_dacls: bool,
}

impl AclSandboxOptions {
    /// Validates the complete constructor shape before native initialization.
    ///
    /// # Errors
    ///
    /// Returns exact dependent-option or filesystem diagnostics.
    pub fn resolve(&self) -> anyhow::Result<ResolvedAclSandboxOptions> {
        let writable_dirs = self
            .writable_dirs
            .iter()
            .map(|directory| absolute_existing_directory("AclSandbox writable dir", directory))
            .collect::<anyhow::Result<Vec<_>>>()?;
        match self.mode {
            AclSandboxMode::WorkspaceWrite => {
                anyhow::ensure!(
                    self.write_sid.is_some(),
                    "AclSandbox workspace-write requires a write SID — derive it from the workspace via workspaceWriteSid()"
                );
                anyhow::ensure!(
                    self.temp_was_explicit,
                    "AclSandbox workspace-write requires an explicit private temp directory or null"
                );
                anyhow::ensure!(
                    self.temp_dir.is_none() || self.temp_write_sid.is_some(),
                    "AclSandbox workspace-write with temp requires a temp write SID — derive it via tempWriteSid()"
                );
            }
            AclSandboxMode::ReadOnly => {
                anyhow::ensure!(
                    self.temp_dir.is_none(),
                    "AclSandbox read-only does not accept a temp directory"
                );
                anyhow::ensure!(
                    self.write_sid.is_none() && self.temp_write_sid.is_none(),
                    "AclSandbox read-only does not accept write SIDs"
                );
            }
        }
        anyhow::ensure!(
            self.temp_dir.is_some() || self.temp_write_sid.is_none(),
            "AclSandbox temp write SID requires a temp directory"
        );
        anyhow::ensure!(
            self.write_sid.is_none() || self.write_sid != self.temp_write_sid,
            "AclSandbox workspace and temp write SIDs must be distinct"
        );
        // Source parity: writable roots fail at construction, while the temp
        // path remains caller-spelled and is validated immediately before
        // native initialization.
        let temp_dir = self.temp_dir.clone();
        Ok(ResolvedAclSandboxOptions {
            writable_dirs,
            temp_dir,
            write_sid: self.write_sid.clone(),
            temp_write_sid: self.temp_write_sid.clone(),
            mode: self.mode,
            manage_dacls: self.manage_dacls,
        })
    }
}

fn absolute_existing_directory(label: &str, directory: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if directory.is_absolute() {
        directory.to_owned()
    } else {
        std::env::current_dir()?.join(directory)
    };
    anyhow::ensure!(
        absolute.is_dir(),
        "{label} does not exist or is not a directory: {}",
        absolute.display()
    );
    Ok(absolute)
}
