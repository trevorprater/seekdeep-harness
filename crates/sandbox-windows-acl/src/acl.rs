//! Fail-closed ACL read, merge, apply, and per-path serialization.

use std::{fs, path::Path};

use sha2::{Digest as _, Sha256};

use crate::{Win32Error, abi};

/// A Win32 kernel object handle crossing the native binding boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeHandle(u64);

impl NativeHandle {
    /// Creates a handle from its pointer-sized ABI value.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the pointer-sized ABI value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Tests both Win32 failure spellings used for handles.
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.0 == 0 || self.0 == u64::MAX
    }

    /// Whether the handle is the null pointer value.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// A non-owning or locally allocated native pointer crossing the binding boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativePointer(u64);

impl NativePointer {
    /// Creates a pointer from its pointer-sized ABI value.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the pointer-sized ABI value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The null pointer.
    pub const NULL: Self = Self(0);

    /// Whether the pointer is null.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// One current DACL view and the descriptor allocation that owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclWithPointer {
    /// Pointer consumed by `SetEntriesInAclW`.
    pub pointer: NativePointer,
    /// Bounded bytes copied for safe exact-ACE inspection.
    pub bytes: Vec<u8>,
}

/// Raw `GetNamedSecurityInfoW` outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclRead {
    /// HRESULT-style Win32 status.
    pub code: u32,
    /// Current explicit DACL, if any.
    pub acl: Option<AclWithPointer>,
    /// Security descriptor allocation owning the current DACL.
    pub descriptor: Option<NativePointer>,
}

/// Raw `SetEntriesInAclW` outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetEntriesResult {
    /// HRESULT-style Win32 status.
    pub code: u32,
    /// Newly allocated merged ACL.
    pub acl: Option<NativePointer>,
}

/// Safe seam implemented by the narrowly scoped native Windows crate.
pub trait AclBindings: Send + Sync {
    /// Returns the calling thread's last Win32 error.
    fn last_error(&self) -> u32;
    /// Formats a Win32 error, or returns an empty string.
    fn format_message(&self, code: u32) -> String;
    /// Mirrors one bounded `GetTempPathW` call.
    fn get_temp_path(&self, capacity: u32, buffer: &mut [u16]) -> u32;
    /// Opens or creates a shared lock file.
    fn create_lock_file(&self, path: &Path) -> NativeHandle;
    /// Takes the byte-zero exclusive lock.
    fn lock_file(&self, handle: NativeHandle, flags: u32) -> bool;
    /// Releases the byte-zero lock.
    fn unlock_file(&self, handle: NativeHandle) -> bool;
    /// Closes a kernel object handle.
    fn close_handle(&self, handle: NativeHandle) -> bool;
    /// Reads the current DACL and its owning descriptor.
    fn read_current_dacl(&self, path: &Path) -> AclRead;
    /// Merges one packed `EXPLICIT_ACCESS_W` into the current DACL.
    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        old_acl: Option<NativePointer>,
    ) -> SetEntriesResult;
    /// Applies a merged DACL to one filesystem object.
    fn set_named_security_info(&self, path: &Path, acl: NativePointer) -> u32;
    /// Releases memory returned by LocalAlloc-family APIs; null means success.
    fn local_free(&self, pointer: NativePointer) -> NativePointer;
}

