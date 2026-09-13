//! Cross-platform Win32 namespace policy and mocked native-call parity.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_session_persistence_jsonl::win32::*;

struct FilesystemMove {
    seen: Arc<Mutex<Vec<(PathBuf, PathBuf, u32)>>>,
}

impl Win32Move for FilesystemMove {
    fn move_new(
        &self,
        existing: &Path,
        replacement: &Path,
        flags: u32,
    ) -> Result<(), Win32NamespaceError> {
        self.seen
            .lock()
            .push((existing.to_path_buf(), replacement.to_path_buf(), flags));
        if !existing.exists() {
            return Err(win32_error(2, existing, replacement));
        }
        if replacement.exists() {
            return Err(win32_error(183, existing, replacement));
        }
        fs::rename(existing, replacement).map_err(|_| win32_error(9999, existing, replacement))
    }
}

struct ErrorMove(u32);

impl Win32Move for ErrorMove {
    fn move_new(
        &self,
        existing: &Path,
        replacement: &Path,
        _flags: u32,
    ) -> Result<(), Win32NamespaceError> {
        Err(win32_error(self.0, existing, replacement))
    }
}

#[test]
fn publishes_new_file_with_write_through_and_no_replace_fallback() {
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("log.tmp");
    let final_path = root.path().join("log.jsonl");
    fs::write(&staging, "content").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    publish_new_file_with(
        &FilesystemMove { seen: seen.clone() },
        &staging,
        &final_path,
    )
    .unwrap();
    assert!(!staging.exists());
    assert_eq!(fs::read_to_string(final_path).unwrap(), "content");
    assert_eq!(seen.lock()[0].2, MOVEFILE_WRITE_THROUGH);
}

#[test]
fn maps_every_source_win32_code_and_retains_paths() {
    for (raw, expected) in [
        (2, Win32Errno::NoEntry),
        (3, Win32Errno::NoEntry),
        (5, Win32Errno::Access),
        (17, Win32Errno::CrossDevice),
        (80, Win32Errno::Exists),
        (183, Win32Errno::Exists),
        (123, Win32Errno::Invalid),
        (9999, Win32Errno::Io),
    ] {
        let error =
            publish_new_file_with(&ErrorMove(raw), Path::new("from"), Path::new("to")).unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(error.win32_code, raw);
        assert_eq!(error.path, Path::new("from"));
        assert_eq!(error.dest, Path::new("to"));
    }
}

#[test]
fn creates_missing_ancestors_with_short_staging_siblings_and_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("a/b");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mover = FilesystemMove { seen: seen.clone() };
    ensure_durable_directory_with(&mover, &target).unwrap();
    ensure_durable_directory_with(&mover, &target).unwrap();
    assert!(target.is_dir());
    assert_eq!(seen.lock().len(), 2);
    assert!(seen.lock().iter().all(|(staging, _, flags)| {
        *flags == MOVEFILE_WRITE_THROUGH
            && staging
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".seekdeep-mkdir-")
    }));

    let longest = root.path().join("x".repeat(255));
    ensure_durable_directory_with(&mover, &longest).unwrap();
    assert!(longest.is_dir());
}

struct RaceMove {
    target: PathBuf,
}

impl Win32Move for RaceMove {
    fn move_new(
        &self,
        existing: &Path,
        replacement: &Path,
        _flags: u32,
    ) -> Result<(), Win32NamespaceError> {
        if replacement == self.target {
            fs::create_dir(replacement).unwrap();
            return Err(win32_error(183, existing, replacement));
        }
        fs::rename(existing, replacement).map_err(|_| win32_error(9999, existing, replacement))
    }
}

#[test]
fn accepts_only_an_existing_directory_race() {
    let root = tempfile::tempdir().unwrap();
    let raced = root.path().join("raced");
    ensure_durable_directory_with(
        &RaceMove {
            target: raced.clone(),
        },
        &raced,
    )
    .unwrap();
    assert!(raced.is_dir());

    let denied = root.path().join("denied");
    let error = ensure_durable_directory_with(&ErrorMove(5), &denied).unwrap_err();
    assert_eq!(
        error.downcast_ref::<Win32NamespaceError>().unwrap().code,
        Win32Errno::Access
    );
}

#[test]
fn rejects_a_non_directory_component() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("blocked");
    fs::write(&blocked, "x").unwrap();
    let error = ensure_durable_directory_with(
        &FilesystemMove {
            seen: Arc::new(Mutex::new(Vec::new())),
        },
        &blocked.join("child"),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotADirectory
    );
}
