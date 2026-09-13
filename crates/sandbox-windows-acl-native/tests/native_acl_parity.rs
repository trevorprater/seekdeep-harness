//! Real-Windows DACL and grant conformance against the source native suites.

#![cfg(windows)]

use std::{path::Path, sync::Arc};

use seekdeep_sandbox_windows_acl::{
    AclBindings, AclSandbox, AclSandboxMode, AclSandboxOptions, AclWriteGrant, GrantBindings,
    ParsedSid, Win32Error, WindowsAclBindings, abi, lock_file_path, revoke_write, with_path_lock,
};
use seekdeep_sandbox_windows_acl_native::WindowsBindings;

fn grant_binding() -> Arc<dyn GrantBindings> {
    Arc::new(WindowsBindings)
}

fn sandbox_binding() -> Arc<dyn WindowsAclBindings> {
    Arc::new(WindowsBindings)
}

fn parse_sid(api: &WindowsBindings, spelling: &str) -> ParsedSid {
    GrantBindings::convert_string_sid(api, spelling).expect("test SID must parse")
}

fn free_sid(api: &WindowsBindings, sid: ParsedSid) {
    assert!(
        AclBindings::local_free(api, sid.pointer).is_null(),
        "test SID allocation must be released"
    );
}

fn read_acl(api: &WindowsBindings, path: &Path) -> Vec<u8> {
    let read = AclBindings::read_current_dacl(api, path);
    assert_eq!(read.code, abi::ERROR_SUCCESS, "DACL read for {path:?}");
    let bytes = read.acl.map_or_else(Vec::new, |acl| acl.bytes);
    if let Some(descriptor) = read.descriptor {
        assert!(
            AclBindings::local_free(api, descriptor).is_null(),
            "DACL descriptor must be released"
        );
    }
    bytes
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

fn sid_length(bytes: &[u8]) -> Option<usize> {
    let count = usize::from(*bytes.get(1)?);
    (count <= usize::from(abi::SID_MAX_SUB_AUTHORITIES)).then_some(8 + count * 4)
}

fn exact_grant_count(acl: &[u8], sid: &[u8]) -> usize {
    let Some(acl_size) = read_u16(acl, 2).map(usize::from) else {
        return 0;
    };
    let Some(ace_count) = read_u16(acl, 4).map(usize::from) else {
        return 0;
    };
    if acl_size > acl.len() || acl_size < 8 {
        return 0;
    }
    let Some(expected_sid_length) = sid_length(sid) else {
        return 0;
    };
    let Some(expected_sid) = sid.get(..expected_sid_length) else {
        return 0;
    };
    let mut matches = 0;
    let mut offset = 8;
    for _ in 0..ace_count {
        let Some(entry_size) = read_u16(acl, offset + 2).map(usize::from) else {
            return 0;
        };
        let Some(end) = offset.checked_add(entry_size) else {
            return 0;
        };
        if entry_size < 8 || end > acl_size {
            return 0;
        }
        let entry_sid = acl.get(offset + 8..end).and_then(|candidate| {
            let length = sid_length(candidate)?;
            candidate.get(..length)
        });
        if acl.get(offset) == Some(&abi::ACCESS_ALLOWED_ACE_TYPE)
            && acl.get(offset + 1)
                == Some(&abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT.to_le_bytes()[0])
            && read_u32(acl, offset + 4) == Some(abi::GRANT_MASK)
            && entry_sid == Some(expected_sid)
        {
            matches += 1;
        }
        offset = end;
    }
    matches
}

fn count_for_sid(api: &WindowsBindings, path: &Path, spelling: &str) -> usize {
    let sid = parse_sid(api, spelling);
    let count = exact_grant_count(&read_acl(api, path), &sid.bytes);
    free_sid(api, sid);
    count
}

fn revoke_for_cleanup(api: &WindowsBindings, path: &Path, spelling: &str) {
    let sid = parse_sid(api, spelling);
    let _ = revoke_write(api, path, sid.pointer);
    let _ = AclBindings::local_free(api, sid.pointer);
}

#[test]
fn malformed_sid_fails_before_any_grant_owner_exists() {
    let error = AclWriteGrant::create("S-1-4-abc-1", grant_binding())
        .expect_err("malformed SID must fail closed");
    assert_eq!(error.api, "ConvertStringSidToSidW");
}

#[test]
fn grant_is_idempotent_and_dispose_splits_standing_from_revocable_paths() {
    let api = WindowsBindings;
    let revocable = tempfile::tempdir().unwrap();
    let standing = tempfile::tempdir().unwrap();
    let sid = "S-1-4-9000-77";

    let result = (|| {
        let mut grant = AclWriteGrant::create(sid, grant_binding())?;
        grant.add(revocable.path(), false)?;
        grant.add(standing.path(), true)?;
        assert_eq!(grant.paths(), [standing.path(), revocable.path()]);
        assert_eq!(count_for_sid(&api, revocable.path(), sid), 1);
        assert_eq!(count_for_sid(&api, standing.path(), sid), 1);

        grant.add(revocable.path(), false)?;
        grant.add(standing.path(), true)?;
        assert_eq!(count_for_sid(&api, revocable.path(), sid), 1);
        assert_eq!(count_for_sid(&api, standing.path(), sid), 1);
        grant.dispose().map_err(|error| {
            Win32Error::new("AclWriteGrant::dispose", 0, Some(error.to_string()))
        })?;
        assert_eq!(count_for_sid(&api, revocable.path(), sid), 0);
        assert_eq!(count_for_sid(&api, standing.path(), sid), 1);
        Ok::<_, Win32Error>(())
    })();

    revoke_for_cleanup(&api, standing.path(), sid);
    result.unwrap();
}

#[test]
fn independent_grants_preserve_each_other_during_revoke() {
    let api = WindowsBindings;
    let directory = tempfile::tempdir().unwrap();
    let sid_a = "S-1-4-9000-78";
    let sid_b = "S-1-4-9000-79";

    let mut grant_a = AclWriteGrant::create(sid_a, grant_binding()).unwrap();
    let mut grant_b = AclWriteGrant::create(sid_b, grant_binding()).unwrap();
    grant_a.add(directory.path(), false).unwrap();
    grant_b.add(directory.path(), false).unwrap();
    assert_eq!(count_for_sid(&api, directory.path(), sid_a), 1);
    assert_eq!(count_for_sid(&api, directory.path(), sid_b), 1);

    grant_a.dispose().unwrap();
    assert_eq!(count_for_sid(&api, directory.path(), sid_a), 0);
    assert_eq!(count_for_sid(&api, directory.path(), sid_b), 1);
    grant_b.dispose().unwrap();
    assert_eq!(count_for_sid(&api, directory.path(), sid_b), 0);
}

#[test]
fn path_lock_is_exclusive_and_action_failure_releases_it() {
    let api = WindowsBindings;
    let directory = tempfile::tempdir().unwrap();
    with_path_lock(&api, directory.path(), || Ok(())).unwrap();
    let lock_path = lock_file_path(&api, directory.path()).unwrap();
    let first = AclBindings::create_lock_file(&api, &lock_path);
    let second = AclBindings::create_lock_file(&api, &lock_path);
    assert!(!first.is_invalid());
    assert!(!second.is_invalid());
    assert!(AclBindings::lock_file(
        &api,
        first,
        abi::LOCKFILE_EXCLUSIVE_LOCK
    ));
    assert!(!AclBindings::lock_file(
        &api,
        second,
        abi::LOCKFILE_EXCLUSIVE_LOCK | abi::LOCKFILE_FAIL_IMMEDIATELY
    ));
    assert_eq!(AclBindings::last_error(&api), abi::ERROR_LOCK_VIOLATION);
    assert!(AclBindings::unlock_file(&api, first));
    assert!(AclBindings::lock_file(
        &api,
        second,
        abi::LOCKFILE_EXCLUSIVE_LOCK | abi::LOCKFILE_FAIL_IMMEDIATELY
    ));
    assert!(AclBindings::unlock_file(&api, second));
    assert!(AclBindings::close_handle(&api, first));
    assert!(AclBindings::close_handle(&api, second));

    let action_error = Win32Error::new("action", 0, Some("expected".into()));
    let error = with_path_lock(&api, directory.path(), || Err::<(), _>(action_error)).unwrap_err();
    assert_eq!(error.api, "action");
    let handle = AclBindings::create_lock_file(&api, &lock_path);
    assert!(AclBindings::lock_file(
        &api,
        handle,
        abi::LOCKFILE_EXCLUSIVE_LOCK | abi::LOCKFILE_FAIL_IMMEDIATELY
    ));
    assert!(AclBindings::unlock_file(&api, handle));
    assert!(AclBindings::close_handle(&api, handle));
    std::fs::remove_file(lock_path).unwrap();
}

fn workspace_options(
    directory: &Path,
    temp: Option<&Path>,
    write_sid: &str,
    temp_sid: Option<&str>,
) -> AclSandboxOptions {
    AclSandboxOptions {
        writable_dirs: vec![directory.to_owned()],
        temp_dir: temp.map(Path::to_owned),
        temp_was_explicit: true,
        write_sid: Some(write_sid.to_owned()),
        temp_write_sid: temp_sid.map(str::to_owned),
        mode: AclSandboxMode::WorkspaceWrite,
        manage_dacls: true,
    }
}

#[test]
fn sandbox_instances_preserve_standing_grants_and_revoke_private_temp() {
    let api = WindowsBindings;
    let workspace = tempfile::tempdir().unwrap();
    let private_temp = tempfile::tempdir().unwrap();
    let sid_a = "S-1-4-9000-1";
    let sid_b = "S-1-4-9000-2";
    let temp_sid = "S-1-4-9000-2-1";

    let result = (|| {
        let mut sandbox_a = AclSandbox::new(
            &workspace_options(workspace.path(), None, sid_a, None),
            sandbox_binding(),
        )?;
        let mut sandbox_b = AclSandbox::new(
            &workspace_options(
                workspace.path(),
                Some(private_temp.path()),
                sid_b,
                Some(temp_sid),
            ),
            sandbox_binding(),
        )?;
        sandbox_a.init(std::process::id())?;
        sandbox_b.init(std::process::id())?;
        sandbox_a.dispose()?;
        sandbox_b.dispose()?;

        assert_eq!(count_for_sid(&api, workspace.path(), sid_a), 1);
        assert_eq!(count_for_sid(&api, workspace.path(), sid_b), 1);
        assert_eq!(count_for_sid(&api, private_temp.path(), temp_sid), 0);
        Ok::<_, Box<dyn std::error::Error>>(())
    })();

    revoke_for_cleanup(&api, workspace.path(), sid_a);
    revoke_for_cleanup(&api, workspace.path(), sid_b);
    result.unwrap();
}

#[test]
fn overlapping_private_temp_fails_before_either_capability_is_applied() {
    let api = WindowsBindings;
    let workspace = tempfile::tempdir().unwrap();
    let nested_temp = workspace.path().join("temp");
    std::fs::create_dir(&nested_temp).unwrap();
    let write_sid = "S-1-4-9000-30";
    let temp_sid = "S-1-4-9000-30-1";
    let mut sandbox = AclSandbox::new(
        &workspace_options(
            workspace.path(),
            Some(&nested_temp),
            write_sid,
            Some(temp_sid),
        ),
        sandbox_binding(),
    )
    .unwrap();

    let error = sandbox.init(std::process::id()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("private temp directory must be disjoint")
    );
    assert_eq!(count_for_sid(&api, workspace.path(), write_sid), 0);
    assert_eq!(count_for_sid(&api, &nested_temp, temp_sid), 0);
}
