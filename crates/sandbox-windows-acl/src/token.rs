//! Restricted-token construction and default-DACL adjustment.

use crate::{
    AclSandboxMode, NativeHandle, NativePointer, SetEntriesResult, Win32Error, abi,
    build_explicit_access,
};

/// An owned SID byte allocation whose address remains stable while borrowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenSid {
    bytes: Vec<u8>,
}

impl TokenSid {
    /// Creates a zeroed SID allocation of a verified size.
    #[must_use]
    pub fn zeroed(length: usize) -> Self {
        Self {
            bytes: vec![0; length],
        }
    }

    /// Borrows the exact allocated bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrows the allocation mutably for a native copy call.
    #[must_use]
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    /// Returns the allocation address as a typed ABI pointer.
    #[must_use]
    pub fn pointer(&self) -> NativePointer {
        NativePointer::from_raw(self.bytes.as_ptr() as usize as u64)
    }
}

/// Fail-closed token-pipeline error preserving source diagnostics.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    /// Checked Win32 failure.
    #[error(transparent)]
    Win32(#[from] Win32Error),
    /// The current token had no logon-session group.
    #[error("CreateRestrictedToken prerequisite failed: no logon SID found among {0} token groups")]
    NoLogonSid(u32),
    /// The token has no default DACL to extend.
    #[error("setTokenDefaultDaclGrant: the token carries no default DACL to extend")]
    NoDefaultDacl,
    /// Workspace-write was requested without a capability.
    #[error(
        "createRestrictedToken: workspace-write restricting list requires at least one write SID"
    )]
    MissingWriteSid,
}

/// Safe call seam implemented by the native Windows adapter.
pub trait TokenBindings: Send + Sync {
    /// Returns the calling thread's last Win32 error.
    fn last_error(&self) -> u32;
    /// Formats a Win32 error, or returns an empty string.
    fn format_message(&self, code: u32) -> String;
    /// Opens a real process handle for the current process ID.
    fn open_process(&self, desired_access: u32, inherit: bool, pid: u32) -> NativeHandle;
    /// Opens the process token and writes its handle when successful.
    fn open_process_token(
        &self,
        process: NativeHandle,
        desired_access: u32,
    ) -> (bool, Option<NativeHandle>);
    /// Closes a kernel handle.
    fn close_handle(&self, handle: NativeHandle) -> bool;
    /// Mirrors the two-call `GetTokenInformation` contract.
    fn get_token_information(
        &self,
        token: NativeHandle,
        class: u32,
        buffer: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool;
    /// Returns the exact SID byte length.
    fn get_length_sid(&self, sid: NativePointer) -> u32;
    /// Copies a SID into caller-owned stable storage.
    fn copy_sid(&self, destination: &mut [u8], source: NativePointer) -> bool;
    /// Creates a well-known SID in caller-owned stable storage.
    fn create_well_known_sid(&self, kind: u32, sid: &mut [u8], size: &mut u32) -> bool;
    /// Validates a caller-owned SID.
    fn is_valid_sid(&self, sid: &[u8]) -> bool;
    /// Merges an explicit ACE into a token's current default DACL.
    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        old_acl: NativePointer,
    ) -> SetEntriesResult;
    /// Replaces token information with a pointer-sized structure.
    fn set_token_information(&self, token: NativeHandle, class: u32, info: &[u8]) -> bool;
    /// Releases a LocalAlloc-family allocation; null means success.
    fn local_free(&self, pointer: NativePointer) -> NativePointer;
    /// Creates a restricted primary token from the exact pointer allowlist.
    fn create_restricted_token(
        &self,
        existing: NativeHandle,
        flags: u32,
        restricting_sids: &[NativePointer],
    ) -> (bool, Option<NativeHandle>);
}

fn last_error(
    api: &dyn TokenBindings,
    name: &'static str,
    detail: impl Into<String>,
) -> Win32Error {
    let code = api.last_error();
    let detail = detail.into();
    let detail = if detail.is_empty() {
        api.format_message(code)
    } else {
        detail
    };
    Win32Error::new(name, code, (!detail.is_empty()).then_some(detail))
}

