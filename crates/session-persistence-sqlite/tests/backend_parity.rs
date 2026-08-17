//! Service, durability, repair, and lifecycle contracts for the `SQLite` backend.

use std::{path::Path, sync::Arc};

use rusqlite::Connection;
use seekdeep_cordis::Context;
use seekdeep_cordis::Fiber;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_session_persistence_sqlite::{
    JournalMode, SqliteConfig, SqliteSessionPersistence, install, invariant::register_invariant,
};
use serde_json::json;

fn mount(path: &Path) -> (Arc<SqliteSessionPersistence>, Arc<SessionStore>, Context) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let backend =
        SqliteSessionPersistence::new(sessions.clone(), SqliteConfig::new(path)).expect("backend");
    (backend, sessions, context)
}

fn balanced_session(id: &str) -> Arc<Session> {
    let session = Session::create(&SessionId::new(id), None, None).expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    session
}

#[tokio::test]
async fn create_is_lazy_and_first_append_atomically_materializes() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session = balanced_session("lazy");

    backend.create(session.header()).await.expect("create");
    assert!(backend.list(None).await.expect("list").is_empty());
    backend
        .append(session.id(), &session.events())
        .await
        .expect("append");
    assert_eq!(
        backend
            .list(None)
            .await
            .expect("list")
            .into_iter()
            .map(|header| header.id)
            .collect::<Vec<_>>(),
        [session.id().clone()]
    );
    assert_eq!(
        backend
            .inspect(session.id(), None)
            .await
            .expect("inspect")
            .events,
        session.events()
    );
    context.fiber().dispose().await.expect("dispose");

    let (reopened, _, reopened_context) = mount(&path);
    assert_eq!(
        reopened.load(session.id()).await.expect("reopen").events,
        session.events()
    );
    reopened_context
        .fiber()
        .dispose()
        .await
        .expect("dispose reopened");
}

#[tokio::test]
async fn inspect_balances_without_mutation_and_load_commits_the_repair() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (writer, _, writer_context) = mount(&path);
    let session = Session::create(&SessionId::new("open-turn"), None, None).expect("session");
    let start = session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    writer.create(session.header()).await.expect("create");
    writer.append(session.id(), &[start]).await.expect("append");
    writer_context
        .fiber()
        .dispose()
        .await
        .expect("dispose writer");

    let (reader, _, reader_context) = mount(&path);
    let inspected = reader.inspect(session.id(), None).await.expect("inspect");
    assert_eq!(inspected.events.len(), 2);
    assert_eq!(physical_event_count(&path, session.id()), 1);
    let loaded = reader.load(session.id()).await.expect("load repair");
    assert_eq!(loaded.events, inspected.events);
    assert_eq!(physical_event_count(&path, session.id()), 2);
    reader_context
        .fiber()
        .dispose()
        .await
        .expect("dispose reader");
}

#[tokio::test]
async fn append_transaction_rolls_back_a_cross_connection_sequence_collision() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let session = balanced_session("collision");
    let (first, _, first_context) = mount(&path);
    first.create(session.header()).await.expect("create");
    first
        .append(session.id(), &session.events())
        .await
        .expect("first turn");

    let (second, _, second_context) = mount(&path);
    second.load(session.id()).await.expect("adopt cursor");
    let continuation = Session::create(
        session.id(),
        Some(session.events()),
        Some(session.header().clone()),
    )
    .expect("continuation");
    continuation
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .expect("turn 2 start");
    continuation
        .append(
            "turn/end",
            json!({"turn": 2, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn 2 end");
    let tail = continuation.events()[2..].to_vec();
    first.append(session.id(), &tail).await.expect("winner");
    let error = second
        .append(session.id(), &tail)
        .await
        .expect_err("unique collision");
    assert!(error.to_string().contains("UNIQUE"));
    assert_eq!(
        physical_event_count(&path, session.id()),
        i64::try_from(session.events().len() + tail.len()).expect("event count fits i64")
    );
    first_context
        .fiber()
        .dispose()
        .await
        .expect("first dispose");
    second_context
        .fiber()
        .dispose()
        .await
        .expect("second dispose");
}

#[tokio::test]
async fn plugin_publishes_tracks_live_events_drains_and_withdraws() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mounted = install(
        &context,
        SqliteConfig {
            path: path.clone(),
            journal_mode: JournalMode::Wal,
            prepared_session_cache_size: 5,
            write_batch_max_delay_ms: 60_000,
        },
    )
    .expect("plugin");
    mounted.await_settled().await.expect("active");
    let persistence = context.get(SESSION_PERSISTENCE).expect("service");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("plugin-live")),
            CreateSessionOptions::default(),
        )
        .expect("live session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");

    mounted.dispose().await.expect("drain");
    assert!(context.get(SESSION_PERSISTENCE).is_none());
    assert_eq!(physical_event_count(&path, session.id()), 2);
    drop(persistence);
}

