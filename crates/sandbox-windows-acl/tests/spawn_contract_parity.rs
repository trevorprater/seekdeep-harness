//! Portable injected-binding parity for native process and pipe lifecycle.

use std::{
    collections::VecDeque,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_sandbox_windows_acl::{
    NativeHandle, PeekResult, ProcessInfo, SpawnBindings, SpawnError, SpawnOptions, StartupHandles,
    abi, drain_pipe, spawn_sandboxed, spawn_sandboxed_inherited, wait_for_exit,
};

struct FakeSpawn {
    last_error: AtomicU32,
    pipe_ok: AtomicBool,
    null_pipe: AtomicBool,
    next_pipe: AtomicU64,
    inherit_fail_call: AtomicU32,
    inherit_calls: AtomicU32,
    created: AtomicBool,
    process: AtomicU64,
    thread: AtomicU64,
    pid: AtomicU32,
    closes: Mutex<Vec<NativeHandle>>,
    peeks: Mutex<VecDeque<(PeekResult, u32)>>,
    read_ok: AtomicBool,
    read_payload: Mutex<Vec<u8>>,
    wait_result: AtomicU32,
    exit_ok: AtomicBool,
    exit_code: AtomicU32,
    job: AtomicU64,
    set_job_ok: AtomicBool,
    stdin: AtomicU64,
    stdout: AtomicU64,
    stderr: AtomicU64,
    assign_ok: AtomicBool,
    resume_result: AtomicU32,
    terminated: Mutex<Vec<(NativeHandle, u32)>>,
    creation_flags: AtomicU32,
    handle_changes: Mutex<Vec<(NativeHandle, u32)>>,
}

impl FakeSpawn {
    fn new() -> Self {
        Self {
            last_error: AtomicU32::new(5),
            pipe_ok: AtomicBool::new(true),
            null_pipe: AtomicBool::new(false),
            next_pipe: AtomicU64::new(10),
            inherit_fail_call: AtomicU32::new(0),
            inherit_calls: AtomicU32::new(0),
            created: AtomicBool::new(true),
            process: AtomicU64::new(100),
            thread: AtomicU64::new(101),
            pid: AtomicU32::new(1234),
            closes: Mutex::new(Vec::new()),
            peeks: Mutex::new(VecDeque::new()),
            read_ok: AtomicBool::new(true),
            read_payload: Mutex::new(Vec::new()),
            wait_result: AtomicU32::new(0),
            exit_ok: AtomicBool::new(true),
            exit_code: AtomicU32::new(42),
            job: AtomicU64::new(200),
            set_job_ok: AtomicBool::new(true),
            stdin: AtomicU64::new(300),
            stdout: AtomicU64::new(301),
            stderr: AtomicU64::new(302),
            assign_ok: AtomicBool::new(true),
            resume_result: AtomicU32::new(1),
            terminated: Mutex::new(Vec::new()),
            creation_flags: AtomicU32::new(0),
            handle_changes: Mutex::new(Vec::new()),
        }
    }

    fn process_info(&self) -> ProcessInfo {
        let process = self.process.load(Ordering::Relaxed);
        let thread = self.thread.load(Ordering::Relaxed);
        ProcessInfo {
            process: (process != 0).then(|| NativeHandle::from_raw(process)),
            thread: (thread != 0).then(|| NativeHandle::from_raw(thread)),
            process_id: self.pid.load(Ordering::Relaxed),
            thread_id: 5678,
        }
    }
}

impl SpawnBindings for FakeSpawn {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }

    fn format_message(&self, _code: u32) -> String {
        String::new()
    }

    fn create_pipe(&self) -> (bool, Option<NativeHandle>, Option<NativeHandle>) {
        if !self.pipe_ok.load(Ordering::Relaxed) {
            return (false, None, None);
        }
        if self.null_pipe.load(Ordering::Relaxed) {
            return (true, None, Some(NativeHandle::from_raw(1)));
        }
        let read = self.next_pipe.fetch_add(2, Ordering::Relaxed);
        (
            true,
            Some(NativeHandle::from_raw(read)),
            Some(NativeHandle::from_raw(read + 1)),
        )
    }

    fn set_handle_information(&self, handle: NativeHandle, _mask: u32, flags: u32) -> bool {
        self.handle_changes.lock().push((handle, flags));
        let call = self.inherit_calls.fetch_add(1, Ordering::Relaxed) + 1;
        self.inherit_fail_call.load(Ordering::Relaxed) != call
    }

    fn create_process_as_user(
        &self,
        _token: NativeHandle,
        _command_line: &str,
        _cwd: &Path,
        _startup: StartupHandles,
        creation_flags: u32,
    ) -> (bool, ProcessInfo) {
        self.creation_flags.store(creation_flags, Ordering::Relaxed);
        (self.created.load(Ordering::Relaxed), self.process_info())
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        self.closes.lock().push(handle);
        true
    }

    fn peek_named_pipe(&self, _handle: NativeHandle) -> PeekResult {
        let (result, error) = self.peeks.lock().pop_front().unwrap_or((
            PeekResult {
                succeeded: false,
                available: 0,
            },
            abi::ERROR_BROKEN_PIPE,
        ));
        self.last_error.store(error, Ordering::Relaxed);
        result
    }

    fn read_file(&self, _handle: NativeHandle, buffer: &mut [u8]) -> (bool, u32) {
        if !self.read_ok.load(Ordering::Relaxed) {
            return (false, 0);
        }
        let payload = self.read_payload.lock();
        let count = payload.len().min(buffer.len());
        buffer[..count].copy_from_slice(&payload[..count]);
        (
            true,
            u32::try_from(count).expect("test payload length fits in u32"),
        )
    }

    fn wait_for_single_object(&self, _handle: NativeHandle, _milliseconds: u32) -> u32 {
        self.wait_result.load(Ordering::Relaxed)
    }

    fn get_exit_code_process(&self, _process: NativeHandle) -> (bool, u32) {
        (
            self.exit_ok.load(Ordering::Relaxed),
            self.exit_code.load(Ordering::Relaxed),
        )
    }

    fn create_job_object(&self) -> NativeHandle {
        NativeHandle::from_raw(self.job.load(Ordering::Relaxed))
    }

    fn set_information_job_object(&self, _job: NativeHandle, information: &[u8]) -> bool {
        assert_eq!(information.len(), abi::JOBOBJECT_EXTENDED_LIMIT_SIZE);
        assert_eq!(
            u32::from_le_bytes(
                information[abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET
                    ..abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET + 4]
                    .try_into()
                    .unwrap()
            ),
            abi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
        self.set_job_ok.load(Ordering::Relaxed)
    }

    fn get_std_handle(&self, selector: i32) -> NativeHandle {
        let raw = match selector {
            abi::STD_INPUT_HANDLE => self.stdin.load(Ordering::Relaxed),
            abi::STD_OUTPUT_HANDLE => self.stdout.load(Ordering::Relaxed),
            abi::STD_ERROR_HANDLE => self.stderr.load(Ordering::Relaxed),
            _ => 0,
        };
        NativeHandle::from_raw(raw)
    }

    fn assign_process_to_job_object(&self, _job: NativeHandle, _process: NativeHandle) -> bool {
        self.assign_ok.load(Ordering::Relaxed)
    }

    fn terminate_process(&self, process: NativeHandle, exit_code: u32) -> bool {
        self.terminated.lock().push((process, exit_code));
        true
    }

    fn resume_thread(&self, _thread: NativeHandle) -> u32 {
        self.resume_result.load(Ordering::Relaxed)
    }
}

fn options(args: &[String]) -> SpawnOptions<'_> {
    SpawnOptions {
        command: "pwsh.exe",
        args,
        cwd: Path::new("C:\\workspace"),
    }
}

