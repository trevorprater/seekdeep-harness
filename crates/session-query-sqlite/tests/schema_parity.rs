//! `SQLite` derived-schema ownership, reset, and filesystem parity.

use rusqlite::Connection;
use seekdeep_session_query_sqlite::schema::{
    JournalMode, SESSION_QUERY_SQLITE_APPLICATION_ID, SESSION_QUERY_SQLITE_SCHEMA_VERSION,
    open_search_database,
};

fn pragma(connection: &Connection, name: &str) -> i64 {
    connection
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

#[test]
fn opens_memory_with_persistent_and_connection_local_schemas() {
    let connection = open_search_database(":memory:", JournalMode::Wal).unwrap();
    assert_eq!(
        pragma(&connection, "application_id"),
        SESSION_QUERY_SQLITE_APPLICATION_ID
    );
    assert_eq!(
        pragma(&connection, "user_version"),
        SESSION_QUERY_SQLITE_SCHEMA_VERSION
    );
    let persistent: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('search_state','persisted_sessions','persisted_docs')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let temporary: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name IN ('live_sessions','live_docs')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persistent, 3);
    assert_eq!(temporary, 2);
}

#[test]
fn creates_owner_only_file_without_changing_existing_parent_mode() {
    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().join("existing");
    std::fs::create_dir(&parent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = parent.join("search.sqlite");
    open_search_database(path.to_str().unwrap(), JournalMode::Delete).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[cfg(unix)]
#[test]
fn creates_owner_only_wal_and_persistent_journal_sidecars_and_preserves_existing_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let wal_path = temporary.path().join("wal.sqlite");
    let wal = open_search_database(wal_path.to_str().unwrap(), JournalMode::Wal).unwrap();
    for path in [
        wal_path.clone(),
        wal_path.with_extension("sqlite-wal"),
        wal_path.with_extension("sqlite-shm"),
    ] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(wal);

    let persist_path = temporary.path().join("persist.sqlite");
    let persist =
        open_search_database(persist_path.to_str().unwrap(), JournalMode::Persist).unwrap();
    assert_eq!(
        std::fs::metadata(format!("{}-journal", persist_path.display()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(persist);

    let existing_path = temporary.path().join("existing.sqlite");
    std::fs::write(&existing_path, []).unwrap();
    std::fs::set_permissions(&existing_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let existing =
        open_search_database(existing_path.to_str().unwrap(), JournalMode::Delete).unwrap();
    assert_eq!(
        std::fs::metadata(&existing_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    drop(existing);
}

#[test]
fn refuses_foreign_or_unknown_databases_and_resets_recognized_versions() {
    let temporary = tempfile::tempdir().unwrap();
    let foreign = temporary.path().join("foreign.sqlite");
    {
        let connection = Connection::open(&foreign).unwrap();
        connection
            .execute_batch("PRAGMA application_id = 42")
            .unwrap();
    }
    assert!(
        open_search_database(foreign.to_str().unwrap(), JournalMode::Wal)
            .unwrap_err()
            .to_string()
            .contains("another application")
    );

    let unknown = temporary.path().join("unknown.sqlite");
    {
        let connection = Connection::open(&unknown).unwrap();
        connection
            .execute("CREATE TABLE business (id INTEGER)", [])
            .unwrap();
    }
    assert!(
        open_search_database(unknown.to_str().unwrap(), JournalMode::Wal)
            .unwrap_err()
            .to_string()
            .contains("not an empty or recognized")
    );

    let recognized = temporary.path().join("recognized.sqlite");
    {
        let connection =
            open_search_database(recognized.to_str().unwrap(), JournalMode::Wal).unwrap();
        connection
            .execute("INSERT INTO persisted_sessions (id,version,created_at,revision,generation) VALUES ('old',0,0,'r',1)", [])
            .unwrap();
        connection.execute_batch("PRAGMA user_version = 7").unwrap();
    }
    let connection =
        open_search_database(recognized.to_str().unwrap(), JournalMode::Persist).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM persisted_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        pragma(&connection, "user_version"),
        SESSION_QUERY_SQLITE_SCHEMA_VERSION
    );

    connection
        .execute("CREATE TABLE surprise (id INTEGER)", [])
        .unwrap();
    drop(connection);
    assert!(
        open_search_database(recognized.to_str().unwrap(), JournalMode::Wal)
            .unwrap_err()
            .to_string()
            .contains("unrecognized user tables: surprise")
    );
}
