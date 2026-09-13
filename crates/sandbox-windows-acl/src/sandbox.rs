//! Single-owner restricted-token, grant, child, and cleanup lifecycle.

use std::{path::PathBuf, sync::Arc};

use tokio::sync::OnceCell;

use crate::{
    AclBindings, AclSandboxMode, AclSandboxOptions, GrantBindings, NativeHandle, NativePointer,
    ParsedSid, ResolvedAclSandboxOptions, RestrictingSidSet, SpawnBindings, SpawnError,
    SpawnOptions, TokenBindings, TokenError, TokenSid, Win32Error, abi,
    assert_private_temp_disjoint, create_restricted_token, drain_pipe, find_logon_sid, grant_write,
    make_well_known_sid, open_current_process_token, revoke_write, set_token_default_dacl_grant,
    spawn_sandboxed, spawn_sandboxed_inherited, wait_for_exit,
};

/// Complete native binding table used by one sandbox owner.
pub trait WindowsAclBindings:
    AclBindings + GrantBindings + TokenBindings + SpawnBindings + Send + Sync
{
    /// Releases caller-owned SID storage created by token helper calls.
    ///
    /// Native Rust adapters normally return null after dropping the owned
    /// buffer. Injected bindings may return non-null to exercise the source's
    /// checked `LocalFree` cleanup contract.
    fn free_token_sid(&self, sid: TokenSid) -> NativePointer;
}

/// Sandbox lifecycle error retaining primary and best-effort cleanup failures.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum AclSandboxError {
    /// Source-owned state or filesystem diagnostic.
    #[error("{0}")]
    Message(String),
    /// Checked Win32 failure.
    #[error(transparent)]
    Win32(#[from] Win32Error),
    /// Restricted-token pipeline failure.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// Native child pipeline failure.
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    /// An init primary failure accompanied by cleanup failures.
    #[error(
        "AclSandbox init failed and {} cleanup operation(s) also failed",
        .cleanup.len()
    )]
    InitCleanup {
        /// Original init failure.
        primary: Box<AclSandboxError>,
        /// Cleanup failures in source order.
        cleanup: Vec<AclSandboxError>,
    },
    /// Aggregated dispose failures.
    #[error(
        "AclSandbox dispose completed with {} cleanup failure(s)",
        .failures.len()
    )]
    DisposeCleanup {
        /// Cleanup failures in source order.
        failures: Vec<AclSandboxError>,
    },
}

/// Child stdio shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxStdio {
    /// Capture stdout and stderr, close stdin.
    #[default]
    Pipe,
    /// Pass the runner's stdio through and retain a kill-on-close job.
    Inherit,
}

/// Source-compatible temp getter state (`undefined`, `null`, or a directory).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AclTempDirState {
    /// Initialization has not completed.
    #[default]
    Unresolved,
    /// Temp writes are disabled.
    Disabled,
    /// Existing private temp directory selected for this instance.
    Enabled(PathBuf),
}

#[derive(Debug)]
struct InitializedState {
    token: NativeHandle,
    write_sid: Option<ParsedSid>,
    temp_write_sid: Option<ParsedSid>,
    token_sids: Vec<TokenSid>,
    revocable_grants: Vec<(PathBuf, NativePointer)>,
}

/// One write-restricted sandbox instance.
pub struct AclSandbox {
    options: ResolvedAclSandboxOptions,
    api: Arc<dyn WindowsAclBindings>,
    temp_dir_resolved: AclTempDirState,
    initialized: Option<InitializedState>,
}

impl std::fmt::Debug for AclSandbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AclSandbox")
            .field("options", &self.options)
            .field("temp_dir_resolved", &self.temp_dir_resolved)
            .field("initialized", &self.initialized.is_some())
            .finish_non_exhaustive()
    }
}

