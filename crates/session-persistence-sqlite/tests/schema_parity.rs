//! Differentially ported schema and row-scanning contracts.

use rusqlite::Connection;
use seekdeep_core::session::{SessionOrigin, SurfaceOp};
use seekdeep_session_persistence_sqlite::schema::{
    EventRow, JournalMode, SCHEMA_VERSION, SESSION_PERSISTENCE_SQLITE_APPLICATION_ID, SessionRow,
    open_database, row_to_event, row_to_meta, scan_rows, store_identity,
};
use serde_json::json;

fn event_row(seq: i64, event_type: &str) -> EventRow {
    EventRow {
        seq,
        event_type: event_type.to_owned(),
        time: 123,
        data: "{}".to_owned(),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn session_row() -> SessionRow {
    SessionRow {
        id: "session-1".to_owned(),
        version: 0,
        created_at: 123,
        cwd: None,
        parent_session: None,
        seed_length: None,
        origin: None,
        incarnation: "incarnation".to_owned(),
        revision: 0,
        delegation_depth: None,
        agent_preset: None,
    }
}

#[test]
fn initializes_and_stamps_an_empty_database() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let database = open_database(&path, JournalMode::Wal).expect("open");
    assert_eq!(
        database
            .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .expect("version"),
        SCHEMA_VERSION
    );
    assert_eq!(
        database
            .pragma_query_value::<i64, _>(None, "application_id", |row| row.get(0))
            .expect("application id"),
        SESSION_PERSISTENCE_SQLITE_APPLICATION_ID
    );
    assert!(
        !store_identity(&database, &path)
            .expect("identity")
            .is_empty()
    );
}

#[test]
fn refuses_foreign_unversioned_objects_without_stamping() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("foreign.sqlite");
    let database = Connection::open(&path).expect("seed");
    database
        .execute_batch("CREATE TABLE foreign_table (id INTEGER)")
        .expect("foreign table");
    drop(database);

    let error = open_database(&path, JournalMode::Wal).expect_err("reject foreign");
    assert!(error.to_string().contains("unversioned schema"));
    let database = Connection::open(&path).expect("reopen");
    assert_eq!(
        database
            .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .expect("version"),
        0
    );
    let persistence_table: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'persistence_state'",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(persistence_table, 0);
}

#[test]
fn failed_initialization_rolls_back_schema_objects_without_changing_identity() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("conflict.sqlite");
    let database = Connection::open(&path).expect("seed");
    database
        .pragma_update(
            None,
            "application_id",
            SESSION_PERSISTENCE_SQLITE_APPLICATION_ID,
        )
        .expect("application id");
    database
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .expect("user version");
    database
        .execute_batch(
            "CREATE VIEW persistence_state AS SELECT 1 AS singleton, 'foreign' AS store_id",
        )
        .expect("conflicting view");
    drop(database);

    assert!(open_database(&path, JournalMode::Wal).is_err());
    let unchanged = Connection::open(&path).expect("inspect unchanged");
    let object_type: String = unchanged
        .query_row(
            "SELECT type FROM sqlite_schema WHERE name = 'persistence_state'",
            [],
            |row| row.get(0),
        )
        .expect("view remains");
    assert_eq!(object_type, "view");
    for table in ["sessions", "events"] {
        let count: i64 = unchanged
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                [table],
                |row| row.get(0),
            )
            .expect("object count");
        assert_eq!(count, 0, "{table}");
    }
    assert_eq!(
        unchanged
            .pragma_query_value::<i64, _>(None, "application_id", |row| row.get(0))
            .expect("application id"),
        SESSION_PERSISTENCE_SQLITE_APPLICATION_ID
    );
    assert_eq!(
        unchanged
            .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .expect("user version"),
        SCHEMA_VERSION
    );
    let journal: String = unchanged
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal, "delete");
}

#[test]
fn sqlite_prefixed_user_table_is_not_mistaken_for_metadata() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("foreign.sqlite");
    let database = Connection::open(&path).expect("seed");
    database
        .execute_batch("CREATE TABLE sqliteX (id INTEGER)")
        .expect("sqliteX is legal");
    drop(database);
    assert!(
        open_database(&path, JournalMode::Wal)
            .expect_err("foreign object")
            .to_string()
            .contains("unversioned schema")
    );
}

#[test]
fn refuses_both_older_and_newer_schema_versions() {
    for version in [SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let temporary = tempfile::tempdir().expect("tempdir");
        let path = temporary.path().join(format!("v{version}.sqlite"));
        let database = Connection::open(&path).expect("seed");
        database
            .pragma_update(None, "user_version", version)
            .expect("set version");
        drop(database);
        let error = open_database(&path, JournalMode::Delete).expect_err("version refusal");
        assert!(
            error
                .to_string()
                .contains(&format!("schema version {version}"))
        );
    }
}

#[test]
fn refuses_current_schema_with_foreign_application_identity() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("foreign-app.sqlite");
    let database = Connection::open(&path).expect("seed");
    database
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .expect("version");
    database
        .pragma_update(None, "application_id", 123_i64)
        .expect("application id");
    drop(database);
    let error = open_database(&path, JournalMode::Wal).expect_err("application refusal");
    assert!(error.to_string().contains("application id 123"));
}