fn assert_win32(error: SpawnError, api: &str) {
    match error {
        SpawnError::Win32(error) => assert_eq!(error.api, api),
        other @ SpawnError::NullProcessHandles { .. } => {
            panic!("expected {api} Win32 error, got {other}")
        }
    }
}

#[test]
fn piped_spawn_checks_pipe_and_inheritance_and_closes_all_six_on_create_failure() {
    let args = Vec::new();
    let api = FakeSpawn::new();
    api.pipe_ok.store(false, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "CreatePipe",
    );

    let api = FakeSpawn::new();
    api.null_pipe.store(true, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "CreatePipe",
    );

    let api = FakeSpawn::new();
    api.inherit_fail_call.store(2, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "SetHandleInformation",
    );

    let api = FakeSpawn::new();
    api.created.store(false, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "CreateProcessAsUserW",
    );
    assert_eq!(
        api.closes
            .lock()
            .iter()
            .map(|handle| handle.raw())
            .collect::<Vec<_>>(),
        [10, 11, 12, 13, 14, 15]
    );
}

#[test]
fn piped_spawn_preserves_null_success_failure_and_success_handle_ownership() {
    let args = vec!["-NoProfile".to_owned()];
    let api = FakeSpawn::new();
    api.thread.store(0, Ordering::Relaxed);
    assert!(matches!(
        spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        SpawnError::NullProcessHandles { pid: 1234 }
    ));

    let api = FakeSpawn::new();
    let child = spawn_sandboxed(&api, NativeHandle::from_raw(9), &options(&args)).unwrap();
    assert_eq!(child.pid, 1234);
    assert_eq!(child.process, NativeHandle::from_raw(100));
    assert_eq!(child.stdout_read, NativeHandle::from_raw(12));
    assert_eq!(child.stderr_read, NativeHandle::from_raw(14));
    assert_eq!(
        api.closes
            .lock()
            .iter()
            .map(|handle| handle.raw())
            .collect::<Vec<_>>(),
        [10, 13, 15, 11, 101]
    );
}

