//! Provider-lifetime standing and session-lifetime revocable ACL grants.

use std::{path::PathBuf, sync::Arc};

use crate::{AclBindings, NativePointer, Win32Error, grant_write, revoke_write};

/// One parsed capability SID produced by the native binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSid {
    /// LocalAlloc-family pointer freed when the grant is disposed.
    pub pointer: NativePointer,
    /// Exact bounded SID bytes used for standing-ACE detection.
    pub bytes: Vec<u8>,
}

/// SID parsing added to the ACL binding table.
pub trait GrantBindings: AclBindings {
    /// Parses an SDDL SID and returns its allocation and bounded bytes.
    ///
    /// # Errors
    ///
    /// Returns the exact `ConvertStringSidToSidW` failure.
    fn convert_string_sid(&self, sid: &str) -> Result<ParsedSid, Win32Error>;
}

/// Aggregated best-effort grant cleanup failure.
#[derive(Debug, thiserror::Error)]
#[error("AclWriteGrant dispose completed with {} cleanup failure(s)", .failures.len())]
pub struct GrantDisposeError {
    /// Every revocation or SID-free failure in cleanup order.
    pub failures: Vec<Win32Error>,
}

/// One capability SID and all paths whose DACL currently carries its ACE.
pub struct AclWriteGrant {
    api: Arc<dyn GrantBindings>,
    sid: ParsedSid,
    write_sid: String,
    revocable_paths: Vec<PathBuf>,
    standing_paths: Vec<PathBuf>,
}

impl std::fmt::Debug for AclWriteGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AclWriteGrant")
            .field("write_sid", &self.write_sid)
            .field("revocable_paths", &self.revocable_paths)
            .field("standing_paths", &self.standing_paths)
            .finish_non_exhaustive()
    }
}

impl AclWriteGrant {
    /// Parses the SID before any ACE is granted.
    ///
    /// # Errors
    ///
    /// Returns a SID parser failure or a successful-but-null pointer failure.
    pub fn create(
        write_sid: impl Into<String>,
        api: Arc<dyn GrantBindings>,
    ) -> Result<Self, Win32Error> {
        let write_sid = write_sid.into();
        let sid = api.convert_string_sid(&write_sid)?;
        if sid.pointer.is_null() {
            return Err(Win32Error::new(
                "ConvertStringSidToSidW",
                api.last_error(),
                Some(format!("null SID for {write_sid}")),
            ));
        }
        Ok(Self {
            api,
            sid,
            write_sid,
            revocable_paths: Vec::new(),
            standing_paths: Vec::new(),
        })
    }

    /// The exact SDDL capability identity.
    #[must_use]
    pub fn write_sid(&self) -> &str {
        &self.write_sid
    }

    /// Adds one path, recording it before the fallible grant operation.
    ///
    /// # Errors
    ///
    /// Returns the first lock or ACL-edit failure. The path remains recorded
    /// so caller-driven disposal can revoke a post-apply failure.
    pub fn add(&mut self, path: &std::path::Path, standing: bool) -> Result<(), Win32Error> {
        let path = path.to_owned();
        if standing {
            self.standing_paths.push(path.clone());
        } else {
            self.revocable_paths.push(path.clone());
        }
        grant_write(self.api.as_ref(), &path, self.sid.pointer, &self.sid.bytes)
    }

    /// Returns standing paths first, followed by revocable paths, in grant order.
    #[must_use]
    pub fn paths(&self) -> Vec<&std::path::Path> {
        self.standing_paths
            .iter()
            .chain(&self.revocable_paths)
            .map(PathBuf::as_path)
            .collect()
    }

    /// Revokes every session-owned path and frees the parsed SID best-effort.
    ///
    /// # Errors
    ///
    /// Returns every cleanup failure in revocation-then-free order.
    pub fn dispose(self) -> Result<(), GrantDisposeError> {
        let mut failures = Vec::new();
        for path in &self.revocable_paths {
            if let Err(error) = revoke_write(self.api.as_ref(), path, self.sid.pointer) {
                failures.push(error);
            }
        }
        if !self.api.local_free(self.sid.pointer).is_null() {
            failures.push(Win32Error::new(
                "LocalFree",
                self.api.last_error(),
                Some("write SID".into()),
            ));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(GrantDisposeError { failures })
        }
    }
}
