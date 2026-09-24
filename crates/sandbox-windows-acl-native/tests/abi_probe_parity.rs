//! Rust equivalent of the source package's MinGW `abi-probe.cpp` contract.

#![cfg(windows)]

use std::mem::{offset_of, size_of};

use seekdeep_sandbox_windows_acl::abi;
use windows_sys::{
    Win32::{
        Foundation::{
            ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER,
            ERROR_INVALID_SID, ERROR_LOCK_VIOLATION, ERROR_NO_MORE_ITEMS, ERROR_NONE_MAPPED,
            ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, MAX_PATH,
        },
        Security::{
            Authorization::{
                EXPLICIT_ACCESS_W, GRANT_ACCESS, NOT_USED_ACCESS, REVOKE_ACCESS, SE_FILE_OBJECT,
                TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
            },
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, INHERITED_ACE,
            LUA_TOKEN, OBJECT_INHERIT_ACE, SANDBOX_INERT, SECURITY_ATTRIBUTES, SID,
            SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_ADJUST_DEFAULT,
            TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_MANDATORY_LABEL,
            TOKEN_QUERY, TokenGroups, TokenIntegrityLevel, TokenUser, WRITE_RESTRICTED,
            WinConsoleLogonSid, WinLocalLogonSid, WinWorldSid,
        },
        Storage::FileSystem::{
            DELETE, FILE_DELETE_CHILD, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, OPEN_ALWAYS,
            STANDARD_RIGHTS_WRITE,
        },
        System::{
            Diagnostics::Debug::{
                FORMAT_MESSAGE_ALLOCATE_BUFFER, FORMAT_MESSAGE_FROM_SYSTEM,
                FORMAT_MESSAGE_IGNORE_INSERTS,
            },
            JobObjects::{
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_LIMIT_INFORMATION,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            },
            Memory::{LMEM_FIXED, LMEM_ZEROINIT, LPTR},
            SystemServices::{SE_GROUP_INTEGRITY, SE_GROUP_INTEGRITY_ENABLED, SE_GROUP_LOGON_ID},
            Threading::{
                CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_SUSPENDED, DETACHED_PROCESS, INFINITE,
                IO_COUNTERS, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
            },
        },
    },
    core::BOOL,
};

