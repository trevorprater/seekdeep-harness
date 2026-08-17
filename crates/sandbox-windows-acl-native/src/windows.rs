//! Windows implementation; all raw ABI calls remain in this module.

use std::{
    ffi::c_void,
    mem::{MaybeUninit, size_of},
    os::windows::ffi::OsStrExt as _,
    path::Path,
    ptr::{null, null_mut},
};

use seekdeep_sandbox_windows_acl::{
    AclBindings, AclRead, AclWithPointer, GrantBindings, NativeHandle, NativePointer, ParsedSid,
    PeekResult, ProcessInfo, SetEntriesResult, SpawnBindings, StartupHandles, TokenBindings,
    TokenSid, Win32Error, WindowsAclBindings, abi,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree, SetHandleInformation},
    Security::{
        ACL,
        Authorization::{
            ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT,
            SetEntriesInAclW, SetNamedSecurityInfoW,
        },
        CopySid, CreateRestrictedToken, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
        IsValidSid, PSID, SID_AND_ATTRIBUTES, SetTokenInformation,
    },
    Storage::FileSystem::{CreateFileW, GetTempPathW, LockFileEx, ReadFile, UnlockFileEx},
    System::{
        Console::{GetStdHandle, SetConsoleCtrlHandler},
        Diagnostics::Debug::FormatMessageW,
        Environment::SetEnvironmentVariableW,
        IO::OVERLAPPED,
        JobObjects::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
        Pipes::{CreatePipe, PeekNamedPipe},
        Threading::{
            CreateProcessAsUserW, GetExitCodeProcess, OpenProcess, OpenProcessToken,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
        },
    },
};

/// Concrete stateless Win32 binding table.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsBindings;

impl WindowsBindings {
    /// Makes the runner ignore its own Ctrl+C while the same-console child
    /// continues to receive and handle the signal.
    pub fn ignore_ctrl_c(self) -> bool {
        // SAFETY: a null handler with `add=1` is the documented ignore form.
        unsafe { SetConsoleCtrlHandler(None, 1) != 0 }
    }

    /// Rewrites one inherited environment entry before child creation.
    pub fn set_environment_variable(self, name: &str, value: &Path) -> bool {
        let name = wide(name);
        let value = wide(value.as_os_str());
        // SAFETY: both values are live, NUL-terminated UTF-16 strings.
        unsafe { SetEnvironmentVariableW(name.as_ptr(), value.as_ptr()) != 0 }
    }

    /// Terminates this runner with an untruncated Windows `u32` exit status.
    pub fn exit_process(self, code: u32) -> ! {
        // SAFETY: `ExitProcess` accepts every u32 status and never returns.
        unsafe { windows_sys::Win32::System::Threading::ExitProcess(code) }
    }
}

fn raw_handle(handle: NativeHandle) -> HANDLE {
    usize::try_from(handle.raw()).expect("Windows handle fits target pointer width") as HANDLE
}

fn native_handle(handle: HANDLE) -> NativeHandle {
    NativeHandle::from_raw(handle as usize as u64)
}

fn raw_pointer(pointer: NativePointer) -> *mut c_void {
    usize::try_from(pointer.raw()).expect("Windows pointer fits target pointer width")
        as *mut c_void
}

fn native_pointer(pointer: *mut c_void) -> NativePointer {
    NativePointer::from_raw(pointer as usize as u64)
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn explicit_access(bytes: &[u8; abi::EXPLICIT_ACCESS_W_SIZE]) -> EXPLICIT_ACCESS_W {
    assert_eq!(size_of::<EXPLICIT_ACCESS_W>(), abi::EXPLICIT_ACCESS_W_SIZE);
    let mut entry = MaybeUninit::<EXPLICIT_ACCESS_W>::zeroed();
    // SAFETY: the source and C++ ABI probe pin the struct to exactly 48 bytes;
    // the destination is aligned storage for that type and every bit pattern
    // in this pointer/integer-only C structure is valid.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), entry.as_mut_ptr().cast::<u8>(), bytes.len());
        entry.assume_init()
    }
}