fn returned_error(
    api: &dyn TokenBindings,
    name: &'static str,
    code: u32,
    detail: impl Into<String>,
) -> Win32Error {
    let detail = detail.into();
    let detail = if detail.is_empty() {
        api.format_message(code)
    } else {
        detail
    };
    Win32Error::new(name, code, (!detail.is_empty()).then_some(detail))
}

/// Opens the current process token with every right required downstream.
///
/// # Errors
///
/// Returns exact open/close failures and rejects a successful null token.
pub fn open_current_process_token(
    api: &dyn TokenBindings,
    pid: u32,
) -> Result<NativeHandle, TokenError> {
    let process = api.open_process(abi::PROCESS_QUERY_INFORMATION, false, pid);
    if process.is_invalid() {
        return Err(last_error(api, "OpenProcess", format!("pid {pid}")).into());
    }
    let access = abi::TOKEN_QUERY
        | abi::TOKEN_DUPLICATE
        | abi::TOKEN_ADJUST_DEFAULT
        | abi::TOKEN_ASSIGN_PRIMARY;
    let (opened, token) = api.open_process_token(process, access);
    if !opened {
        let code = api.last_error();
        let _ = api.close_handle(process);
        return Err(returned_error(api, "OpenProcessToken", code, format!("pid {pid}")).into());
    }
    if !api.close_handle(process) {
        return Err(last_error(api, "CloseHandle", "OpenProcess process handle").into());
    }
    token.ok_or_else(|| {
        returned_error(
            api,
            "OpenProcessToken",
            api.last_error(),
            "null token handle",
        )
        .into()
    })
}

fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buffer.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_pointer(buffer: &[u8], offset: usize) -> Option<NativePointer> {
    let raw = u64::from_le_bytes(buffer.get(offset..offset + 8)?.try_into().ok()?);
    (raw != 0).then(|| NativePointer::from_raw(raw))
}

/// Finds and copies the current token's logon SID.
///
/// # Errors
///
/// Returns exact information, length, or copy failures; rejects malformed
/// size probes and a token without a logon SID.
pub fn find_logon_sid(
    api: &dyn TokenBindings,
    token: NativeHandle,
) -> Result<TokenSid, TokenError> {
    let mut needed = 0;
    let _ = api.get_token_information(token, abi::TOKEN_GROUPS, None, &mut needed);
    if needed == 0 {
        return Err(last_error(api, "GetTokenInformation", "TokenGroups size query").into());
    }
    if needed < 8 {
        return Err(returned_error(
            api,
            "GetTokenInformation",
            api.last_error(),
            format!("implausible TokenGroups size {needed}"),
        )
        .into());
    }
    let mut groups = vec![0_u8; needed as usize];
    if !api.get_token_information(token, abi::TOKEN_GROUPS, Some(&mut groups), &mut needed) {
        return Err(last_error(api, "GetTokenInformation", "TokenGroups").into());
    }
    let group_count = read_u32(&groups, 0).unwrap_or(0);
    for index in 0..group_count {
        let offset = abi::TOKEN_GROUPS_OFFSET + index as usize * abi::SID_AND_ATTRIBUTES_SIZE;
        let sid = read_pointer(&groups, offset);
        let attributes = read_u32(&groups, offset + 8).unwrap_or(0);
        if sid.is_none() || attributes & abi::SE_GROUP_LOGON_ID != abi::SE_GROUP_LOGON_ID {
            continue;
        }
        let Some(sid) = sid else {
            continue;
        };
        let length = api.get_length_sid(sid);
        if length == 0 {
            return Err(last_error(api, "GetLengthSid", format!("logon SID group {index}")).into());
        }
        let mut copy = TokenSid::zeroed(length as usize);
        if !api.copy_sid(copy.bytes_mut(), sid) {
            return Err(last_error(api, "CopySid", format!("logon SID group {index}")).into());
        }
        return Ok(copy);
    }
    Err(TokenError::NoLogonSid(group_count))
}

