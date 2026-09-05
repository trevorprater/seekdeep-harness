//! Injected full-lifecycle parity for `AclSandbox` ownership and rollback.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_sandbox_windows_acl::{
    AclBindings, AclRead, AclSandbox, AclSandboxError, AclSandboxMode, AclSandboxOptions,
    AclTempDirState, GrantBindings, NativeHandle, NativePointer, ParsedSid, PeekResult,
    ProcessInfo, SandboxStdio, SetEntriesResult, SpawnBindings, StartupHandles, TokenBindings,
    TokenSid, Win32Error, WindowsAclBindings, abi,
};

struct LifecycleBindings {
    temp: tempfile::TempDir,
    calls: Mutex<Vec<String>>,
    next_sid: AtomicU64,
    next_pipe: AtomicU64,
    convert_fail: AtomicBool,
    convert_null: AtomicBool,
    restricted_fail: AtomicBool,
    close_fail: AtomicBool,
    cleanup_free_fail: AtomicBool,
    token_sid_free_fail: AtomicBool,
    last_error: std::sync::atomic::AtomicU32,
}

impl LifecycleBindings {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
            calls: Mutex::new(Vec::new()),
            next_sid: AtomicU64::new(42),
            next_pipe: AtomicU64::new(300),
            convert_fail: AtomicBool::new(false),
            convert_null: AtomicBool::new(false),
            restricted_fail: AtomicBool::new(false),
            close_fail: AtomicBool::new(false),
            cleanup_free_fail: AtomicBool::new(false),
            token_sid_free_fail: AtomicBool::new(false),
            last_error: std::sync::atomic::AtomicU32::new(5),
        }
    }

    fn record(&self, value: impl Into<String>) {
        self.calls.lock().push(value.into());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
}

impl AclBindings for LifecycleBindings {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }
    fn format_message(&self, _code: u32) -> String {
        String::new()
    }
    fn get_temp_path(&self, _capacity: u32, buffer: &mut [u16]) -> u32 {
        let text = format!(
            "{}{sep}",
            self.temp.path().display(),
            sep = std::path::MAIN_SEPARATOR
        );
        let encoded = text.encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        u32::try_from(encoded.len()).expect("test path fits u32")
    }
    fn create_lock_file(&self, _path: &Path) -> NativeHandle {
        NativeHandle::from_raw(7)
    }
    fn lock_file(&self, _handle: NativeHandle, _flags: u32) -> bool {
        true
    }
    fn unlock_file(&self, _handle: NativeHandle) -> bool {
        true
    }
    fn close_handle(&self, handle: NativeHandle) -> bool {
        self.record(format!("close:{}", handle.raw()));
        !self.close_fail.load(Ordering::Relaxed) || !matches!(handle.raw(), 21 | 22)
    }
    fn read_current_dacl(&self, path: &Path) -> AclRead {
        self.record(format!("read:{}", path.display()));
        AclRead {
            code: 0,
            acl: None,
            descriptor: None,
        }
    }
    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        _old_acl: Option<NativePointer>,
    ) -> SetEntriesResult {
        self.record(format!(
            "acl-mode:{}",
            u32::from_le_bytes(entry[4..8].try_into().unwrap())
        ));
        SetEntriesResult {
            code: 0,
            acl: Some(NativePointer::from_raw(9)),
        }
    }
    fn set_named_security_info(&self, path: &Path, _acl: NativePointer) -> u32 {
        self.record(format!("apply:{}", path.display()));
        0
    }
    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        self.record(format!("free:{}", pointer.raw()));
        if self.cleanup_free_fail.load(Ordering::Relaxed) && pointer.raw() >= 42 {
            NativePointer::from_raw(1)
        } else {
            NativePointer::NULL
        }
    }
}

impl GrantBindings for LifecycleBindings {
    fn convert_string_sid(&self, sid: &str) -> Result<ParsedSid, Win32Error> {
        self.record(format!("convert:{sid}"));
        if self.convert_fail.load(Ordering::Relaxed) {
            return Err(Win32Error::new(
                "ConvertStringSidToSidW",
                87,
                Some(sid.into()),
            ));
        }
        let pointer = if self.convert_null.load(Ordering::Relaxed) {
            NativePointer::NULL
        } else {
            NativePointer::from_raw(self.next_sid.fetch_add(1, Ordering::Relaxed))
        };
        Ok(ParsedSid {
            pointer,
            bytes: vec![1, 0, 0, 0, 0, 0, 0, 5],
        })
    }
}

