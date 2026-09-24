//! Audited Windows ABI constants used by the restricted-token design.
//!
//! The public names intentionally match the upstream Win32 ABI catalog, so
//! repeating every SDK definition as prose would obscure the executable table.
#![allow(missing_docs)]

pub const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
pub const TOKEN_DUPLICATE: u32 = 0x0002;
pub const TOKEN_QUERY: u32 = 0x0008;
pub const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
pub const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
pub const STANDARD_RIGHTS_WRITE: u32 = 0x0002_0000;
pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
pub const DELETE: u32 = 0x0001_0000;
pub const FILE_DELETE_CHILD: u32 = 0x0040;
pub const GRANT_MASK: u32 =
    (FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD) & !STANDARD_RIGHTS_WRITE;
pub const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
pub const DISABLE_MAX_PRIVILEGE: u32 = 0x1;
pub const LUA_TOKEN: u32 = 0x4;
pub const WRITE_RESTRICTED: u32 = 0x8;
pub const WIN_WORLD_SID: u32 = 1;
pub const TOKEN_GROUPS: u32 = 2;
pub const TOKEN_DEFAULT_DACL: u32 = 6;
pub const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
pub const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
pub const SE_FILE_OBJECT: u32 = 1;
pub const TRUSTEE_IS_UNKNOWN: u32 = 0;
pub const TRUSTEE_IS_SID: u32 = 0;
pub const NO_MULTIPLE_TRUSTEE: u32 = 0;
pub const GRANT_ACCESS: u32 = 1;
pub const REVOKE_ACCESS: u32 = 4;
pub const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
pub const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
pub const HANDLE_FLAG_INHERIT: u32 = 0x1;
pub const INFINITE: u32 = 0xFFFF_FFFF;
pub const MAX_PATH: u32 = 260;
pub const CREATE_SUSPENDED: u32 = 0x4;
pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub const ERROR_BROKEN_PIPE: u32 = 109;
pub const ERROR_NO_DATA: u32 = 232;
pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const FILE_SHARE_READ: u32 = 0x1;
pub const FILE_SHARE_WRITE: u32 = 0x2;
pub const OPEN_ALWAYS: u32 = 4;
pub const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
pub const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
pub const SID_MAX_SUB_AUTHORITIES: u8 = 15;
pub const INHERITED_ACE: u8 = 0x10;
pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
pub const JOBOBJECT_EXTENDED_LIMIT_SIZE: usize = 144;
pub const JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET: usize = 16;
pub const SECURITY_MAX_SID_SIZE: usize = 68;
pub const SID_AND_ATTRIBUTES_SIZE: usize = 16;
pub const TOKEN_GROUPS_OFFSET: usize = 8;
pub const EXPLICIT_ACCESS_W_SIZE: usize = 48;
pub const STARTUPINFOW_SIZE: u32 = 104;
pub const PROCESS_INFORMATION_SIZE: usize = 24;
pub const STD_INPUT_HANDLE: i32 = -10;
pub const STD_OUTPUT_HANDLE: i32 = -11;
pub const STD_ERROR_HANDLE: i32 = -12;
pub const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x0000_1000;
pub const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x0000_0200;
pub const ERROR_LOCK_VIOLATION: u32 = 33;
pub const FILE_SHARE_DELETE: u32 = 0x4;
pub const TRUSTEE_W_OFFSET: usize = 16;
pub const TRUSTEE_W_PTSTRNAME_OFFSET: usize = 24;