#[test]
fn preserves_a_complete_log_and_an_interrupted_contiguous_turn() {
    let complete = vec![event_row(0, "turn/start"), event_row(1, "turn/end")];
    let (events, torn) = scan_rows(&complete, 0).expect("complete");
    assert_eq!(events.len(), 2);
    assert_eq!(torn, None);

    let interrupted = vec![
        event_row(0, "turn/start"),
        event_row(1, "turn/end"),
        event_row(2, "turn/start"),
        event_row(3, "message"),
    ];
    let (events, torn) = scan_rows(&interrupted, 0).expect("interrupted");
    assert_eq!(events.len(), 4);
    assert_eq!(torn, None);
}

#[test]
fn marks_a_gap_after_the_last_boundary_as_a_torn_tail() {
    let rows = vec![
        event_row(0, "turn/start"),
        event_row(1, "turn/end"),
        event_row(3, "message"),
    ];
    let (events, torn) = scan_rows(&rows, 0).expect("tail gap");
    assert_eq!(events.len(), 2);
    assert_eq!(torn, Some(2));
}

#[test]
fn rejects_a_gap_or_unparsable_row_in_the_committed_region() {
    let gap = vec![event_row(0, "turn/start"), event_row(2, "turn/end")];
    assert!(
        scan_rows(&gap, 0)
            .expect_err("gap")
            .to_string()
            .contains("seq gap")
    );

    let mut malformed = event_row(0, "turn/start");
    malformed.data = "{".to_owned();
    let rows = vec![malformed, event_row(1, "turn/end")];
    assert!(
        scan_rows(&rows, 0)
            .expect_err("malformed")
            .to_string()
            .contains("unparsable committed event")
    );
}

#[test]
fn tolerates_an_unparsable_uncommitted_tail() {
    let mut malformed = event_row(2, "message");
    malformed.data = "{".to_owned();
    let rows = vec![
        event_row(0, "turn/start"),
        event_row(1, "turn/end"),
        malformed,
    ];
    let (events, torn) = scan_rows(&rows, 0).expect("torn tail");
    assert_eq!(events.len(), 2);
    assert_eq!(torn, Some(2));
}

#[test]
fn suffix_scan_uses_the_requested_base() {
    let rows = vec![event_row(7, "message"), event_row(8, "turn/end")];
    let (events, torn) = scan_rows(&rows, 7).expect("suffix");
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        [7, 8]
    );
    assert_eq!(torn, None);
}

#[test]
fn row_reconstruction_restores_every_optional_header_field() {
    let mut row = session_row();
    row.cwd = Some("/workspace".to_owned());
    row.parent_session = Some("parent".to_owned());
    row.seed_length = Some(4);
    row.origin = Some("subagent".to_owned());
    row.delegation_depth = Some(2);
    row.agent_preset = Some("research".to_owned());
    let header = row_to_meta(&row).expect("header");
    assert_eq!(header.cwd.as_deref(), Some("/workspace"));
    assert_eq!(
        header
            .parent_session
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("parent")
    );
    assert_eq!(header.seed_length, Some(4));
    assert_eq!(header.origin, Some(SessionOrigin::Subagent));
    assert_eq!(header.delegation_depth, Some(2));
    assert_eq!(header.agent_preset.as_deref(), Some("research"));
}

#[test]
fn row_reconstruction_rejects_invalid_creation_metadata() {
    let mut row = session_row();
    row.created_at = -1;
    assert_eq!(
        row_to_meta(&row).expect_err("negative").to_string(),
        "stored session createdAt must be a non-negative safe integer"
    );
}

#[test]
fn event_row_round_trips_all_envelope_columns() {
    let row = EventRow {
        seq: 9,
        event_type: "message/user".to_owned(),
        time: 456,
        data: json!({"message": "hello"}).to_string(),
        source_event_seqs: Some(json!([1, 3]).to_string()),
        surface_op: Some(json!({"op": "replace", "start": 4, "end": 6}).to_string()),
        ignorable: Some(1),
    };
    let event = row_to_event(&row).expect("event");
    assert_eq!(event.source_event_seqs, Some(vec![1, 3]));
    assert_eq!(event.surface_op, Some(SurfaceOp::replace(4, 6)));
    assert_eq!(event.ignorable, Some(true));
    assert_eq!(event.data, json!({"message": "hello"}));
}

#[test]
fn only_integer_one_restores_the_ignorable_marker() {
    let mut row = event_row(0, "plugin/event");
    row.ignorable = Some(2);
    assert_eq!(row_to_event(&row).expect("event").ignorable, None);
    row.ignorable = Some(1);
    assert_eq!(row_to_event(&row).expect("event").ignorable, Some(true));
}

#[test]
fn empty_store_identity_is_rejected() {
    let database = open_database(std::path::Path::new(":memory:"), JournalMode::Wal).expect("open");
    database
        .execute("UPDATE persistence_state SET store_id = ''", [])
        .expect("corrupt identity");
    let error =
        store_identity(&database, std::path::Path::new(":memory:")).expect_err("missing identity");
    assert!(error.to_string().contains("no valid store identity"));
}