fn last_error(api: &dyn AclBindings, name: &'static str, detail: impl Into<String>) -> Win32Error {
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
    api: &dyn AclBindings,
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

/// Packs the verified x64 `EXPLICIT_ACCESS_W` layout.
#[must_use]
pub fn build_explicit_access(
    sid: NativePointer,
    mode: u32,
    permissions: u32,
) -> [u8; abi::EXPLICIT_ACCESS_W_SIZE] {
    let mut entry = [0_u8; abi::EXPLICIT_ACCESS_W_SIZE];
    entry[0..4].copy_from_slice(&permissions.to_le_bytes());
    entry[4..8].copy_from_slice(&mode.to_le_bytes());
    entry[8..12].copy_from_slice(&abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT.to_le_bytes());
    entry[24..28].copy_from_slice(&abi::NO_MULTIPLE_TRUSTEE.to_le_bytes());
    entry[28..32].copy_from_slice(&abi::TRUSTEE_IS_SID.to_le_bytes());
    entry[32..36].copy_from_slice(&abi::TRUSTEE_IS_UNKNOWN.to_le_bytes());
    entry[40..48].copy_from_slice(&sid.raw().to_le_bytes());
    entry
}

fn temp_path(api: &dyn AclBindings) -> Result<String, Win32Error> {
    let capacity = abi::MAX_PATH + 1;
    let mut buffer = vec![0_u16; capacity as usize];
    let length = api.get_temp_path(capacity, &mut buffer);
    if length == 0 {
        return Err(last_error(api, "GetTempPathW", ""));
    }
    if length > capacity {
        return Err(Win32Error::new(
            "GetTempPathW",
            abi::ERROR_INSUFFICIENT_BUFFER,
            Some(format!(
                "required {length} chars exceed the {capacity}-char buffer; nothing was written"
            )),
        ));
    }
    Ok(String::from_utf16_lossy(&buffer[..length as usize]))
}

/// Computes the case-insensitive, deterministic lock file name.
///
/// # Errors
///
/// Returns the exact `GetTempPathW` failure or bounded-buffer diagnostic.
pub fn lock_file_path(
    api: &dyn AclBindings,
    path: &Path,
) -> Result<std::path::PathBuf, Win32Error> {
    let spelling = path.to_string_lossy().to_lowercase();
    let digest = hex::encode(Sha256::digest(spelling.as_bytes()));
    Ok(std::path::PathBuf::from(temp_path(api)?)
        .join("seekdeep-acl-locks")
        .join(format!("{}.lock", &digest[..16])))
}

/// Runs an action while holding the source-compatible exclusive path lock.
///
/// # Errors
///
/// Returns lock setup/release failures or the action's original error. An
/// action error is never masked by best-effort lock cleanup.
pub fn with_path_lock<T>(
    api: &dyn AclBindings,
    path: &Path,
    action: impl FnOnce() -> Result<T, Win32Error>,
) -> Result<T, Win32Error> {
    let lock_path = lock_file_path(api, path)?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Win32Error::new(
                "CreateFileW",
                0,
                Some(format!("{}: {error}", lock_path.display())),
            )
        })?;
    }
    let handle = api.create_lock_file(&lock_path);
    if handle.is_invalid() {
        return Err(last_error(
            api,
            "CreateFileW",
            lock_path.display().to_string(),
        ));
    }
    if !api.lock_file(handle, abi::LOCKFILE_EXCLUSIVE_LOCK) {
        let code = api.last_error();
        let _ = api.close_handle(handle);
        return Err(returned_error(
            api,
            "LockFileEx",
            code,
            lock_path.display().to_string(),
        ));
    }
    let value = match action() {
        Ok(value) => value,
        Err(error) => {
            let _ = api.unlock_file(handle);
            let _ = api.close_handle(handle);
            return Err(error);
        }
    };
    if !api.unlock_file(handle) {
        let code = api.last_error();
        let _ = api.close_handle(handle);
        return Err(returned_error(
            api,
            "UnlockFileEx",
            code,
            lock_path.display().to_string(),
        ));
    }
    if !api.close_handle(handle) {
        return Err(last_error(
            api,
            "CloseHandle",
            format!("lock file {}", lock_path.display()),
        ));
    }
    Ok(value)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn same_sid(left: &[u8], right: &[u8]) -> bool {
    let Some((&left_revision, left_tail)) = left.split_first() else {
        return false;
    };
    let Some((&right_revision, right_tail)) = right.split_first() else {
        return false;
    };
    let Some((&left_count, _)) = left_tail.split_first() else {
        return false;
    };
    let Some((&right_count, _)) = right_tail.split_first() else {
        return false;
    };
    if left_revision != right_revision
        || left_count != right_count
        || left_count > abi::SID_MAX_SUB_AUTHORITIES
    {
        return false;
    }
    let length = 8 + usize::from(left_count) * 4;
    left.get(..length) == right.get(..length)
}

fn has_exact_grant(acl: &[u8], sid: &[u8]) -> bool {
    let Some(acl_size) = read_u16(acl, 2).map(usize::from) else {
        return false;
    };
    let Some(ace_count) = read_u16(acl, 4).map(usize::from) else {
        return false;
    };
    if !(8..=1_048_576).contains(&acl_size) || acl_size > acl.len() {
        return false;
    }
    let mut offset = 8;
    for _ in 0..ace_count {
        let Some(entry_size) = read_u16(acl, offset + 2).map(usize::from) else {
            return false;
        };
        let Some(end) = offset.checked_add(entry_size) else {
            return false;
        };
        if entry_size < 8 || end > acl_size {
            return false;
        }
        let exact = acl.get(offset) == Some(&abi::ACCESS_ALLOWED_ACE_TYPE)
            && acl.get(offset + 1)
                == Some(&abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT.to_le_bytes()[0])
            && read_u32(acl, offset + 4) == Some(abi::GRANT_MASK);
        if exact && same_sid(&acl[offset + 8..end], sid) {
            return true;
        }
        offset = end;
    }
    false
}

fn free_checked(
    api: &dyn AclBindings,
    pointer: NativePointer,
    detail: String,
) -> Result<(), Win32Error> {
    if api.local_free(pointer).is_null() {
        Ok(())
    } else {
        Err(last_error(api, "LocalFree", detail))
    }
}