/// Creates and validates one well-known SID in bounded caller-owned storage.
///
/// # Errors
///
/// Returns exact creation or validity failures.
pub fn make_well_known_sid(api: &dyn TokenBindings, kind: u32) -> Result<TokenSid, TokenError> {
    let mut sid = TokenSid::zeroed(abi::SECURITY_MAX_SID_SIZE);
    let mut size = 68_u32;
    if !api.create_well_known_sid(kind, sid.bytes_mut(), &mut size) {
        return Err(last_error(api, "CreateWellKnownSid", format!("type {kind}")).into());
    }
    if !api.is_valid_sid(sid.bytes()) {
        return Err(
            last_error(api, "IsValidSid", format!("CreateWellKnownSid type {kind}")).into(),
        );
    }
    Ok(sid)
}

/// Merges a restricting-SID full-access ACE into the token's default DACL.
///
/// # Errors
///
/// Returns exact information, merge, apply, or null-DACL failures.
pub fn set_token_default_dacl_grant(
    api: &dyn TokenBindings,
    token: NativeHandle,
    sid: NativePointer,
) -> Result<(), TokenError> {
    let mut needed = 0;
    let _ = api.get_token_information(token, abi::TOKEN_DEFAULT_DACL, None, &mut needed);
    if needed == 0 {
        return Err(last_error(api, "GetTokenInformation", "TokenDefaultDacl size query").into());
    }
    let mut buffer = vec![0_u8; needed as usize];
    if !api.get_token_information(
        token,
        abi::TOKEN_DEFAULT_DACL,
        Some(&mut buffer),
        &mut needed,
    ) {
        return Err(last_error(api, "GetTokenInformation", "TokenDefaultDacl").into());
    }
    let current = read_pointer(&buffer, 0).ok_or(TokenError::NoDefaultDacl)?;
    let merged = api.set_entries_in_acl(
        &build_explicit_access(sid, abi::GRANT_ACCESS, abi::FILE_ALL_ACCESS),
        current,
    );
    if merged.code != abi::ERROR_SUCCESS {
        return Err(
            returned_error(api, "SetEntriesInAclW", merged.code, "default DACL merge").into(),
        );
    }
    let Some(new_dacl) = merged.acl else {
        return Err(returned_error(
            api,
            "SetEntriesInAclW",
            merged.code,
            "null merged default DACL",
        )
        .into());
    };
    let info = new_dacl.raw().to_le_bytes();
    if !api.set_token_information(token, abi::TOKEN_DEFAULT_DACL, &info) {
        let code = api.last_error();
        let _ = api.local_free(new_dacl);
        return Err(returned_error(api, "SetTokenInformation", code, "TokenDefaultDacl").into());
    }
    let _ = api.local_free(new_dacl);
    Ok(())
}

/// The well-known SID shared by both restricting-list modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestrictingSidSet {
    /// Everyone (`S-1-1-0`).
    pub world: TokenSid,
}

/// Creates the write-restricted primary token with the exact mode allowlist.
///
/// # Errors
///
/// Rejects workspace-write without a capability and returns exact native
/// creation or successful-null-handle failures.
pub fn create_restricted_token(
    api: &dyn TokenBindings,
    current_token: NativeHandle,
    logon_sid: &TokenSid,
    write_sids: &[NativePointer],
    known: &RestrictingSidSet,
    mode: AclSandboxMode,
) -> Result<NativeHandle, TokenError> {
    let mut restricting = vec![logon_sid.pointer(), known.world.pointer()];
    if mode == AclSandboxMode::WorkspaceWrite {
        if write_sids.is_empty() {
            return Err(TokenError::MissingWriteSid);
        }
        restricting.extend_from_slice(write_sids);
    }
    let flags = abi::DISABLE_MAX_PRIVILEGE | abi::LUA_TOKEN | abi::WRITE_RESTRICTED;
    let (created, token) = api.create_restricted_token(current_token, flags, &restricting);
    if !created {
        return Err(last_error(
            api,
            "CreateRestrictedToken",
            format!("restricting SIDs: {}", restricting.len()),
        )
        .into());
    }
    token.ok_or_else(|| {
        returned_error(
            api,
            "CreateRestrictedToken",
            api.last_error(),
            "null token handle",
        )
        .into()
    })
}