fn last_win32(name: &'static str, detail: impl Into<String>) -> Win32Error {
    // SAFETY: `GetLastError` takes no pointers and reads thread-local state.
    let code = unsafe { GetLastError() };
    Win32Error::new(name, code, Some(detail.into()))
}

impl AclBindings for WindowsBindings {
    fn last_error(&self) -> u32 {
        // SAFETY: no arguments or memory preconditions.
        unsafe { GetLastError() }
    }

    fn format_message(&self, code: u32) -> String {
        let mut buffer = [0_u16; 512];
        // SAFETY: the mutable buffer is valid for its advertised length and
        // the source/argument pointers are null under these flags.
        let length = unsafe {
            FormatMessageW(
                abi::FORMAT_MESSAGE_FROM_SYSTEM | abi::FORMAT_MESSAGE_IGNORE_INSERTS,
                null(),
                code,
                0,
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).expect("fixed buffer fits u32"),
                null(),
            )
        };
        String::from_utf16_lossy(&buffer[..length as usize])
            .trim()
            .to_owned()
    }

    fn get_temp_path(&self, capacity: u32, buffer: &mut [u16]) -> u32 {
        // SAFETY: `buffer` is writable and the caller passes its exact bound.
        unsafe { GetTempPathW(capacity, buffer.as_mut_ptr()) }
    }

    fn create_lock_file(&self, path: &Path) -> NativeHandle {
        let path = wide(path.as_os_str());
        // SAFETY: the path is NUL-terminated; optional pointers are null.
        native_handle(unsafe {
            CreateFileW(
                path.as_ptr(),
                abi::GENERIC_READ | abi::GENERIC_WRITE,
                abi::FILE_SHARE_READ | abi::FILE_SHARE_WRITE,
                null(),
                abi::OPEN_ALWAYS,
                0,
                null_mut(),
            )
        })
    }

    fn lock_file(&self, handle: NativeHandle, flags: u32) -> bool {
        let mut overlapped = MaybeUninit::<OVERLAPPED>::zeroed();
        // SAFETY: the handle is caller-owned and the zeroed OVERLAPPED remains
        // live for the synchronous call, selecting byte range zero..one.
        unsafe { LockFileEx(raw_handle(handle), flags, 0, 1, 0, overlapped.as_mut_ptr()) != 0 }
    }

    fn unlock_file(&self, handle: NativeHandle) -> bool {
        let mut overlapped = MaybeUninit::<OVERLAPPED>::zeroed();
        // SAFETY: same synchronous zero-offset contract as `lock_file`.
        unsafe { UnlockFileEx(raw_handle(handle), 0, 1, 0, overlapped.as_mut_ptr()) != 0 }
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        // SAFETY: ownership and double-close prevention live in the safe state machine.
        unsafe { CloseHandle(raw_handle(handle)) != 0 }
    }

    fn read_current_dacl(&self, path: &Path) -> AclRead {
        let mut path = wide(path.as_os_str());
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: *mut c_void = null_mut();
        // SAFETY: all out pointers are valid; the path is mutable,
        // NUL-terminated UTF-16 as required by this legacy API signature.
        let code = unsafe {
            GetNamedSecurityInfoW(
                path.as_mut_ptr(),
                SE_FILE_OBJECT,
                abi::DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &raw mut dacl,
                null_mut(),
                &raw mut descriptor,
            )
        };
        let acl = if code == 0 && !dacl.is_null() {
            // SAFETY: a successful API result points at an ACL within the live
            // descriptor allocation; `AclSize` bounds the copied view.
            let bytes = unsafe {
                std::slice::from_raw_parts(dacl.cast::<u8>(), usize::from((*dacl).AclSize)).to_vec()
            };
            Some(AclWithPointer {
                pointer: native_pointer(dacl.cast()),
                bytes,
            })
        } else {
            None
        };
        AclRead {
            code,
            acl,
            descriptor: (!descriptor.is_null()).then(|| native_pointer(descriptor)),
        }
    }

    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        old_acl: Option<NativePointer>,
    ) -> SetEntriesResult {
        let entry = explicit_access(entry);
        let mut new_acl: *mut ACL = null_mut();
        // SAFETY: the aligned entry lives through the call; old ACL ownership
        // stays with its descriptor and the output slot is valid.
        let code = unsafe {
            SetEntriesInAclW(
                1,
                &raw const entry,
                old_acl.map_or(null(), |pointer| raw_pointer(pointer).cast::<ACL>()),
                &raw mut new_acl,
            )
        };
        SetEntriesResult {
            code,
            acl: (!new_acl.is_null()).then(|| native_pointer(new_acl.cast())),
        }
    }

    fn set_named_security_info(&self, path: &Path, acl: NativePointer) -> u32 {
        let mut path = wide(path.as_os_str());
        // SAFETY: path is NUL-terminated and the merged ACL remains live until
        // the safe caller frees it after this synchronous apply.
        unsafe {
            SetNamedSecurityInfoW(
                path.as_mut_ptr(),
                SE_FILE_OBJECT,
                abi::DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                raw_pointer(acl).cast::<ACL>(),
                null(),
            )
        }
    }

    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        // SAFETY: only pointers returned by LocalAlloc-family APIs enter this method.
        native_pointer(unsafe { LocalFree(raw_pointer(pointer)) })
    }
}