impl TokenBindings for LifecycleBindings {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }
    fn format_message(&self, _code: u32) -> String {
        String::new()
    }
    fn open_process(&self, _desired_access: u32, _inherit: bool, _pid: u32) -> NativeHandle {
        NativeHandle::from_raw(20)
    }
    fn open_process_token(
        &self,
        _process: NativeHandle,
        _desired_access: u32,
    ) -> (bool, Option<NativeHandle>) {
        (true, Some(NativeHandle::from_raw(21)))
    }
    fn close_handle(&self, handle: NativeHandle) -> bool {
        AclBindings::close_handle(self, handle)
    }
    fn get_token_information(
        &self,
        _token: NativeHandle,
        class: u32,
        buffer: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool {
        match class {
            abi::TOKEN_GROUPS => {
                *needed = 24;
                let Some(buffer) = buffer else { return false };
                buffer[0..4].copy_from_slice(&1_u32.to_le_bytes());
                buffer[8..16].copy_from_slice(&77_u64.to_le_bytes());
                buffer[16..20].copy_from_slice(&abi::SE_GROUP_LOGON_ID.to_le_bytes());
                true
            }
            abi::TOKEN_DEFAULT_DACL => {
                *needed = 8;
                let Some(buffer) = buffer else { return false };
                buffer[0..8].copy_from_slice(&88_u64.to_le_bytes());
                true
            }
            _ => false,
        }
    }
    fn get_length_sid(&self, _sid: NativePointer) -> u32 {
        12
    }
    fn copy_sid(&self, destination: &mut [u8], _source: NativePointer) -> bool {
        destination.fill(0x11);
        true
    }
    fn create_well_known_sid(&self, _kind: u32, sid: &mut [u8], _size: &mut u32) -> bool {
        sid.fill(0x22);
        true
    }
    fn is_valid_sid(&self, _sid: &[u8]) -> bool {
        true
    }
    fn set_entries_in_acl(
        &self,
        _entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        _old_acl: NativePointer,
    ) -> SetEntriesResult {
        SetEntriesResult {
            code: 0,
            acl: Some(NativePointer::from_raw(9)),
        }
    }
    fn set_token_information(&self, _token: NativeHandle, _class: u32, _info: &[u8]) -> bool {
        true
    }
    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        AclBindings::local_free(self, pointer)
    }
    fn create_restricted_token(
        &self,
        _existing: NativeHandle,
        _flags: u32,
        restricting_sids: &[NativePointer],
    ) -> (bool, Option<NativeHandle>) {
        self.record(format!("restrict-count:{}", restricting_sids.len()));
        if self.restricted_fail.load(Ordering::Relaxed) {
            (false, None)
        } else {
            (true, Some(NativeHandle::from_raw(22)))
        }
    }
}

impl SpawnBindings for LifecycleBindings {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }
    fn format_message(&self, _code: u32) -> String {
        String::new()
    }
    fn create_pipe(&self) -> (bool, Option<NativeHandle>, Option<NativeHandle>) {
        let read = self.next_pipe.fetch_add(2, Ordering::Relaxed);
        (
            true,
            Some(NativeHandle::from_raw(read)),
            Some(NativeHandle::from_raw(read + 1)),
        )
    }
    fn set_handle_information(&self, _handle: NativeHandle, _mask: u32, _flags: u32) -> bool {
        true
    }
    fn create_process_as_user(
        &self,
        _token: NativeHandle,
        _command_line: &str,
        _cwd: &Path,
        _startup: StartupHandles,
        _creation_flags: u32,
    ) -> (bool, ProcessInfo) {
        (
            true,
            ProcessInfo {
                process: Some(NativeHandle::from_raw(400)),
                thread: Some(NativeHandle::from_raw(401)),
                process_id: 402,
                thread_id: 403,
            },
        )
    }
    fn close_handle(&self, handle: NativeHandle) -> bool {
        AclBindings::close_handle(self, handle)
    }
    fn peek_named_pipe(&self, _handle: NativeHandle) -> PeekResult {
        self.last_error
            .store(abi::ERROR_BROKEN_PIPE, Ordering::Relaxed);
        PeekResult {
            succeeded: false,
            available: 0,
        }
    }
    fn read_file(&self, _handle: NativeHandle, _buffer: &mut [u8]) -> (bool, u32) {
        (true, 0)
    }
    fn wait_for_single_object(&self, _handle: NativeHandle, _milliseconds: u32) -> u32 {
        0
    }
    fn get_exit_code_process(&self, _process: NativeHandle) -> (bool, u32) {
        (true, 42)
    }
    fn create_job_object(&self) -> NativeHandle {
        NativeHandle::from_raw(500)
    }
    fn set_information_job_object(&self, _job: NativeHandle, _information: &[u8]) -> bool {
        true
    }
    fn get_std_handle(&self, selector: i32) -> NativeHandle {
        NativeHandle::from_raw(u64::try_from(-selector).expect("selectors are negative") + 500)
    }
    fn assign_process_to_job_object(&self, _job: NativeHandle, _process: NativeHandle) -> bool {
        true
    }
    fn terminate_process(&self, _process: NativeHandle, _exit_code: u32) -> bool {
        true
    }
    fn resume_thread(&self, _thread: NativeHandle) -> u32 {
        1
    }
}