#[test]
fn sdk_layouts_match_the_pinned_x64_probe() {
    assert_eq!(size_of::<*const ()>(), 8);
    assert_eq!(size_of::<HANDLE>(), 8);
    assert_eq!(size_of::<u32>(), 4);
    assert_eq!(size_of::<u16>(), 2);
    assert_eq!(size_of::<BOOL>(), 4);

    assert_eq!(size_of::<STARTUPINFOW>(), 104);
    assert_eq!(offset_of!(STARTUPINFOW, cb), 0);
    assert_eq!(offset_of!(STARTUPINFOW, lpReserved), 8);
    assert_eq!(offset_of!(STARTUPINFOW, lpDesktop), 16);
    assert_eq!(offset_of!(STARTUPINFOW, lpTitle), 24);
    assert_eq!(offset_of!(STARTUPINFOW, dwX), 32);
    assert_eq!(offset_of!(STARTUPINFOW, dwY), 36);
    assert_eq!(offset_of!(STARTUPINFOW, dwXSize), 40);
    assert_eq!(offset_of!(STARTUPINFOW, dwYSize), 44);
    assert_eq!(offset_of!(STARTUPINFOW, dwXCountChars), 48);
    assert_eq!(offset_of!(STARTUPINFOW, dwYCountChars), 52);
    assert_eq!(offset_of!(STARTUPINFOW, dwFillAttribute), 56);
    assert_eq!(offset_of!(STARTUPINFOW, dwFlags), 60);
    assert_eq!(offset_of!(STARTUPINFOW, wShowWindow), 64);
    assert_eq!(offset_of!(STARTUPINFOW, cbReserved2), 66);
    assert_eq!(offset_of!(STARTUPINFOW, lpReserved2), 72);
    assert_eq!(offset_of!(STARTUPINFOW, hStdInput), 80);
    assert_eq!(offset_of!(STARTUPINFOW, hStdOutput), 88);
    assert_eq!(offset_of!(STARTUPINFOW, hStdError), 96);

    assert_eq!(size_of::<PROCESS_INFORMATION>(), 24);
    assert_eq!(offset_of!(PROCESS_INFORMATION, hProcess), 0);
    assert_eq!(offset_of!(PROCESS_INFORMATION, hThread), 8);
    assert_eq!(offset_of!(PROCESS_INFORMATION, dwProcessId), 16);
    assert_eq!(offset_of!(PROCESS_INFORMATION, dwThreadId), 20);

    assert_eq!(size_of::<SECURITY_ATTRIBUTES>(), 24);
    assert_eq!(offset_of!(SECURITY_ATTRIBUTES, nLength), 0);
    assert_eq!(offset_of!(SECURITY_ATTRIBUTES, lpSecurityDescriptor), 8);
    assert_eq!(offset_of!(SECURITY_ATTRIBUTES, bInheritHandle), 16);

    assert_eq!(size_of::<TRUSTEE_W>(), 32);
    assert_eq!(offset_of!(TRUSTEE_W, pMultipleTrustee), 0);
    assert_eq!(offset_of!(TRUSTEE_W, MultipleTrusteeOperation), 8);
    assert_eq!(offset_of!(TRUSTEE_W, TrusteeForm), 12);
    assert_eq!(offset_of!(TRUSTEE_W, TrusteeType), 16);
    assert_eq!(offset_of!(TRUSTEE_W, ptstrName), 24);

    assert_eq!(size_of::<EXPLICIT_ACCESS_W>(), 48);
    assert_eq!(offset_of!(EXPLICIT_ACCESS_W, grfAccessPermissions), 0);
    assert_eq!(offset_of!(EXPLICIT_ACCESS_W, grfAccessMode), 4);
    assert_eq!(offset_of!(EXPLICIT_ACCESS_W, grfInheritance), 8);
    assert_eq!(offset_of!(EXPLICIT_ACCESS_W, Trustee), 16);

    assert_eq!(size_of::<SID_AND_ATTRIBUTES>(), 16);
    assert_eq!(offset_of!(SID_AND_ATTRIBUTES, Sid), 0);
    assert_eq!(offset_of!(SID_AND_ATTRIBUTES, Attributes), 8);
    assert_eq!(size_of::<TOKEN_GROUPS>(), 24);
    assert_eq!(offset_of!(TOKEN_GROUPS, GroupCount), 0);
    assert_eq!(offset_of!(TOKEN_GROUPS, Groups), 8);
    assert_eq!(size_of::<TOKEN_MANDATORY_LABEL>(), 16);
    assert_eq!(size_of::<SID>(), 12);

    assert_eq!(size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>(), 64);
    assert_eq!(size_of::<IO_COUNTERS>(), 48);
    assert_eq!(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(), 144);
    assert_eq!(
        offset_of!(JOBOBJECT_EXTENDED_LIMIT_INFORMATION, BasicLimitInformation),
        0
    );
    assert_eq!(
        offset_of!(JOBOBJECT_EXTENDED_LIMIT_INFORMATION, BasicLimitInformation)
            + offset_of!(JOBOBJECT_BASIC_LIMIT_INFORMATION, LimitFlags),
        16
    );
    assert_eq!(
        offset_of!(JOBOBJECT_EXTENDED_LIMIT_INFORMATION, ProcessMemoryLimit),
        112
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn sdk_constants_match_the_safe_contract_table() {
    assert_eq!(abi::TOKEN_ASSIGN_PRIMARY, TOKEN_ASSIGN_PRIMARY);
    assert_eq!(abi::TOKEN_DUPLICATE, TOKEN_DUPLICATE);
    assert_eq!(abi::TOKEN_QUERY, TOKEN_QUERY);
    assert_eq!(abi::TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_DEFAULT);
    assert_eq!(SE_GROUP_LOGON_ID.cast_unsigned(), 0xC000_0000);
    assert_eq!(SE_GROUP_INTEGRITY, 0x20);
    assert_eq!(SE_GROUP_INTEGRITY_ENABLED, 0x40);

    assert_eq!(abi::FILE_GENERIC_WRITE, FILE_GENERIC_WRITE);
    assert_eq!(abi::STANDARD_RIGHTS_WRITE, STANDARD_RIGHTS_WRITE);
    assert_eq!(abi::DELETE, DELETE);
    assert_eq!(abi::FILE_DELETE_CHILD, FILE_DELETE_CHILD);
    assert_eq!(abi::GRANT_MASK, 0x0011_0156);
    assert_eq!(FILE_GENERIC_WRITE & !STANDARD_RIGHTS_WRITE, 0x0010_0116);
    assert_eq!(abi::FILE_SHARE_READ, FILE_SHARE_READ);
    assert_eq!(abi::FILE_SHARE_WRITE, FILE_SHARE_WRITE);
    assert_eq!(abi::FILE_SHARE_DELETE, FILE_SHARE_DELETE);
    assert_eq!(abi::GENERIC_READ, GENERIC_READ);
    assert_eq!(abi::GENERIC_WRITE, GENERIC_WRITE);
    assert_eq!(abi::OPEN_ALWAYS, OPEN_ALWAYS);
    assert_eq!(abi::LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_EXCLUSIVE_LOCK);
    assert_eq!(abi::LOCKFILE_FAIL_IMMEDIATELY, LOCKFILE_FAIL_IMMEDIATELY);
    assert_eq!(abi::ERROR_LOCK_VIOLATION, ERROR_LOCK_VIOLATION);
    assert_eq!(u32::from(abi::INHERITED_ACE), INHERITED_ACE);

    assert_eq!(abi::DISABLE_MAX_PRIVILEGE, DISABLE_MAX_PRIVILEGE);
    assert_eq!(SANDBOX_INERT, 0x2);
    assert_eq!(abi::LUA_TOKEN, LUA_TOKEN);
    assert_eq!(abi::WRITE_RESTRICTED, WRITE_RESTRICTED);
    assert_eq!(abi::WIN_WORLD_SID, WinWorldSid.cast_unsigned());
    assert_eq!(WinLocalLogonSid, 80);
    assert_eq!(WinConsoleLogonSid, 81);
    assert_eq!(TokenUser, 1);
    assert_eq!(TokenGroups, 2);
    assert_eq!(TokenIntegrityLevel, 25);
    assert_eq!(abi::SE_FILE_OBJECT, SE_FILE_OBJECT.cast_unsigned());
    assert_eq!(abi::DACL_SECURITY_INFORMATION, DACL_SECURITY_INFORMATION);
    assert_eq!(abi::TRUSTEE_IS_UNKNOWN, TRUSTEE_IS_UNKNOWN.cast_unsigned());
    assert_eq!(abi::TRUSTEE_IS_SID, TRUSTEE_IS_SID.cast_unsigned());
    assert_eq!(NOT_USED_ACCESS, 0);
    assert_eq!(abi::GRANT_ACCESS, GRANT_ACCESS.cast_unsigned());
    assert_eq!(abi::REVOKE_ACCESS, REVOKE_ACCESS.cast_unsigned());
    assert_eq!(
        abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT
    );
    assert_eq!(OBJECT_INHERIT_ACE, 0x1);
    assert_eq!(CONTAINER_INHERIT_ACE, 0x2);

    assert_eq!(abi::CREATE_SUSPENDED, CREATE_SUSPENDED);
    assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    assert_eq!(DETACHED_PROCESS, 0x8);
    assert_eq!(CREATE_NEW_CONSOLE, 0x10);
    assert_eq!(abi::STARTF_USESTDHANDLES, STARTF_USESTDHANDLES);
    assert_eq!(abi::HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
    assert_eq!(abi::INFINITE, INFINITE);

    assert_eq!(LMEM_FIXED, 0);
    assert_eq!(LMEM_ZEROINIT, 0x40);
    assert_eq!(LPTR, 0x40);
    assert_eq!(FORMAT_MESSAGE_ALLOCATE_BUFFER, 0x100);
    assert_eq!(abi::FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_FROM_SYSTEM);
    assert_eq!(
        abi::FORMAT_MESSAGE_IGNORE_INSERTS,
        FORMAT_MESSAGE_IGNORE_INSERTS
    );
    assert_eq!(abi::MAX_PATH, MAX_PATH);
    assert_eq!(abi::ERROR_SUCCESS, ERROR_SUCCESS);
    assert_eq!(abi::ERROR_INSUFFICIENT_BUFFER, ERROR_INSUFFICIENT_BUFFER);
    assert_eq!(ERROR_NO_MORE_ITEMS, 259);
    assert_eq!(ERROR_INVALID_PARAMETER, 87);
    assert_eq!(ERROR_INVALID_SID, 1337);
    assert_eq!(ERROR_NONE_MAPPED, 1332);
    assert_eq!(abi::ERROR_BROKEN_PIPE, ERROR_BROKEN_PIPE);

    assert_eq!(
        abi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    );
    assert_eq!(
        abi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation.cast_unsigned()
    );
}

#[test]
fn safe_table_sizes_match_the_sdk_layouts() {
    assert_eq!(abi::STARTUPINFOW_SIZE as usize, size_of::<STARTUPINFOW>());
    assert_eq!(
        abi::PROCESS_INFORMATION_SIZE,
        size_of::<PROCESS_INFORMATION>()
    );
    assert_eq!(abi::EXPLICIT_ACCESS_W_SIZE, size_of::<EXPLICIT_ACCESS_W>());
    assert_eq!(
        abi::SID_AND_ATTRIBUTES_SIZE,
        size_of::<SID_AND_ATTRIBUTES>()
    );
    assert_eq!(abi::TOKEN_GROUPS_OFFSET, offset_of!(TOKEN_GROUPS, Groups));
    assert_eq!(abi::SECURITY_MAX_SID_SIZE, 68);
    assert_eq!(abi::SID_MAX_SUB_AUTHORITIES, 15);
    assert_eq!(
        abi::JOBOBJECT_EXTENDED_LIMIT_SIZE,
        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
    );
    assert_eq!(
        abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET,
        offset_of!(JOBOBJECT_BASIC_LIMIT_INFORMATION, LimitFlags)
    );
}
