//! Portable injected-binding parity for ACL and grant failure lifecycles.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use parking_lot::Mutex;
use seekdeep_sandbox_windows_acl::{
    AclBindings, AclRead, AclWithPointer, AclWriteGrant, GrantBindings, NativeHandle,
    NativePointer, ParsedSid, SetEntriesResult, Win32Error, abi, build_explicit_access,
    grant_write, lock_file_path, revoke_write, with_path_lock,
};

struct FakeAcl {
    temp: tempfile::TempDir,
    temp_length_override: AtomicU32,
    last_error: AtomicU32,
    create_handle: AtomicU64,
    lock_ok: AtomicBool,
    unlock_ok: AtomicBool,
    close_ok: AtomicBool,
    read: Mutex<AclRead>,
    merge: Mutex<SetEntriesResult>,
    apply_code: AtomicU32,
    frees: Mutex<VecDeque<NativePointer>>,
    calls: Mutex<Vec<String>>,
    convert_ok: AtomicBool,
}

impl FakeAcl {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
            temp_length_override: AtomicU32::new(u32::MAX),
            last_error: AtomicU32::new(5),
            create_handle: AtomicU64::new(7),
            lock_ok: AtomicBool::new(true),
            unlock_ok: AtomicBool::new(true),
            close_ok: AtomicBool::new(true),
            read: Mutex::new(AclRead {
                code: 0,
                acl: None,
                descriptor: None,
            }),
            merge: Mutex::new(SetEntriesResult {
                code: 0,
                acl: Some(NativePointer::from_raw(9)),
            }),
            apply_code: AtomicU32::new(0),
            frees: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            convert_ok: AtomicBool::new(true),
        }
    }

    fn call_names(&self) -> Vec<String> {
        self.calls.lock().clone()
    }

    fn record(&self, call: impl Into<String>) {
        self.calls.lock().push(call.into());
    }
}

impl AclBindings for FakeAcl {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }

    fn format_message(&self, _code: u32) -> String {
        String::new()
    }

    fn get_temp_path(&self, _capacity: u32, buffer: &mut [u16]) -> u32 {
        let override_length = self.temp_length_override.load(Ordering::Relaxed);
        if override_length != u32::MAX {
            return override_length;
        }
        let mut text = self.temp.path().to_string_lossy().into_owned();
        text.push(std::path::MAIN_SEPARATOR);
        let encoded = text.encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        u32::try_from(encoded.len()).expect("test temp path fits in u32")
    }

    fn create_lock_file(&self, path: &Path) -> NativeHandle {
        self.record(format!("open:{}", path.display()));
        NativeHandle::from_raw(self.create_handle.load(Ordering::Relaxed))
    }

    fn lock_file(&self, handle: NativeHandle, _flags: u32) -> bool {
        self.record(format!("lock:{}", handle.raw()));
        self.lock_ok.load(Ordering::Relaxed)
    }

    fn unlock_file(&self, handle: NativeHandle) -> bool {
        self.record(format!("unlock:{}", handle.raw()));
        self.unlock_ok.load(Ordering::Relaxed)
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        self.record(format!("close:{}", handle.raw()));
        self.close_ok.load(Ordering::Relaxed)
    }

    fn read_current_dacl(&self, path: &Path) -> AclRead {
        self.record(format!("read:{}", path.display()));
        self.read.lock().clone()
    }

    fn set_entries_in_acl(
        &self,
        entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        old_acl: Option<NativePointer>,
    ) -> SetEntriesResult {
        self.record(format!(
            "merge:{}:{}",
            u32::from_le_bytes(entry[4..8].try_into().unwrap()),
            old_acl.map_or(0, NativePointer::raw)
        ));
        *self.merge.lock()
    }

    fn set_named_security_info(&self, path: &Path, acl: NativePointer) -> u32 {
        self.record(format!("apply:{}:{}", path.display(), acl.raw()));
        self.apply_code.load(Ordering::Relaxed)
    }

    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        self.record(format!("free:{}", pointer.raw()));
        self.frees.lock().pop_front().unwrap_or(NativePointer::NULL)
    }
}

impl GrantBindings for FakeAcl {
    fn convert_string_sid(&self, sid: &str) -> Result<ParsedSid, Win32Error> {
        self.record(format!("convert:{sid}"));
        if !self.convert_ok.load(Ordering::Relaxed) {
            return Err(Win32Error::new(
                "ConvertStringSidToSidW",
                self.last_error.load(Ordering::Relaxed),
                Some(sid.into()),
            ));
        }
        Ok(ParsedSid {
            pointer: NativePointer::from_raw(42),
            bytes: vec![1, 1, 0, 0, 0, 0, 0, 5, 7, 0, 0, 0],
        })
    }
}