impl WindowsAclBindings for LifecycleBindings {
    fn free_token_sid(&self, _sid: TokenSid) -> NativePointer {
        self.record("free-token-sid");
        if self.token_sid_free_fail.load(Ordering::Relaxed) {
            NativePointer::from_raw(1)
        } else {
            NativePointer::NULL
        }
    }
}

fn workspace_options(
    workspace: &Path,
    temp: Option<&Path>,
    manage_dacls: bool,
) -> AclSandboxOptions {
    AclSandboxOptions {
        writable_dirs: vec![workspace.to_owned()],
        temp_dir: temp.map(Path::to_owned),
        temp_was_explicit: true,
        write_sid: Some("S-1-4-1-2".into()),
        temp_write_sid: temp.map(|_| "S-1-4-3-4-1".into()),
        mode: AclSandboxMode::WorkspaceWrite,
        manage_dacls,
    }
}

#[test]
fn constructor_validates_writable_roots_but_defers_temp_filesystem_validation() {
    let api = Arc::new(LifecycleBindings::new());
    let root = tempfile::tempdir().unwrap();
    let missing_workspace = root.path().join("missing-workspace");
    assert!(
        AclSandbox::new(
            &workspace_options(&missing_workspace, None, true),
            api.clone()
        )
        .is_err()
    );

    let missing_temp = root.path().join("missing-temp");
    let mut sandbox = AclSandbox::new(
        &workspace_options(root.path(), Some(&missing_temp), true),
        api,
    )
    .unwrap();
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
    assert!(
        sandbox
            .init(1)
            .unwrap_err()
            .to_string()
            .contains("temp dir does not exist")
    );
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
}

#[test]
fn happy_workspace_lifecycle_keeps_workspace_grant_and_revokes_temp_once() {
    let api = Arc::new(LifecycleBindings::new());
    let workspace = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox = AclSandbox::new(
        &workspace_options(workspace.path(), Some(temp.path()), true),
        binding,
    )
    .unwrap();
    sandbox.init(1).unwrap();
    assert_eq!(
        sandbox.temp_dir(),
        &AclTempDirState::Enabled(temp.path().to_owned())
    );
    assert_eq!(
        sandbox.init(1).unwrap_err().to_string(),
        "AclSandbox is already initialized"
    );
    sandbox.dispose().unwrap();
    let calls = api.calls();
    let workspace_read = format!("read:{}", workspace.path().display());
    let temp_read = format!("read:{}", temp.path().display());
    assert_eq!(
        calls.iter().filter(|call| *call == &workspace_read).count(),
        1
    );
    assert_eq!(calls.iter().filter(|call| *call == &temp_read).count(), 2);
    assert!(calls.contains(&"close:22".into()));
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == "free-token-sid")
            .count(),
        2
    );
}

#[test]
fn read_only_and_caller_managed_modes_apply_no_backend_owned_grants() {
    let workspace = tempfile::tempdir().unwrap();
    let api = Arc::new(LifecycleBindings::new());
    let read_only = AclSandboxOptions {
        writable_dirs: Vec::new(),
        temp_dir: None,
        temp_was_explicit: false,
        write_sid: None,
        temp_write_sid: None,
        mode: AclSandboxMode::ReadOnly,
        manage_dacls: true,
    };
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox = AclSandbox::new(&read_only, binding).unwrap();
    sandbox.init(1).unwrap();
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Disabled);
    assert!(
        !api.calls()
            .iter()
            .any(|call| call.starts_with("convert:") || call.starts_with("read:"))
    );
    sandbox.dispose().unwrap();

    let api = Arc::new(LifecycleBindings::new());
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox =
        AclSandbox::new(&workspace_options(workspace.path(), None, false), binding).unwrap();
    sandbox.init(1).unwrap();
    sandbox.dispose().unwrap();
    assert!(!api.calls().iter().any(|call| call.starts_with("read:")));
}

