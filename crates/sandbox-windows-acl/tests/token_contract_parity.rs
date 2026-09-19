//! Portable injected-binding parity for the restricted-token pipeline.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use parking_lot::Mutex;
use seekdeep_sandbox_windows_acl::{
    AclSandboxMode, NativeHandle, NativePointer, RestrictingSidSet, SetEntriesResult,
    TokenBindings, TokenError, TokenSid, abi, create_restricted_token, find_logon_sid,
    make_well_known_sid, open_current_process_token, set_token_default_dacl_grant,
};

struct FakeToken {
    last_error: AtomicU32,
    process: AtomicU64,
    open_token_ok: AtomicBool,
    opened_token: AtomicU64,
    close_ok: AtomicBool,
    groups_needed: AtomicU32,
    groups_second_ok: AtomicBool,
    group_count: AtomicU32,
    group_sid: AtomicU64,
    group_attributes: AtomicU32,
    sid_length: AtomicU32,
    copy_ok: AtomicBool,
    well_known_ok: AtomicBool,
    valid_sid: AtomicBool,
    default_needed: AtomicU32,
    default_second_ok: AtomicBool,
    default_dacl: AtomicU64,
    merge: Mutex<SetEntriesResult>,
    set_token_ok: AtomicBool,
    restricted_ok: AtomicBool,
    restricted_token: AtomicU64,
    restricted_sids: Mutex<Vec<NativePointer>>,
    frees: Mutex<Vec<NativePointer>>,
    closes: Mutex<Vec<NativeHandle>>,
}

impl FakeToken {
    fn new() -> Self {
        Self {
            last_error: AtomicU32::new(5),
            process: AtomicU64::new(7),
            open_token_ok: AtomicBool::new(true),
            opened_token: AtomicU64::new(9),
            close_ok: AtomicBool::new(true),
            groups_needed: AtomicU32::new(24),
            groups_second_ok: AtomicBool::new(true),
            group_count: AtomicU32::new(1),
            group_sid: AtomicU64::new(77),
            group_attributes: AtomicU32::new(abi::SE_GROUP_LOGON_ID),
            sid_length: AtomicU32::new(12),
            copy_ok: AtomicBool::new(true),
            well_known_ok: AtomicBool::new(true),
            valid_sid: AtomicBool::new(true),
            default_needed: AtomicU32::new(8),
            default_second_ok: AtomicBool::new(true),
            default_dacl: AtomicU64::new(88),
            merge: Mutex::new(SetEntriesResult {
                code: 0,
                acl: Some(NativePointer::from_raw(99)),
            }),
            set_token_ok: AtomicBool::new(true),
            restricted_ok: AtomicBool::new(true),
            restricted_token: AtomicU64::new(101),
            restricted_sids: Mutex::new(Vec::new()),
            frees: Mutex::new(Vec::new()),
            closes: Mutex::new(Vec::new()),
        }
    }
}

impl TokenBindings for FakeToken {
    fn last_error(&self) -> u32 {
        self.last_error.load(Ordering::Relaxed)
    }

    fn format_message(&self, _code: u32) -> String {
        String::new()
    }

    fn open_process(&self, _desired_access: u32, _inherit: bool, _pid: u32) -> NativeHandle {
        NativeHandle::from_raw(self.process.load(Ordering::Relaxed))
    }

    fn open_process_token(
        &self,
        _process: NativeHandle,
        _desired_access: u32,
    ) -> (bool, Option<NativeHandle>) {
        let raw = self.opened_token.load(Ordering::Relaxed);
        (
            self.open_token_ok.load(Ordering::Relaxed),
            (raw != 0).then(|| NativeHandle::from_raw(raw)),
        )
    }