impl GrantBindings for WindowsBindings {
    fn convert_string_sid(&self, sid: &str) -> Result<ParsedSid, Win32Error> {
        let sid_text = wide(sid);
        let mut pointer: PSID = null_mut();
        // SAFETY: NUL-terminated input and valid out pointer.
        if unsafe { ConvertStringSidToSidW(sid_text.as_ptr(), &raw mut pointer) } == 0 {
            return Err(last_win32("ConvertStringSidToSidW", sid));
        }
        if pointer.is_null() {
            return Ok(ParsedSid {
                pointer: NativePointer::NULL,
                bytes: Vec::new(),
            });
        }
        // SAFETY: successful SID conversion returns a valid SID allocation.
        let length = unsafe { GetLengthSid(pointer) };
        let bytes = if length == 0 {
            Vec::new()
        } else {
            // SAFETY: `GetLengthSid` is the exact allocation-readable bound.
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize).to_vec() }
        };
        Ok(ParsedSid {
            pointer: native_pointer(pointer),
            bytes,
        })
    }
}

impl TokenBindings for WindowsBindings {
    fn last_error(&self) -> u32 {
        AclBindings::last_error(self)
    }

    fn format_message(&self, code: u32) -> String {
        AclBindings::format_message(self, code)
    }

    fn open_process(&self, desired_access: u32, inherit: bool, pid: u32) -> NativeHandle {
        // SAFETY: scalar-only API; returned ownership is handled by the caller.
        native_handle(unsafe { OpenProcess(desired_access, i32::from(inherit), pid) })
    }

    fn open_process_token(
        &self,
        process: NativeHandle,
        desired_access: u32,
    ) -> (bool, Option<NativeHandle>) {
        let mut token: HANDLE = null_mut();
        // SAFETY: process handle is live and the output slot is valid.
        let ok =
            unsafe { OpenProcessToken(raw_handle(process), desired_access, &raw mut token) } != 0;
        (ok, (!token.is_null()).then(|| native_handle(token)))
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        AclBindings::close_handle(self, handle)
    }