impl AclSandbox {
    /// Constructor-validates dependent options and writable roots.
    ///
    /// # Errors
    ///
    /// Returns the exact dependent-option or writable-directory diagnostic.
    pub fn new(
        options: &AclSandboxOptions,
        api: Arc<dyn WindowsAclBindings>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            options: options.resolve()?,
            api,
            temp_dir_resolved: AclTempDirState::Unresolved,
            initialized: None,
        })
    }

    /// Absolute writable roots resolved at construction.
    #[must_use]
    pub fn writable_dirs(&self) -> &[PathBuf] {
        &self.options.writable_dirs
    }

    /// Resolved temp state matching source `undefined`/`null`/path semantics.
    #[must_use]
    pub const fn temp_dir(&self) -> &AclTempDirState {
        &self.temp_dir_resolved
    }

    fn parse_sid(&self, spelling: &str) -> Result<ParsedSid, AclSandboxError> {
        let parsed = GrantBindings::convert_string_sid(self.api.as_ref(), spelling)?;
        if parsed.pointer.is_null() {
            return Err(Win32Error::new(
                "ConvertStringSidToSidW",
                AclBindings::last_error(self.api.as_ref()),
                Some(spelling.to_owned()),
            )
            .into());
        }
        Ok(parsed)
    }

    fn close_error(&self, handle: NativeHandle, detail: &str) -> Option<AclSandboxError> {
        if AclBindings::close_handle(self.api.as_ref(), handle) {
            None
        } else {
            Some(
                Win32Error::new(
                    "CloseHandle",
                    AclBindings::last_error(self.api.as_ref()),
                    Some(detail.to_owned()),
                )
                .into(),
            )
        }
    }

    fn free_parsed_sid(&self, sid: Option<ParsedSid>, label: &str) -> Option<AclSandboxError> {
        let sid = sid?;
        if AclBindings::local_free(self.api.as_ref(), sid.pointer).is_null() {
            None
        } else {
            Some(
                Win32Error::new(
                    "LocalFree",
                    AclBindings::last_error(self.api.as_ref()),
                    Some(label.to_owned()),
                )
                .into(),
            )
        }
    }

    fn free_token_sid(&self, sid: TokenSid, label: &str) -> Option<AclSandboxError> {
        if self.api.free_token_sid(sid).is_null() {
            None
        } else {
            Some(
                Win32Error::new(
                    "LocalFree",
                    AclBindings::last_error(self.api.as_ref()),
                    Some(label.to_owned()),
                )
                .into(),
            )
        }
    }

    /// Materializes grants and constructs the restricted primary token once.
    ///
    /// # Errors
    ///
    /// Returns the primary fail-closed error, aggregated with every cleanup
    /// failure when rollback is not completely clean.
    #[allow(clippy::too_many_lines)] // one owner preserves the source rollback order
    pub fn init(&mut self, pid: u32) -> Result<(), AclSandboxError> {
        if self.initialized.is_some() {
            return Err(AclSandboxError::Message(
                "AclSandbox is already initialized".into(),
            ));
        }
        let current_token = open_current_process_token(self.api.as_ref(), pid)?;
        let mut current_token_open = true;
        let mut restricted_token = None;
        let mut write_sid = None;
        let mut temp_write_sid = None;
        let mut token_sids = Vec::new();
        let mut grants = Vec::new();

        let attempt = (|| -> Result<(), AclSandboxError> {
            write_sid = self
                .options
                .write_sid
                .as_deref()
                .map(|sid| self.parse_sid(sid))
                .transpose()?;
            temp_write_sid = self
                .options
                .temp_write_sid
                .as_deref()
                .map(|sid| self.parse_sid(sid))
                .transpose()?;

            let temp_dir = if self.options.mode == AclSandboxMode::ReadOnly {
                None
            } else {
                self.options.temp_dir.clone()
            };
            if let Some(temp_dir) = &temp_dir {
                if !temp_dir.is_dir() {
                    return Err(AclSandboxError::Message(format!(
                        "AclSandbox temp dir does not exist or is not a directory: {}",
                        temp_dir.display()
                    )));
                }
                assert_private_temp_disjoint(&self.options.writable_dirs, temp_dir)
                    .map_err(|error| AclSandboxError::Message(error.to_string()))?;
            }
            self.temp_dir_resolved = temp_dir
                .clone()
                .map_or(AclTempDirState::Disabled, AclTempDirState::Enabled);

            if self.options.manage_dacls
                && let Some(write) = &write_sid
            {
                for path in &self.options.writable_dirs {
                    grant_write(self.api.as_ref(), path, write.pointer, &write.bytes)?;
                }
                if let (Some(path), Some(temp_sid)) = (&temp_dir, &temp_write_sid) {
                    grants.push((path.clone(), temp_sid.pointer));
                    grant_write(self.api.as_ref(), path, temp_sid.pointer, &temp_sid.bytes)?;
                }
            }

            let logon = find_logon_sid(self.api.as_ref(), current_token)?;
            token_sids.push(logon);
            let world = make_well_known_sid(self.api.as_ref(), abi::WIN_WORLD_SID)?;
            token_sids.push(world);
            let write_pointers = [
                write_sid.as_ref().map(|sid| sid.pointer),
                temp_write_sid.as_ref().map(|sid| sid.pointer),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let known = RestrictingSidSet {
                world: token_sids[1].clone(),
            };
            let token = create_restricted_token(
                self.api.as_ref(),
                current_token,
                &token_sids[0],
                &write_pointers,
                &known,
                self.options.mode,
            )?;
            restricted_token = Some(token);
            let default_sid = temp_write_sid
                .as_ref()
                .or(write_sid.as_ref())
                .map_or_else(|| token_sids[1].pointer(), |sid| sid.pointer);
            set_token_default_dacl_grant(self.api.as_ref(), token, default_sid)?;
            if let Some(error) = self.close_error(current_token, "current process token") {
                return Err(error);
            }
            current_token_open = false;
            Ok(())
        })();

        if let Err(primary) = attempt {
            let mut cleanup = Vec::new();
            if current_token_open
                && let Some(error) =
                    self.close_error(current_token, "current process token after init failure")
            {
                cleanup.push(error);
            }
            if let Some(token) = restricted_token
                && let Some(error) = self.close_error(token, "restricted token after init failure")
            {
                cleanup.push(error);
            }
            for (path, sid) in &grants {
                if let Err(error) = revoke_write(self.api.as_ref(), path, *sid) {
                    cleanup.push(error.into());
                }
            }
            if let Some(error) = self.free_parsed_sid(write_sid, "workspace write SID") {
                cleanup.push(error);
            }
            if let Some(error) = self.free_parsed_sid(temp_write_sid, "temp write SID") {
                cleanup.push(error);
            }
            for sid in token_sids {
                if let Some(error) = self.free_token_sid(sid, "init SID allocation") {
                    cleanup.push(error);
                }
            }
            self.temp_dir_resolved = AclTempDirState::Unresolved;
            if cleanup.is_empty() {
                return Err(primary);
            }
            return Err(AclSandboxError::InitCleanup {
                primary: Box::new(primary),
                cleanup,
            });
        }

        let token = restricted_token.ok_or_else(|| {
            AclSandboxError::Message("AclSandbox restricted token was not resolved".into())
        })?;
        self.initialized = Some(InitializedState {
            token,
            write_sid,
            temp_write_sid,
            token_sids,
            revocable_grants: grants,
        });
        Ok(())
    }

    /// Spawns a child under the initialized restricted token.
    ///
    /// # Errors
    ///
    /// Returns an uninitialized-state diagnostic or the exact native spawn failure.
    pub fn spawn(
        &self,
        command: &str,
        args: &[String],
        cwd: &std::path::Path,
        stdio: SandboxStdio,
    ) -> Result<AclSandboxChild, AclSandboxError> {
        let state = self.initialized.as_ref().ok_or_else(|| {
            AclSandboxError::Message("AclSandbox is not initialized: call init() first".into())
        })?;
        let options = SpawnOptions { command, args, cwd };
        let (pid, owned) = match stdio {
            SandboxStdio::Pipe => {
                let child = spawn_sandboxed(self.api.as_ref(), state.token, &options)?;
                (
                    child.pid,
                    ChildHandles::Pipe {
                        process: child.process,
                        stdout: child.stdout_read,
                        stderr: child.stderr_read,
                    },
                )
            }
            SandboxStdio::Inherit => {
                let child = spawn_sandboxed_inherited(self.api.as_ref(), state.token, &options)?;
                (
                    child.pid,
                    ChildHandles::Inherit {
                        process: child.process,
                        job: child.job,
                    },
                )
            }
        };
        Ok(AclSandboxChild::new(pid, self.api.clone(), owned))
    }

    /// Releases every owned resource; repeated or pre-init disposal is a no-op.
    ///
    /// # Errors
    ///
    /// Returns every revocation, SID free, and token close failure.
    pub fn dispose(&mut self) -> Result<(), AclSandboxError> {
        let Some(state) = self.initialized.take() else {
            return Ok(());
        };
        let mut failures = Vec::new();
        if self.options.manage_dacls {
            for (path, sid) in &state.revocable_grants {
                if let Err(error) = revoke_write(self.api.as_ref(), path, *sid) {
                    failures.push(error.into());
                }
            }
        }
        if let Some(error) = self.free_parsed_sid(state.write_sid, "workspace write SID") {
            failures.push(error);
        }
        if let Some(error) = self.free_parsed_sid(state.temp_write_sid, "temp write SID") {
            failures.push(error);
        }
        if let Some(error) = self.close_error(state.token, "restricted token") {
            failures.push(error);
        }
        for sid in state.token_sids {
            if let Some(error) = self.free_token_sid(sid, "init SID allocation") {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AclSandboxError::DisposeCleanup { failures })
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ChildHandles {
    Pipe {
        process: NativeHandle,
        stdout: NativeHandle,
        stderr: NativeHandle,
    },
    Inherit {
        process: NativeHandle,
        job: NativeHandle,
    },
}

/// Settled child result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclSandboxChildResult {
    /// Captured stdout, empty under inherited stdio.
    pub stdout: Vec<u8>,
    /// Captured stderr, empty under inherited stdio.
    pub stderr: Vec<u8>,
    /// Full unsigned Windows exit code.
    pub exit_code: u32,
}

/// One running child with idempotent settlement ownership.
pub struct AclSandboxChild {
    pid: u32,
    api: Arc<dyn WindowsAclBindings>,
    handles: ChildHandles,
    settled: OnceCell<Result<AclSandboxChildResult, SpawnError>>,
}

impl std::fmt::Debug for AclSandboxChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AclSandboxChild")
            .field("pid", &self.pid)
            .field("handles", &self.handles)
            .field("settled", &self.settled.initialized())
            .finish_non_exhaustive()
    }
}