fn merge_and_apply(
    api: &dyn AclBindings,
    path: &Path,
    entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
    old_acl: Option<&AclWithPointer>,
    descriptor: Option<NativePointer>,
    label: &str,
) -> Result<(), Win32Error> {
    let merged = api.set_entries_in_acl(entry, old_acl.map(|acl| acl.pointer));
    if merged.code != abi::ERROR_SUCCESS {
        if let Some(descriptor) = descriptor {
            let _ = api.local_free(descriptor);
        }
        return Err(returned_error(
            api,
            "SetEntriesInAclW",
            merged.code,
            format!("{label}({})", path.display()),
        ));
    }
    let Some(new_acl) = merged.acl else {
        if let Some(descriptor) = descriptor {
            let _ = api.local_free(descriptor);
        }
        return Err(returned_error(
            api,
            "SetEntriesInAclW",
            api.last_error(),
            format!("{label}({}): null new ACL", path.display()),
        ));
    };
    let freed_descriptor = descriptor.map(|pointer| api.local_free(pointer));
    let apply_result = api.set_named_security_info(path, new_acl);
    let freed_new = api.local_free(new_acl);
    if apply_result != abi::ERROR_SUCCESS {
        return Err(returned_error(
            api,
            "SetNamedSecurityInfoW",
            apply_result,
            format!("{label}({})", path.display()),
        ));
    }
    if freed_descriptor.is_some_and(|pointer| !pointer.is_null()) {
        return Err(last_error(
            api,
            "LocalFree",
            format!("{label}({}) descriptor", path.display()),
        ));
    }
    if !freed_new.is_null() {
        return Err(last_error(
            api,
            "LocalFree",
            format!("{label}({}) new ACL", path.display()),
        ));
    }
    Ok(())
}

/// Grants the exact inheritable write capability, skipping a standing exact ACE.
///
/// # Errors
///
/// Returns the first checked lock, DACL read, merge, apply, or cleanup failure.
pub fn grant_write(
    api: &dyn AclBindings,
    path: &Path,
    sid_pointer: NativePointer,
    sid_bytes: &[u8],
) -> Result<(), Win32Error> {
    with_path_lock(api, path, || {
        let read = api.read_current_dacl(path);
        if read.code != abi::ERROR_SUCCESS {
            return Err(returned_error(
                api,
                "GetNamedSecurityInfoW",
                read.code,
                path.display().to_string(),
            ));
        }
        if read
            .acl
            .as_ref()
            .is_some_and(|acl| has_exact_grant(&acl.bytes, sid_bytes))
        {
            if let Some(descriptor) = read.descriptor {
                free_checked(
                    api,
                    descriptor,
                    format!("grantWrite({}) descriptor", path.display()),
                )?;
            }
            return Ok(());
        }
        merge_and_apply(
            api,
            path,
            &build_explicit_access(sid_pointer, abi::GRANT_ACCESS, abi::GRANT_MASK),
            read.acl.as_ref(),
            read.descriptor,
            "grantWrite",
        )
    })
}

/// Revokes every ACE for the capability SID and preserves unrelated entries.
///
/// # Errors
///
/// Returns the first checked lock, DACL read, merge, apply, or cleanup failure.
pub fn revoke_write(
    api: &dyn AclBindings,
    path: &Path,
    sid_pointer: NativePointer,
) -> Result<bool, Win32Error> {
    with_path_lock(api, path, || {
        let read = api.read_current_dacl(path);
        if read.code != abi::ERROR_SUCCESS {
            return Err(returned_error(
                api,
                "GetNamedSecurityInfoW",
                read.code,
                path.display().to_string(),
            ));
        }
        if read.acl.is_none() {
            if let Some(descriptor) = read.descriptor {
                free_checked(
                    api,
                    descriptor,
                    format!("revokeWrite({}) descriptor", path.display()),
                )?;
            }
            return Ok(false);
        }
        merge_and_apply(
            api,
            path,
            &build_explicit_access(sid_pointer, abi::REVOKE_ACCESS, 0),
            read.acl.as_ref(),
            read.descriptor,
            "revokeWrite",
        )?;
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_grant_walk_is_bounded_and_sid_aware() {
        let sid = [1, 1, 0, 0, 0, 0, 0, 5, 7, 0, 0, 0];
        let mut acl = vec![0_u8; 28];
        acl[2..4].copy_from_slice(&28_u16.to_le_bytes());
        acl[4..6].copy_from_slice(&1_u16.to_le_bytes());
        acl[8] = abi::ACCESS_ALLOWED_ACE_TYPE;
        acl[9] = abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT.to_le_bytes()[0];
        acl[10..12].copy_from_slice(&20_u16.to_le_bytes());
        acl[12..16].copy_from_slice(&abi::GRANT_MASK.to_le_bytes());
        acl[16..28].copy_from_slice(&sid);
        assert!(has_exact_grant(&acl, &sid));
        acl[2..4].copy_from_slice(&4_u16.to_le_bytes());
        assert!(!has_exact_grant(&acl, &sid));
    }
}