#[tokio::test]
async fn revisions_are_stable_across_reopen_and_change_on_mutation() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let session = balanced_session("revision");
    let (first, _, first_context) = mount(&path);
    first.create(session.header()).await.expect("create");
    first
        .append(session.id(), &session.events())
        .await
        .expect("append");
    let before = first
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    first_context.fiber().dispose().await.expect("dispose");

    let (second, _, second_context) = mount(&path);
    let reopened = second
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    assert_eq!(reopened, before);
    second
        .commit_repair(session.header(), None, &[])
        .await
        .expect("empty repair");
    assert_eq!(
        second
            .list_snapshots(None)
            .await
            .expect("snapshot")
            .remove(0)
            .revision,
        before
    );
    second_context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn append_rejects_first_sequence_and_mid_batch_gaps_without_materializing() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session = balanced_session("gaps");
    backend.create(session.header()).await.expect("create");
    let mut wrong_first = session.events();
    wrong_first[0].seq = 1;
    assert!(
        backend
            .append(session.id(), &wrong_first)
            .await
            .expect_err("first gap")
            .to_string()
            .contains("must begin at seq 0")
    );
    let mut middle = session.events();
    middle[1].seq = 2;
    assert!(
        backend
            .append(session.id(), &middle)
            .await
            .expect_err("middle gap")
            .to_string()
            .contains("sequence gap")
    );
    assert!(backend.list(None).await.expect("still lazy").is_empty());
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn read_from_is_a_non_mutating_exact_suffix() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session = balanced_session("suffix");
    backend.create(session.header()).await.expect("create");
    backend
        .append(session.id(), &session.events())
        .await
        .expect("append");
    let revision = backend
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    assert_eq!(
        backend
            .read_from(session.id(), 1, None)
            .await
            .expect("suffix")
            .events,
        session.events()[1..]
    );
    assert!(
        backend
            .read_from(session.id(), 99, None)
            .await
            .expect("past end")
            .events
            .is_empty()
    );
    assert_eq!(
        backend
            .list_snapshots(None)
            .await
            .expect("snapshot")
            .remove(0)
            .revision,
        revision
    );
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn lightweight_revision_changes_after_each_mutating_transaction() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session = balanced_session("revision-change");
    backend.create(session.header()).await.expect("create");
    backend
        .append(session.id(), &session.events()[..1])
        .await
        .expect("first append");
    let first = backend
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    backend
        .append(session.id(), &session.events()[1..])
        .await
        .expect("second append");
    let second = backend
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    assert_ne!(first, second);
    assert!(first.as_str().ends_with(":revision:1"));
    assert!(second.as_str().ends_with(":revision:2"));
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn missing_and_duplicate_session_id_failures_are_eager_and_stable() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let missing = SessionId::new("missing");
    assert!(
        backend
            .load(&missing)
            .await
            .expect_err("missing load")
            .to_string()
            .contains("was not found")
    );
    assert!(
        backend
            .inspect(&missing, None)
            .await
            .expect_err("missing inspect")
            .to_string()
            .contains("was not found")
    );
    let session = balanced_session("duplicate");
    backend.create(session.header()).await.expect("create");
    assert!(
        backend
            .create(session.header())
            .await
            .expect_err("in-memory duplicate")
            .to_string()
            .contains("already exists")
    );
    backend
        .append(session.id(), &session.events())
        .await
        .expect("materialize");
    context.fiber().dispose().await.expect("dispose");

    let (reopened, _, reopened_context) = mount(&path);
    assert!(
        reopened
            .create(session.header())
            .await
            .expect_err("stored duplicate")
            .to_string()
            .contains("persisted log")
    );
    reopened_context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn stored_format_version_refusal_names_upgrade_direction() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let session = balanced_session("future-format");
    let (writer, _, writer_context) = mount(&path);
    writer.create(session.header()).await.expect("create");
    writer
        .append(session.id(), &session.events())
        .await
        .expect("append");
    writer_context.fiber().dispose().await.expect("dispose");
    let database = Connection::open(&path).expect("probe");
    database
        .execute(
            "UPDATE sessions SET version = 1 WHERE id = ?1",
            [session.id().as_str()],
        )
        .expect("future version");
    drop(database);

    let (reader, _, reader_context) = mount(&path);
    let error = reader.load(session.id()).await.expect_err("future refusal");
    assert!(error.to_string().contains("newer harness"));
    assert!(error.to_string().contains("upgrade the harness"));
    reader_context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn required_unknown_events_refuse_but_ignorable_unknown_events_load() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let session = balanced_session("unknown");
    let (writer, _, writer_context) = mount(&path);
    writer.create(session.header()).await.expect("create");
    writer
        .append(session.id(), &session.events())
        .await
        .expect("append");
    writer_context.fiber().dispose().await.expect("dispose");
    let database = Connection::open(&path).expect("probe");
    database
        .execute(
            "INSERT INTO events (session_id, seq, type, time, data) VALUES (?1, 2, 'future/required', 3, '{}')",
            [session.id().as_str()],
        )
        .expect("unknown");
    drop(database);

    let (reader, _, reader_context) = mount(&path);
    let error = reader
        .inspect(session.id(), None)
        .await
        .expect_err("required unknown");
    assert!(error.to_string().contains("not marked ignorable"));
    reader_context.fiber().dispose().await.expect("dispose");

    let database = Connection::open(&path).expect("probe");
    database
        .execute(
            "UPDATE events SET ignorable = 1 WHERE session_id = ?1 AND seq = 2",
            [session.id().as_str()],
        )
        .expect("mark ignorable");
    drop(database);
    let (reader, _, reader_context) = mount(&path);
    assert_eq!(
        reader
            .inspect(session.id(), None)
            .await
            .expect("ignorable")
            .events
            .len(),
        3
    );
    reader_context.fiber().dispose().await.expect("dispose");
}