fn exact_acl(sid: &[u8]) -> Vec<u8> {
    let size = 8 + 8 + sid.len();
    let mut acl = vec![0_u8; size];
    acl[2..4].copy_from_slice(
        &u16::try_from(size)
            .expect("test ACL fits in u16")
            .to_le_bytes(),
    );
    acl[4..6].copy_from_slice(&1_u16.to_le_bytes());
    acl[8] = abi::ACCESS_ALLOWED_ACE_TYPE;
    acl[9] = abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT.to_le_bytes()[0];
    acl[10..12].copy_from_slice(
        &u16::try_from(8 + sid.len())
            .expect("test ACE fits in u16")
            .to_le_bytes(),
    );
    acl[12..16].copy_from_slice(&abi::GRANT_MASK.to_le_bytes());
    acl[16..].copy_from_slice(sid);
    acl
}

#[test]
fn explicit_access_layout_and_lock_name_match_the_x64_oracle() {
    let entry = build_explicit_access(
        NativePointer::from_raw(0x1122_3344_5566_7788),
        abi::GRANT_ACCESS,
        abi::GRANT_MASK,
    );
    assert_eq!(entry.len(), 48);
    assert_eq!(
        u32::from_le_bytes(entry[0..4].try_into().unwrap()),
        abi::GRANT_MASK
    );
    assert_eq!(
        u32::from_le_bytes(entry[4..8].try_into().unwrap()),
        abi::GRANT_ACCESS
    );
    assert_eq!(u32::from_le_bytes(entry[8..12].try_into().unwrap()), 3);
    assert!(entry[12..24].iter().all(|byte| *byte == 0));
    assert_eq!(
        u64::from_le_bytes(entry[40..48].try_into().unwrap()),
        0x1122_3344_5566_7788
    );

    let api = FakeAcl::new();
    let first = lock_file_path(&api, Path::new("C:\\Repo")).unwrap();
    let second = lock_file_path(&api, Path::new("c:\\repo")).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.file_name().unwrap().to_string_lossy().len(), 21);
}

#[test]
fn temp_path_zero_and_unwritten_overflow_fail_before_lock_file_creation() {
    let api = FakeAcl::new();
    api.temp_length_override.store(0, Ordering::Relaxed);
    let error = lock_file_path(&api, Path::new("C:\\Repo")).unwrap_err();
    assert_eq!(error.api, "GetTempPathW");
    assert_eq!(error.win32_code, 5);
    assert!(api.call_names().is_empty());

    api.temp_length_override
        .store(abi::MAX_PATH + 2, Ordering::Relaxed);
    let error = lock_file_path(&api, Path::new("C:\\Repo")).unwrap_err();
    assert_eq!(error.api, "GetTempPathW");
    assert_eq!(error.win32_code, abi::ERROR_INSUFFICIENT_BUFFER);
    assert!(
        error
            .detail()
            .unwrap()
            .contains("buffer; nothing was written")
    );
    assert!(api.call_names().is_empty());
}

#[test]
fn path_lock_checks_every_call_and_never_masks_the_action_error() {
    let api = FakeAcl::new();
    api.create_handle.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(
        with_path_lock(&api, Path::new("C:\\locked"), || Ok(()))
            .unwrap_err()
            .api,
        "CreateFileW"
    );

    let api = FakeAcl::new();
    api.lock_ok.store(false, Ordering::Relaxed);
    assert_eq!(
        with_path_lock(&api, Path::new("C:\\locked"), || Ok(()))
            .unwrap_err()
            .api,
        "LockFileEx"
    );
    assert!(api.call_names().contains(&"close:7".into()));

    let api = FakeAcl::new();
    api.unlock_ok.store(false, Ordering::Relaxed);
    assert_eq!(
        with_path_lock(&api, Path::new("C:\\locked"), || Ok(()))
            .unwrap_err()
            .api,
        "UnlockFileEx"
    );

    let api = FakeAcl::new();
    api.close_ok.store(false, Ordering::Relaxed);
    assert_eq!(
        with_path_lock(&api, Path::new("C:\\locked"), || Ok(()))
            .unwrap_err()
            .api,
        "CloseHandle"
    );

    let api = FakeAcl::new();
    api.unlock_ok.store(false, Ordering::Relaxed);
    api.close_ok.store(false, Ordering::Relaxed);
    let error = Win32Error::new("action", 99, Some("original".into()));
    assert_eq!(
        with_path_lock(&api, Path::new("C:\\locked"), || Err::<(), _>(error))
            .unwrap_err()
            .api,
        "action"
    );
}