    fn close_handle(&self, handle: NativeHandle) -> bool {
        self.closes.lock().push(handle);
        self.close_ok.load(Ordering::Relaxed)
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
                *needed = self.groups_needed.load(Ordering::Relaxed);
                let Some(buffer) = buffer else {
                    return false;
                };
                if !self.groups_second_ok.load(Ordering::Relaxed) {
                    return false;
                }
                buffer[0..4]
                    .copy_from_slice(&self.group_count.load(Ordering::Relaxed).to_le_bytes());
                if self.group_count.load(Ordering::Relaxed) > 0 && buffer.len() >= 24 {
                    buffer[8..16]
                        .copy_from_slice(&self.group_sid.load(Ordering::Relaxed).to_le_bytes());
                    buffer[16..20].copy_from_slice(
                        &self.group_attributes.load(Ordering::Relaxed).to_le_bytes(),
                    );
                }
                true
            }
            abi::TOKEN_DEFAULT_DACL => {
                *needed = self.default_needed.load(Ordering::Relaxed);
                let Some(buffer) = buffer else {
                    return false;
                };
                if !self.default_second_ok.load(Ordering::Relaxed) {
                    return false;
                }
                buffer[0..8]
                    .copy_from_slice(&self.default_dacl.load(Ordering::Relaxed).to_le_bytes());
                true
            }
            _ => panic!("unexpected token information class {class}"),
        }
    }

    fn get_length_sid(&self, _sid: NativePointer) -> u32 {
        self.sid_length.load(Ordering::Relaxed)
    }

    fn copy_sid(&self, destination: &mut [u8], _source: NativePointer) -> bool {
        if self.copy_ok.load(Ordering::Relaxed) {
            destination.fill(0x5a);
            true
        } else {
            false
        }
    }

    fn create_well_known_sid(&self, _kind: u32, sid: &mut [u8], size: &mut u32) -> bool {
        *size = 12;
        sid[..12].fill(0x33);
        self.well_known_ok.load(Ordering::Relaxed)
    }

    fn is_valid_sid(&self, _sid: &[u8]) -> bool {
        self.valid_sid.load(Ordering::Relaxed)
    }

    fn set_entries_in_acl(
        &self,
        _entry: &[u8; abi::EXPLICIT_ACCESS_W_SIZE],
        _old_acl: NativePointer,
    ) -> SetEntriesResult {
        *self.merge.lock()
    }

    fn set_token_information(&self, _token: NativeHandle, _class: u32, _info: &[u8]) -> bool {
        self.set_token_ok.load(Ordering::Relaxed)
    }

    fn local_free(&self, pointer: NativePointer) -> NativePointer {
        self.frees.lock().push(pointer);
        NativePointer::NULL
    }

    fn create_restricted_token(
        &self,
        _existing: NativeHandle,
        _flags: u32,
        restricting_sids: &[NativePointer],
    ) -> (bool, Option<NativeHandle>) {
        *self.restricted_sids.lock() = restricting_sids.to_vec();
        let raw = self.restricted_token.load(Ordering::Relaxed);
        (
            self.restricted_ok.load(Ordering::Relaxed),
            (raw != 0).then(|| NativeHandle::from_raw(raw)),
        )
    }
}

fn token() -> NativeHandle {
    NativeHandle::from_raw(9)
}

fn assert_win32_api(error: TokenError, expected: &str) {
    match error {
        TokenError::Win32(error) => assert_eq!(error.api, expected),
        other => panic!("expected Win32 error for {expected}, got {other}"),
    }
}

#[test]
fn process_token_open_is_fail_closed_and_closes_every_owned_process_handle() {
    let api = FakeToken::new();
    api.process.store(0, Ordering::Relaxed);
    assert_win32_api(
        open_current_process_token(&api, 123).unwrap_err(),
        "OpenProcess",
    );

    let api = FakeToken::new();
    api.open_token_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        open_current_process_token(&api, 123).unwrap_err(),
        "OpenProcessToken",
    );
    assert_eq!(api.closes.lock().as_slice(), &[NativeHandle::from_raw(7)]);

    let api = FakeToken::new();
    api.close_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        open_current_process_token(&api, 123).unwrap_err(),
        "CloseHandle",
    );

    let api = FakeToken::new();
    api.opened_token.store(0, Ordering::Relaxed);
    assert_win32_api(
        open_current_process_token(&api, 123).unwrap_err(),
        "OpenProcessToken",
    );

    let api = FakeToken::new();
    assert_eq!(open_current_process_token(&api, 123).unwrap(), token());
}

#[test]
fn logon_sid_scan_checks_probe_shape_group_attributes_length_and_copy() {
    let api = FakeToken::new();
    api.groups_needed.store(0, Ordering::Relaxed);
    assert_win32_api(
        find_logon_sid(&api, token()).unwrap_err(),
        "GetTokenInformation",
    );

    let api = FakeToken::new();
    api.groups_needed.store(4, Ordering::Relaxed);
    assert_win32_api(
        find_logon_sid(&api, token()).unwrap_err(),
        "GetTokenInformation",
    );

    let api = FakeToken::new();
    api.groups_second_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        find_logon_sid(&api, token()).unwrap_err(),
        "GetTokenInformation",
    );

    for (sid, attributes) in [(0, abi::SE_GROUP_LOGON_ID), (77, 0)] {
        let api = FakeToken::new();
        api.group_sid.store(sid, Ordering::Relaxed);
        api.group_attributes.store(attributes, Ordering::Relaxed);
        assert!(matches!(
            find_logon_sid(&api, token()).unwrap_err(),
            TokenError::NoLogonSid(1)
        ));
    }

    let api = FakeToken::new();
    api.sid_length.store(0, Ordering::Relaxed);
    assert_win32_api(find_logon_sid(&api, token()).unwrap_err(), "GetLengthSid");

    let api = FakeToken::new();
    api.copy_ok.store(false, Ordering::Relaxed);
    assert_win32_api(find_logon_sid(&api, token()).unwrap_err(), "CopySid");

    let api = FakeToken::new();
    let sid = find_logon_sid(&api, token()).unwrap();
    assert_eq!(sid.bytes(), &[0x5a; 12]);
}