#[test]
fn inherited_spawn_enforces_job_before_resume_and_cleans_each_failure_shape() {
    let args = Vec::new();
    let api = FakeSpawn::new();
    api.job.store(0, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "CreateJobObjectW",
    );

    let api = FakeSpawn::new();
    api.set_job_ok.store(false, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "SetInformationJobObject",
    );
    assert_eq!(api.closes.lock().as_slice(), &[NativeHandle::from_raw(200)]);

    let api = FakeSpawn::new();
    api.stdout.store(0, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "GetStdHandle",
    );

    let api = FakeSpawn::new();
    api.inherit_fail_call.store(1, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "SetHandleInformation",
    );

    let api = FakeSpawn::new();
    api.created.store(false, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "CreateProcessAsUserW",
    );
    assert!(api.closes.lock().contains(&NativeHandle::from_raw(200)));

    let api = FakeSpawn::new();
    api.process.store(0, Ordering::Relaxed);
    assert!(matches!(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        SpawnError::NullProcessHandles { pid: 1234 }
    ));
    assert_eq!(api.closes.lock().as_slice(), &[NativeHandle::from_raw(200)]);
}

#[test]
fn inherited_assignment_failure_terminates_suspended_child_and_resume_failure_closes_job() {
    let args = Vec::new();
    let api = FakeSpawn::new();
    api.assign_ok.store(false, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "AssignProcessToJobObject",
    );
    assert_eq!(
        api.terminated.lock().as_slice(),
        &[(NativeHandle::from_raw(100), 1)]
    );
    assert_eq!(
        api.closes
            .lock()
            .iter()
            .map(|handle| handle.raw())
            .collect::<Vec<_>>(),
        [101, 100, 200]
    );

    let api = FakeSpawn::new();
    api.resume_result.store(u32::MAX, Ordering::Relaxed);
    assert_win32(
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap_err(),
        "ResumeThread",
    );
    assert!(api.terminated.lock().is_empty());
    assert_eq!(
        api.closes
            .lock()
            .iter()
            .map(|handle| handle.raw())
            .collect::<Vec<_>>(),
        [101, 100, 200]
    );
}