#[cfg(unix)]
#[tokio::test]
async fn new_database_is_owner_only_and_existing_mode_is_preserved() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("owner.sqlite");
    let (backend, _, context) = mount(&path);
    backend.list(None).await.expect("initialize");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(format!("{}-wal", path.display()))
            .expect("wal metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(format!("{}-shm", path.display()))
            .expect("shm metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    context.fiber().dispose().await.expect("dispose");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("change mode");
    let (reopened, _, reopened_context) = mount(&path);
    reopened.list(None).await.expect("reopen");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    reopened_context.fiber().dispose().await.expect("dispose");
}

#[cfg(unix)]
#[tokio::test]
async fn persistent_rollback_journal_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("persist.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let backend = SqliteSessionPersistence::new(
        sessions,
        SqliteConfig {
            path: path.clone(),
            journal_mode: JournalMode::Persist,
            prepared_session_cache_size: 5,
            write_batch_max_delay_ms: 200,
        },
    )
    .expect("backend");
    let session = balanced_session("journal-mode");
    backend.create(session.header()).await.expect("create");
    backend
        .append(session.id(), &session.events())
        .await
        .expect("append");
    assert_eq!(
        std::fs::metadata(format!("{}-journal", path.display()))
            .expect("journal metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn explained_empty_invariant_reserves_and_releases_package_identity() {
    let context = Context::new();
    let invariants = Arc::new(
        seekdeep_invariants::InvariantRegistry::new(
            &context,
            &seekdeep_invariants::InvariantConfig::default(),
        )
        .expect("registry"),
    );
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("ready");
    assert!(register_invariant(&invariants).is_err());
    registration.dispose().await.expect("dispose");
    register_invariant(&invariants)
        .expect("replacement")
        .await_ready()
        .await
        .expect("replacement ready");
}

#[tokio::test]
async fn disposed_unmaterialized_lifecycle_releases_its_id_before_immediate_reuse() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mounted = install(
        &context,
        SqliteConfig {
            path,
            journal_mode: JournalMode::Wal,
            prepared_session_cache_size: 5,
            write_batch_max_delay_ms: 60_000,
        },
    )
    .expect("plugin");
    mounted.await_settled().await.expect("active");

    let first_fiber = Fiber::active_child("first session");
    let first_owner = context.with_fiber(first_fiber.clone());
    sessions
        .create(
            &first_owner,
            Some(SessionId::new("reusable")),
            CreateSessionOptions::default(),
        )
        .expect("first");
    first_fiber.dispose().await.expect("dispose first");

    let second_fiber = Fiber::active_child("second session");
    let second_owner = context.with_fiber(second_fiber.clone());
    let second = sessions
        .create(
            &second_owner,
            Some(SessionId::new("reusable")),
            CreateSessionOptions::default(),
        )
        .expect("same id is reusable");
    second
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    second
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    sessions.flush(&second).await.expect("flush");
    assert_eq!(
        context
            .get(SESSION_PERSISTENCE)
            .expect("persistence")
            .persistence()
            .list(None)
            .await
            .expect("list")
            .len(),
        1
    );
    second_fiber.dispose().await.expect("dispose second");
    mounted.dispose().await.expect("dispose backend");
}

#[tokio::test]
async fn hot_reload_adopts_live_prefix_and_persists_only_the_live_suffix() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let config = SqliteConfig {
        path: path.clone(),
        journal_mode: JournalMode::Wal,
        prepared_session_cache_size: 5,
        write_batch_max_delay_ms: 60_000,
    };
    let first = install(&context, config.clone()).expect("first backend");
    first.await_settled().await.expect("first active");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("hmr")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start 1");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end 1");
    sessions.flush(&session).await.expect("first flush");
    first.dispose().await.expect("unmount first");
    assert_eq!(physical_event_count(&path, session.id()), 2);

    session
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .expect("open second turn while unmounted");
    let second = install(&context, config).expect("second backend");
    second.await_settled().await.expect("second active");
    sessions.flush(&session).await.expect("second flush");
    assert_eq!(physical_event_count(&path, session.id()), 3);

    let database = Connection::open(&path).expect("probe");
    let event_types = database
        .prepare("SELECT type FROM events WHERE session_id = ?1 ORDER BY seq")
        .expect("statement")
        .query_map([session.id().as_str()], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("types");
    assert_eq!(event_types, ["turn/start", "turn/end", "turn/start"]);
    drop(database);
    second.dispose().await.expect("unmount second");
}

fn physical_event_count(path: &Path, id: &SessionId) -> i64 {
    let database = Connection::open(path).expect("probe");
    database
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .expect("count")
}