#[test]
fn grant_read_merge_apply_and_exact_ace_skip_preserve_cleanup_order() {
    let api = FakeAcl::new();
    let sid = [1, 1, 0, 0, 0, 0, 0, 5, 7, 0, 0, 0];
    grant_write(
        &api,
        Path::new("C:\\granted"),
        NativePointer::from_raw(42),
        &sid,
    )
    .unwrap();
    let calls = api.call_names();
    assert!(calls.iter().any(|call| call == "merge:1:0"));
    assert!(calls.iter().any(|call| call == "apply:C:\\granted:9"));
    assert!(calls.iter().any(|call| call == "free:9"));

    api.calls.lock().clear();
    *api.read.lock() = AclRead {
        code: 0,
        acl: Some(AclWithPointer {
            pointer: NativePointer::from_raw(11),
            bytes: exact_acl(&sid),
        }),
        descriptor: Some(NativePointer::from_raw(6)),
    };
    grant_write(
        &api,
        Path::new("C:\\granted"),
        NativePointer::from_raw(42),
        &sid,
    )
    .unwrap();
    let calls = api.call_names();
    assert!(calls.contains(&"free:6".into()));
    assert!(!calls.iter().any(|call| call.starts_with("merge:")));
    assert!(!calls.iter().any(|call| call.starts_with("apply:")));
}

#[test]
fn malformed_acl_falls_back_and_all_merge_apply_cleanup_failures_are_checked() {
    let sid = [1, 0, 0, 0, 0, 0, 0, 5];
    let api = FakeAcl::new();
    *api.read.lock() = AclRead {
        code: 0,
        acl: Some(AclWithPointer {
            pointer: NativePointer::from_raw(11),
            bytes: vec![0, 0, 4, 0, 1, 0, 0, 0],
        }),
        descriptor: Some(NativePointer::from_raw(6)),
    };
    grant_write(&api, Path::new("x"), NativePointer::from_raw(42), &sid).unwrap();
    assert!(api.call_names().iter().any(|call| call == "merge:1:11"));

    let api = FakeAcl::new();
    *api.read.lock() = AclRead {
        code: 0,
        acl: None,
        descriptor: Some(NativePointer::from_raw(6)),
    };
    *api.merge.lock() = SetEntriesResult { code: 5, acl: None };
    let error = grant_write(&api, Path::new("x"), NativePointer::from_raw(42), &sid).unwrap_err();
    assert_eq!(error.api, "SetEntriesInAclW");
    assert!(api.call_names().contains(&"free:6".into()));

    let api = FakeAcl::new();
    *api.merge.lock() = SetEntriesResult { code: 0, acl: None };
    assert_eq!(
        grant_write(&api, Path::new("x"), NativePointer::from_raw(42), &sid)
            .unwrap_err()
            .api,
        "SetEntriesInAclW"
    );

    let api = FakeAcl::new();
    api.apply_code.store(5, Ordering::Relaxed);
    assert_eq!(
        grant_write(&api, Path::new("x"), NativePointer::from_raw(42), &sid)
            .unwrap_err()
            .api,
        "SetNamedSecurityInfoW"
    );
    assert!(api.call_names().contains(&"free:9".into()));
}

#[test]
fn revoke_no_dacl_and_grant_disposal_match_standing_revocable_ownership() {
    let api = Arc::new(FakeAcl::new());
    assert!(!revoke_write(api.as_ref(), Path::new("x"), NativePointer::from_raw(42)).unwrap());
    *api.read.lock() = AclRead {
        code: 0,
        acl: None,
        descriptor: Some(NativePointer::from_raw(6)),
    };
    assert!(!revoke_write(api.as_ref(), Path::new("x"), NativePointer::from_raw(42)).unwrap());
    assert!(api.call_names().contains(&"free:6".into()));

    *api.read.lock() = AclRead {
        code: 0,
        acl: None,
        descriptor: None,
    };
    let binding: Arc<dyn GrantBindings> = api.clone();
    let mut grant = AclWriteGrant::create("S-1-4-42-42", binding).unwrap();
    grant.add(Path::new("C:\\revocable"), false).unwrap();
    grant.add(Path::new("C:\\standing"), true).unwrap();
    assert_eq!(
        grant.paths(),
        [
            PathBuf::from("C:\\standing"),
            PathBuf::from("C:\\revocable")
        ]
    );
    grant.dispose().unwrap();
    let calls = api.call_names();
    assert!(calls.iter().any(|call| call == "read:C:\\revocable"));
    let standing_reads = calls
        .iter()
        .filter(|call| call.as_str() == "read:C:\\standing")
        .count();
    assert_eq!(standing_reads, 1, "standing path is read only during add");
    assert!(calls.contains(&"free:42".into()));
}

#[test]
fn grant_dispose_aggregates_revocation_and_sid_free_failures() {
    let api = Arc::new(FakeAcl::new());
    let binding: Arc<dyn GrantBindings> = api.clone();
    let mut grant = AclWriteGrant::create("S-1-4-42-42", binding).unwrap();
    grant.add(Path::new("C:\\granted"), false).unwrap();
    api.read.lock().code = 2;
    api.frees.lock().push_back(NativePointer::from_raw(1));
    let error = grant.dispose().unwrap_err();
    assert_eq!(error.failures.len(), 2);
    assert_eq!(error.failures[0].api, "GetNamedSecurityInfoW");
    assert_eq!(error.failures[1].api, "LocalFree");
}