#[test]
fn inherited_success_restores_stdio_and_returns_process_and_job() {
    let args = Vec::new();
    let api = FakeSpawn::new();
    let child =
        spawn_sandboxed_inherited(&api, NativeHandle::from_raw(9), &options(&args)).unwrap();
    assert_eq!(
        child,
        seekdeep_sandbox_windows_acl::SpawnedInherited {
            pid: 1234,
            process: NativeHandle::from_raw(100),
            job: NativeHandle::from_raw(200),
        }
    );
    assert_eq!(
        api.creation_flags.load(Ordering::Relaxed),
        abi::CREATE_SUSPENDED
    );
    assert_eq!(
        api.handle_changes
            .lock()
            .iter()
            .map(|(_, flags)| *flags)
            .collect::<Vec<_>>(),
        [1, 1, 1, 0, 0, 0]
    );
    assert_eq!(api.closes.lock().as_slice(), &[NativeHandle::from_raw(101)]);
}

#[tokio::test]
async fn pipe_drain_distinguishes_clean_eof_peek_failure_read_failure_and_content() {
    let handle = NativeHandle::from_raw(50);
    let api = FakeSpawn::new();
    api.peeks.lock().push_back((
        PeekResult {
            succeeded: false,
            available: 0,
        },
        abi::ERROR_NO_DATA,
    ));
    assert_eq!(drain_pipe(&api, handle).await.unwrap(), Vec::<u8>::new());
    assert_eq!(api.closes.lock().as_slice(), &[handle]);

    let api = FakeSpawn::new();
    api.peeks.lock().push_back((
        PeekResult {
            succeeded: false,
            available: 0,
        },
        5,
    ));
    assert_win32(drain_pipe(&api, handle).await.unwrap_err(), "PeekNamedPipe");

    let api = FakeSpawn::new();
    api.peeks.lock().push_back((
        PeekResult {
            succeeded: true,
            available: 3,
        },
        0,
    ));
    api.read_ok.store(false, Ordering::Relaxed);
    assert_win32(drain_pipe(&api, handle).await.unwrap_err(), "ReadFile");

    let api = FakeSpawn::new();
    api.peeks.lock().extend([
        (
            PeekResult {
                succeeded: true,
                available: 3,
            },
            0,
        ),
        (
            PeekResult {
                succeeded: false,
                available: 0,
            },
            abi::ERROR_BROKEN_PIPE,
        ),
    ]);
    *api.read_payload.lock() = b"abc".to_vec();
    assert_eq!(drain_pipe(&api, handle).await.unwrap(), b"abc");
    assert_eq!(api.closes.lock().as_slice(), &[handle]);
}

#[test]
fn exit_wait_checks_both_calls_and_closes_only_after_success() {
    let process = NativeHandle::from_raw(100);
    let api = FakeSpawn::new();
    api.wait_result.store(u32::MAX, Ordering::Relaxed);
    assert_win32(
        wait_for_exit(&api, process).unwrap_err(),
        "WaitForSingleObject",
    );
    assert!(api.closes.lock().is_empty());

    let api = FakeSpawn::new();
    api.exit_ok.store(false, Ordering::Relaxed);
    assert_win32(
        wait_for_exit(&api, process).unwrap_err(),
        "GetExitCodeProcess",
    );
    assert!(api.closes.lock().is_empty());

    let api = FakeSpawn::new();
    assert_eq!(wait_for_exit(&api, process).unwrap(), 42);
    assert_eq!(api.closes.lock().as_slice(), &[process]);
}