#[test]
fn well_known_sid_creation_and_validation_are_checked() {
    let api = FakeToken::new();
    api.well_known_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        make_well_known_sid(&api, abi::WIN_WORLD_SID).unwrap_err(),
        "CreateWellKnownSid",
    );

    let api = FakeToken::new();
    api.valid_sid.store(false, Ordering::Relaxed);
    assert_win32_api(
        make_well_known_sid(&api, abi::WIN_WORLD_SID).unwrap_err(),
        "IsValidSid",
    );

    let api = FakeToken::new();
    assert_eq!(
        make_well_known_sid(&api, abi::WIN_WORLD_SID)
            .unwrap()
            .bytes()
            .len(),
        abi::SECURITY_MAX_SID_SIZE
    );
}

#[test]
fn default_dacl_merge_checks_every_stage_and_always_frees_the_merged_acl() {
    let api = FakeToken::new();
    api.default_needed.store(0, Ordering::Relaxed);
    assert_win32_api(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        "GetTokenInformation",
    );

    let api = FakeToken::new();
    api.default_second_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        "GetTokenInformation",
    );

    let api = FakeToken::new();
    api.default_dacl.store(0, Ordering::Relaxed);
    assert!(matches!(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        TokenError::NoDefaultDacl
    ));

    let api = FakeToken::new();
    *api.merge.lock() = SetEntriesResult { code: 5, acl: None };
    assert_win32_api(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        "SetEntriesInAclW",
    );

    let api = FakeToken::new();
    *api.merge.lock() = SetEntriesResult { code: 0, acl: None };
    assert_win32_api(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        "SetEntriesInAclW",
    );

    let api = FakeToken::new();
    api.set_token_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap_err(),
        "SetTokenInformation",
    );
    assert_eq!(api.frees.lock().as_slice(), &[NativePointer::from_raw(99)]);

    let api = FakeToken::new();
    set_token_default_dacl_grant(&api, token(), NativePointer::from_raw(77)).unwrap();
    assert_eq!(api.frees.lock().as_slice(), &[NativePointer::from_raw(99)]);
}

#[test]
fn restricting_lists_are_mode_exact_and_creation_is_fail_closed() {
    let api = FakeToken::new();
    let logon = TokenSid::zeroed(12);
    let known = RestrictingSidSet {
        world: TokenSid::zeroed(12),
    };
    assert_eq!(
        create_restricted_token(&api, token(), &logon, &[], &known, AclSandboxMode::ReadOnly,)
            .unwrap(),
        NativeHandle::from_raw(101)
    );
    assert_eq!(api.restricted_sids.lock().len(), 2);

    let write = NativePointer::from_raw(77);
    create_restricted_token(
        &api,
        token(),
        &logon,
        &[write],
        &known,
        AclSandboxMode::WorkspaceWrite,
    )
    .unwrap();
    assert_eq!(api.restricted_sids.lock().len(), 3);
    assert_eq!(api.restricted_sids.lock()[2], write);

    assert!(matches!(
        create_restricted_token(
            &api,
            token(),
            &logon,
            &[],
            &known,
            AclSandboxMode::WorkspaceWrite,
        )
        .unwrap_err(),
        TokenError::MissingWriteSid
    ));

    api.restricted_ok.store(false, Ordering::Relaxed);
    assert_win32_api(
        create_restricted_token(&api, token(), &logon, &[], &known, AclSandboxMode::ReadOnly)
            .unwrap_err(),
        "CreateRestrictedToken",
    );

    api.restricted_ok.store(true, Ordering::Relaxed);
    api.restricted_token.store(0, Ordering::Relaxed);
    assert_win32_api(
        create_restricted_token(&api, token(), &logon, &[], &known, AclSandboxMode::ReadOnly)
            .unwrap_err(),
        "CreateRestrictedToken",
    );
}