    fn get_token_information(
        &self,
        token: NativeHandle,
        class: u32,
        buffer: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool {
        let (pointer, length) = buffer.map_or((null_mut(), 0), |buffer| {
            (
                buffer.as_mut_ptr().cast::<c_void>(),
                u32::try_from(buffer.len()).expect("Win32 buffer length fits u32"),
            )
        });
        // SAFETY: optional buffer pointer/length pair and return slot are valid.
        unsafe {
            GetTokenInformation(
                raw_handle(token),
                class.cast_signed(),
                pointer,
                length,
                needed,
            ) != 0
        }
    }

    fn get_length_sid(&self, sid: NativePointer) -> u32 {
        // SAFETY: caller supplies a SID pointer from token information.
        unsafe { GetLengthSid(raw_pointer(sid)) }
    }

    fn copy_sid(&self, destination: &mut [u8], source: NativePointer) -> bool {
        // SAFETY: destination is writable for the advertised length and source is a valid SID.
        unsafe {
            CopySid(
                u32::try_from(destination.len()).expect("SID buffer fits u32"),
                destination.as_mut_ptr().cast(),
                raw_pointer(source),
            ) != 0
        }
    }

    fn create_well_known_sid(&self, kind: u32, sid: &mut [u8], size: &mut u32) -> bool {
        // SAFETY: SID output buffer and size slot are valid.
        unsafe {
            CreateWellKnownSid(
                kind.cast_signed(),
                null_mut(),
                sid.as_mut_ptr().cast(),
                size,
            ) != 0
        }
    }

    fn is_valid_sid(&self, sid: &[u8]) -> bool {
        // SAFETY: caller-owned SID storage remains live for this call.
        unsafe { IsValidSid(sid.as_ptr().cast_mut().cast()) != 0 }
    }

    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        old_acl: NativePointer,
    ) -> SetEntriesResult {
        AclBindings::set_entries_in_acl(self, entry, Some(old_acl))
    }

    fn set_token_information(&self, token: NativeHandle, class: u32, info: &[u8]) -> bool {
        // SAFETY: information slice remains live for the synchronous copy.
        unsafe {
            SetTokenInformation(
                raw_handle(token),
                class.cast_signed(),
                info.as_ptr().cast(),
                u32::try_from(info.len()).expect("token information fits u32"),
            ) != 0
        }
    }

    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        AclBindings::local_free(self, pointer)
    }

    fn create_restricted_token(
        &self,
        existing: NativeHandle,
        flags: u32,
        restricting_sids: &[NativePointer],
    ) -> (bool, Option<NativeHandle>) {
        let entries = restricting_sids
            .iter()
            .map(|sid| SID_AND_ATTRIBUTES {
                Sid: raw_pointer(*sid),
                Attributes: 0,
            })
            .collect::<Vec<_>>();
        let mut token: HANDLE = null_mut();
        // SAFETY: entry pointers remain live; empty disabled/privilege arrays
        // are represented by null pointers and the output slot is valid.
        let ok = unsafe {
            CreateRestrictedToken(
                raw_handle(existing),
                flags,
                0,
                null(),
                0,
                null(),
                u32::try_from(entries.len()).expect("restricting SID count fits u32"),
                entries.as_ptr(),
                &raw mut token,
            )
        } != 0;
        (ok, (!token.is_null()).then(|| native_handle(token)))
    }
}

impl SpawnBindings for WindowsBindings {
    fn last_error(&self) -> u32 {
        AclBindings::last_error(self)
    }

    fn format_message(&self, code: u32) -> String {
        AclBindings::format_message(self, code)
    }

    fn create_pipe(&self) -> (bool, Option<NativeHandle>, Option<NativeHandle>) {
        let mut read: HANDLE = null_mut();
        let mut write: HANDLE = null_mut();
        // SAFETY: both output slots are valid and default security is selected.
        let ok = unsafe { CreatePipe(&raw mut read, &raw mut write, null(), 0) } != 0;
        (
            ok,
            (!read.is_null()).then(|| native_handle(read)),
            (!write.is_null()).then(|| native_handle(write)),
        )
    }

    fn set_handle_information(&self, handle: NativeHandle, mask: u32, flags: u32) -> bool {
        // SAFETY: live handle and scalar flags.
        unsafe { SetHandleInformation(raw_handle(handle), mask, flags) != 0 }
    }