#[test]
fn failed_init_rolls_back_and_can_be_retried_without_provisional_temp_state() {
    let workspace = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let api = Arc::new(LifecycleBindings::new());
    api.convert_fail.store(true, Ordering::Relaxed);
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox = AclSandbox::new(
        &workspace_options(workspace.path(), Some(temp.path()), true),
        binding,
    )
    .unwrap();
    assert!(
        sandbox
            .init(1)
            .unwrap_err()
            .to_string()
            .contains("ConvertStringSidToSidW")
    );
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
    api.convert_fail.store(false, Ordering::Relaxed);
    sandbox.init(1).unwrap();
    sandbox.dispose().unwrap();
}

#[test]
fn failed_token_pipeline_aggregates_every_cleanup_failure_in_order() {
    let workspace = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let api = Arc::new(LifecycleBindings::new());
    api.restricted_fail.store(true, Ordering::Relaxed);
    api.close_fail.store(true, Ordering::Relaxed);
    api.cleanup_free_fail.store(true, Ordering::Relaxed);
    api.token_sid_free_fail.store(true, Ordering::Relaxed);
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox = AclSandbox::new(
        &workspace_options(workspace.path(), Some(temp.path()), true),
        binding,
    )
    .unwrap();
    let error = sandbox.init(1).unwrap_err();
    let AclSandboxError::InitCleanup { primary, cleanup } = error else {
        panic!("expected aggregated init failure")
    };
    assert!(
        primary.to_string().contains("CreateRestrictedToken"),
        "unexpected primary: {primary:?}"
    );
    assert_eq!(cleanup.len(), 5);
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
}

#[tokio::test]
async fn child_wait_is_idempotent_for_pipe_and_inherited_stdio() {
    let workspace = tempfile::tempdir().unwrap();
    let api = Arc::new(LifecycleBindings::new());
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox =
        AclSandbox::new(&workspace_options(workspace.path(), None, false), binding).unwrap();
    assert!(
        sandbox
            .spawn("x", &[], workspace.path(), SandboxStdio::Pipe)
            .is_err()
    );
    sandbox.init(1).unwrap();

    let pipe = sandbox
        .spawn("x", &[], workspace.path(), SandboxStdio::Pipe)
        .unwrap();
    assert_eq!(pipe.pid(), 402);
    let expected = pipe.wait().await.unwrap();
    assert_eq!(expected.exit_code, 42);
    assert_eq!(pipe.wait().await.unwrap(), expected);

    let inherited = sandbox
        .spawn("x", &[], workspace.path(), SandboxStdio::Inherit)
        .unwrap();
    let expected = inherited.wait().await.unwrap();
    assert!(expected.stdout.is_empty() && expected.stderr.is_empty());
    assert_eq!(inherited.wait().await.unwrap(), expected);
    assert_eq!(
        api.calls()
            .iter()
            .filter(|call| call.as_str() == "close:500")
            .count(),
        1
    );
    sandbox.dispose().unwrap();
}

#[test]
fn dispose_is_noop_before_init_and_aggregates_owned_cleanup_failures() {
    let workspace = tempfile::tempdir().unwrap();
    let api = Arc::new(LifecycleBindings::new());
    let binding: Arc<dyn WindowsAclBindings> = api.clone();
    let mut sandbox =
        AclSandbox::new(&workspace_options(workspace.path(), None, false), binding).unwrap();
    sandbox.dispose().unwrap();
    sandbox.init(1).unwrap();
    api.close_fail.store(true, Ordering::Relaxed);
    api.cleanup_free_fail.store(true, Ordering::Relaxed);
    api.token_sid_free_fail.store(true, Ordering::Relaxed);
    let error = sandbox.dispose().unwrap_err();
    let AclSandboxError::DisposeCleanup { failures } = error else {
        panic!("expected dispose aggregate")
    };
    assert_eq!(failures.len(), 4);
    sandbox.dispose().unwrap();
}