impl AclSandboxChild {
    fn new(pid: u32, api: Arc<dyn WindowsAclBindings>, handles: ChildHandles) -> Self {
        Self {
            pid,
            api,
            handles,
            settled: OnceCell::new(),
        }
    }

    /// Child process ID.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Settles once; repeated calls return the same cloned result or error.
    ///
    /// # Errors
    ///
    /// Returns the cached pipe, wait, exit-code, or job-close failure.
    pub async fn wait(&self) -> Result<AclSandboxChildResult, SpawnError> {
        self.settled
            .get_or_init(|| async {
                match self.handles {
                    ChildHandles::Pipe {
                        process,
                        stdout,
                        stderr,
                    } => {
                        let (stdout, stderr) = tokio::join!(
                            drain_pipe(self.api.as_ref(), stdout),
                            drain_pipe(self.api.as_ref(), stderr)
                        );
                        let stdout = stdout?;
                        let stderr = stderr?;
                        let exit_code = wait_for_exit(self.api.as_ref(), process)?;
                        Ok(AclSandboxChildResult {
                            stdout,
                            stderr,
                            exit_code,
                        })
                    }
                    ChildHandles::Inherit { process, job } => {
                        let exit_code = wait_for_exit(self.api.as_ref(), process)?;
                        if !SpawnBindings::close_handle(self.api.as_ref(), job) {
                            return Err(Win32Error::new(
                                "CloseHandle",
                                SpawnBindings::last_error(self.api.as_ref()),
                                Some("kill-on-close job".into()),
                            )
                            .into());
                        }
                        Ok(AclSandboxChildResult {
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            exit_code,
                        })
                    }
                }
            })
            .await
            .clone()
    }
}