    fn create_process_as_user(
        &self,
        token: NativeHandle,
        command_line: &str,
        cwd: &Path,
        startup: StartupHandles,
        creation_flags: u32,
    ) -> (bool, ProcessInfo) {
        let mut command_line = wide(command_line);
        let cwd = wide(cwd.as_os_str());
        // SAFETY: zero is a valid baseline for both documented C structs.
        let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup_info.cb = u32::try_from(size_of::<STARTUPINFOW>()).expect("size fits u32");
        startup_info.dwFlags = abi::STARTF_USESTDHANDLES;
        startup_info.hStdInput = raw_handle(startup.stdin);
        startup_info.hStdOutput = raw_handle(startup.stdout);
        startup_info.hStdError = raw_handle(startup.stderr);
        // SAFETY: zero is the required empty output state.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: mutable command line, NUL-terminated cwd, live token and
        // startup/output structs all outlive the synchronous call.
        let ok = unsafe {
            CreateProcessAsUserW(
                raw_handle(token),
                null(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                creation_flags,
                null(),
                cwd.as_ptr(),
                &raw const startup_info,
                &raw mut info,
            )
        } != 0;
        let process_info = ProcessInfo {
            process: (!info.hProcess.is_null()).then(|| native_handle(info.hProcess)),
            thread: (!info.hThread.is_null()).then(|| native_handle(info.hThread)),
            process_id: info.dwProcessId,
            thread_id: info.dwThreadId,
        };
        (ok, process_info)
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        AclBindings::close_handle(self, handle)
    }

    fn peek_named_pipe(&self, handle: NativeHandle) -> PeekResult {
        let mut available = 0;
        // SAFETY: all unused out pointers are null and `available` is writable.
        let succeeded = unsafe {
            PeekNamedPipe(
                raw_handle(handle),
                null_mut(),
                0,
                null_mut(),
                &raw mut available,
                null_mut(),
            )
        } != 0;
        PeekResult {
            succeeded,
            available,
        }
    }

    fn read_file(&self, handle: NativeHandle, buffer: &mut [u8]) -> (bool, u32) {
        let mut read = 0;
        // SAFETY: mutable buffer and byte-count slot are valid; synchronous I/O uses null OVERLAPPED.
        let ok = unsafe {
            ReadFile(
                raw_handle(handle),
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).expect("read buffer fits u32"),
                &raw mut read,
                null_mut(),
            )
        } != 0;
        (ok, read)
    }

    fn wait_for_single_object(&self, handle: NativeHandle, milliseconds: u32) -> u32 {
        // SAFETY: live process handle and scalar timeout.
        unsafe { WaitForSingleObject(raw_handle(handle), milliseconds) }
    }

    fn get_exit_code_process(&self, process: NativeHandle) -> (bool, u32) {
        let mut code = 0;
        // SAFETY: live process handle and writable output slot.
        let ok = unsafe { GetExitCodeProcess(raw_handle(process), &raw mut code) } != 0;
        (ok, code)
    }

    fn create_job_object(&self) -> NativeHandle {
        // SAFETY: null security and name select documented defaults.
        native_handle(unsafe { CreateJobObjectW(null(), null()) })
    }

    fn set_information_job_object(&self, job: NativeHandle, information: &[u8]) -> bool {
        // SAFETY: verified 144-byte information block remains live for the call.
        unsafe {
            SetInformationJobObject(
                raw_handle(job),
                abi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION.cast_signed(),
                information.as_ptr().cast(),
                u32::try_from(information.len()).expect("job information fits u32"),
            ) != 0
        }
    }

    fn get_std_handle(&self, selector: i32) -> NativeHandle {
        // SAFETY: selector is one of the three documented STD_* constants.
        native_handle(unsafe { GetStdHandle(selector.cast_unsigned()) })
    }

    fn assign_process_to_job_object(&self, job: NativeHandle, process: NativeHandle) -> bool {
        // SAFETY: both handles are live and owned by the safe spawn state machine.
        unsafe { AssignProcessToJobObject(raw_handle(job), raw_handle(process)) != 0 }
    }

    fn terminate_process(&self, process: NativeHandle, exit_code: u32) -> bool {
        // SAFETY: process handle is live on this failure path.
        unsafe { TerminateProcess(raw_handle(process), exit_code) != 0 }
    }

    fn resume_thread(&self, thread: NativeHandle) -> u32 {
        // SAFETY: primary thread handle is live and currently suspended.
        unsafe { ResumeThread(raw_handle(thread)) }
    }
}

impl WindowsAclBindings for WindowsBindings {
    fn free_token_sid(&self, _sid: TokenSid) -> NativePointer {
        // Rust-owned SID buffers are released safely by drop at function exit.
        NativePointer::NULL
    }
}
