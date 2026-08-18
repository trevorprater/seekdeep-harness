//! Service, durability, repair, and lifecycle contracts for the `SQLite` backend.

use std::{path::Path, sync::Arc};

use rusqlite::Connection;
use seekdeep_cordis::{Context, EventArgs, Fiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, MessageSource, UserMessage};
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionPersistence, SessionPersistenceAborted,
};
use seekdeep_session_persistence_sqlite::{
    JournalMode, SqliteConfig, SqliteSessionPersistence, install, invariant::register_invariant,
    schema::open_database,
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
async fn session_disposal_drains_buffered_events_before_retiring_ownership() {
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

    let first_fiber = Fiber::active_child("buffered owner");
    let first_owner = context.with_fiber(first_fiber.clone());
    let first = sessions
        .create(
            &first_owner,
            Some(SessionId::new("buffered-retirement")),
            CreateSessionOptions::default(),
        )
        .expect("first session");
    first
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    first
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    first_fiber.dispose().await.expect("dispose first owner");

    let persistence = context
        .get(SESSION_PERSISTENCE)
        .expect("persistence")
        .persistence();
    let successor_fiber = Fiber::active_child("colliding successor");
    let successor_owner = context.with_fiber(successor_fiber.clone());
    let successor = sessions
        .create(
            &successor_owner,
            Some(first.id().clone()),
            CreateSessionOptions::default(),
        )
        .expect("successor session");
    successor
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("successor event");
    let collision = sessions
        .flush(&successor)
        .await
        .expect_err("persisted id collision");
    assert!(
        collision.to_string().contains("persisted log")
            || collision.to_string().contains("collision"),
        "{collision:#}"
    );
    successor_fiber.dispose().await.expect("dispose successor");
    assert_eq!(
        persistence
            .load(first.id())
            .await
            .expect("load drained log")
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    mounted.dispose().await.expect("dispose backend");
}

#[tokio::test]
async fn backend_teardown_retries_failed_session_retirement_before_close() {
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

    let owner_fiber = Fiber::active_child("retrying retirement owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let session = sessions
        .create(
            &owner,
            Some(SessionId::new("retry-retirement")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    sessions
        .flush(&session)
        .await
        .expect("initialize lazy owner");

    let lock = Connection::open(&path).expect("external lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold SQLite writer lock");
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
    owner_fiber.dispose().await.expect("dispose owner");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    lock.execute_batch("ROLLBACK").expect("release writer lock");
    drop(lock);

    mounted
        .dispose()
        .await
        .expect("teardown retries retirement");
    assert_eq!(physical_event_count(&path, session.id()), 2);
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

#[tokio::test]
async fn repeated_created_notification_is_idempotent_for_the_exact_live_session() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mounted = install(&context, SqliteConfig::new(&path)).expect("backend");
    mounted.await_settled().await.expect("active");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("idempotent-init")),
            CreateSessionOptions::default(),
        )
        .expect("session");
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
    sessions.flush(&session).await.expect("first flush");
    context
        .events()
        .emit(
            &context,
            "session/created",
            &EventArgs::from_values(vec![session.clone()]),
        )
        .expect("repeat created");
    sessions.flush(&session).await.expect("second flush");
    assert_eq!(physical_event_count(&path, session.id()), 2);
    mounted.dispose().await.expect("dispose");
}

#[test]
fn public_configuration_defaults_and_schema_version_match_the_source() {
    let config = SqliteConfig::new("sessions.sqlite");
    assert_eq!(config.journal_mode, JournalMode::Wal);
    assert_eq!(config.prepared_session_cache_size, 5);
    assert_eq!(config.write_batch_max_delay_ms, 200);
    assert_eq!(seekdeep_session_persistence_sqlite::SCHEMA_VERSION, 15);
}

#[test]
fn every_configured_journal_mode_reaches_the_database() {
    for (mode, expected) in [
        (JournalMode::Wal, "wal"),
        (JournalMode::Delete, "delete"),
        (JournalMode::Truncate, "truncate"),
        (JournalMode::Persist, "persist"),
    ] {
        let temporary = tempfile::tempdir().expect("tempdir");
        let path = temporary.path().join(format!("{expected}.sqlite"));
        let database = open_database(&path, mode).expect("configured database");
        let actual: String = database
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode");
        assert_eq!(actual.to_ascii_lowercase(), expected);
    }
}

#[tokio::test]
async fn recreated_session_identity_changes_even_when_revision_restarts_at_one() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let session = balanced_session("recreated-revision");
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
    drop(first);
    first_context
        .fiber()
        .dispose()
        .await
        .expect("dispose first");

    let database = Connection::open(&path).expect("delete probe");
    database
        .execute(
            "DELETE FROM sessions WHERE id = ?1",
            [session.id().as_str()],
        )
        .expect("delete session");
    drop(database);

    let (second, _, second_context) = mount(&path);
    second.create(session.header()).await.expect("recreate");
    second
        .append(session.id(), &session.events())
        .await
        .expect("reappend");
    let after = second
        .list_snapshots(None)
        .await
        .expect("snapshot")
        .remove(0)
        .revision;
    assert_ne!(before, after);
    assert!(before.as_str().ends_with(":revision:1"));
    assert!(after.as_str().ends_with(":revision:1"));
    second_context
        .fiber()
        .dispose()
        .await
        .expect("dispose second");
}

#[tokio::test]
async fn append_and_load_round_trip_surface_columns_through_sqlite() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session =
        Session::create(&SessionId::new("surface-roundtrip"), None, None).expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("start");
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(Vec::new(), MessageSource::user()))
                .expect("user message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                source_event_seqs: None,
                ignorable: false,
            },
        )
        .expect("user message");
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(Vec::new(), MessageSource::user()))
                .expect("second user message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                source_event_seqs: Some(vec![1]),
                ignorable: false,
            },
        )
        .expect("second user message");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("end");
    backend.create(session.header()).await.expect("create");
    backend
        .append(session.id(), &session.events())
        .await
        .expect("append");
    let loaded = backend.load(session.id()).await.expect("load");
    assert_eq!(loaded.events[1].surface_op, Some(SurfaceOp::append()));
    assert_eq!(loaded.events[1].source_event_seqs, None);
    assert_eq!(loaded.events[2].surface_op, Some(SurfaceOp::append()));
    assert_eq!(loaded.events[2].source_event_seqs, Some(vec![1]));
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn invalid_database_parent_and_preaborted_snapshot_list_fail_early() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let parent_file = temporary.path().join("not-a-directory");
    std::fs::write(&parent_file, b"file").expect("parent file");
    let invalid_path = parent_file.join("sessions.sqlite");
    let (invalid, _, invalid_context) = mount(&invalid_path);
    assert!(invalid.list(None).await.is_err());
    invalid_context
        .fiber()
        .dispose()
        .await
        .expect("dispose invalid");

    let valid_path = temporary.path().join("valid.sqlite");
    let (valid, _, valid_context) = mount(&valid_path);
    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("snapshot cancelled"));
    let cancelled = valid
        .list_snapshots(Some(signal))
        .await
        .expect_err("cancelled");
    assert_eq!(
        cancelled
            .downcast_ref::<SessionPersistenceAborted>()
            .expect("typed cancellation")
            .reason,
        json!("snapshot cancelled")
    );
    valid_context
        .fiber()
        .dispose()
        .await
        .expect("dispose valid");
}

#[tokio::test]
async fn fresh_backend_append_adopts_storage_only_session_and_continues_sequence() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let first_turn = balanced_session("adopt-append");
    let (first, _, first_context) = mount(&path);
    first.create(first_turn.header()).await.expect("create");
    first
        .append(first_turn.id(), &first_turn.events())
        .await
        .expect("first append");
    drop(first);
    first_context
        .fiber()
        .dispose()
        .await
        .expect("dispose first");

    let second_turn = [
        seekdeep_core::session::SessionEvent {
            event_type: "turn/start".to_owned(),
            seq: 2,
            time: 7,
            data: json!({"turn": 2}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        seekdeep_core::session::SessionEvent {
            event_type: "turn/end".to_owned(),
            seq: 3,
            time: 8,
            data: json!({"turn": 2, "reason": {"kind": "completed"}}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
    ];
    let (second, _, second_context) = mount(&path);
    second
        .append(first_turn.id(), &second_turn)
        .await
        .expect("adopted append");
    assert_eq!(
        second
            .load(first_turn.id())
            .await
            .expect("load")
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    second_context
        .fiber()
        .dispose()
        .await
        .expect("dispose second");
}

#[tokio::test]
async fn empty_append_keeps_created_session_lazy_and_unlisted() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let (backend, _, context) = mount(&path);
    let session = balanced_session("empty-batch");
    backend.create(session.header()).await.expect("create");
    backend
        .append(session.id(), &[])
        .await
        .expect("empty append");
    assert!(
        backend
            .list(None)
            .await
            .expect("list")
            .iter()
            .all(|header| header.id != *session.id())
    );
    context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn ownerless_state_rejects_fresh_reuse_and_cwd_mismatch() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mounted = install(&context, SqliteConfig::new(&path)).expect("plugin");
    mounted.await_settled().await.expect("active");
    let persistence = context
        .get(SESSION_PERSISTENCE)
        .expect("persistence")
        .persistence();

    let lazy_id = SessionId::new("wrong-cwd-claim");
    let mut lazy_header = SessionHeader::new(lazy_id.clone());
    lazy_header.cwd = Some("/other".to_owned());
    persistence.create(&lazy_header).await.expect("lazy create");
    let lazy_seed = balanced_session("lazy-seed").events();
    let live = sessions
        .create(
            &context,
            Some(lazy_id),
            CreateSessionOptions {
                seed: Some(lazy_seed),
                cwd: Some("/work".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .expect("live cwd mismatch");
    let error = sessions.flush(&live).await.expect_err("cwd collision");
    assert!(
        error.to_string().contains("different cwd") || error.to_string().contains("collision"),
        "{error:#}"
    );

    let stored = balanced_session("ownerless-loaded");
    let mut stored_header = stored.header().clone();
    stored_header.cwd = Some("/work".to_owned());
    persistence
        .create(&stored_header)
        .await
        .expect("stored create");
    persistence
        .append(stored.id(), &stored.events())
        .await
        .expect("stored append");
    persistence.load(stored.id()).await.expect("ownerless load");
    let fresh = sessions
        .create(
            &context,
            Some(stored.id().clone()),
            CreateSessionOptions {
                cwd: Some("/work".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .expect("fresh live collision");
    fresh
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("fresh event");
    assert!(sessions.flush(&fresh).await.is_err());
    mounted.dispose().await.expect("dispose");
}

#[tokio::test]
async fn matching_ownerless_prefix_is_claimed_and_only_seed_suffix_is_appended() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.sqlite");
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mounted = install(&context, SqliteConfig::new(&path)).expect("plugin");
    mounted.await_settled().await.expect("active");
    let persistence = context
        .get(SESSION_PERSISTENCE)
        .expect("persistence")
        .persistence();

    let stored = balanced_session("claim-prefix");
    let mut header = stored.header().clone();
    header.cwd = Some("/work".to_owned());
    persistence.create(&header).await.expect("create");
    persistence
        .append(stored.id(), &stored.events())
        .await
        .expect("append");
    let loaded = persistence.load(stored.id()).await.expect("load");
    let live = sessions
        .create(
            &context,
            Some(stored.id().clone()),
            CreateSessionOptions {
                seed: Some(loaded.events.clone()),
                cwd: loaded.meta.cwd.clone(),
                created_at: Some(loaded.meta.created_at),
                ..CreateSessionOptions::default()
            },
        )
        .expect("matching live session");
    sessions.flush(&live).await.expect("claim and flush");
    let claimed = persistence.load(stored.id()).await.expect("claimed load");
    assert_eq!(claimed.meta, loaded.meta);
    assert_eq!(
        claimed.events[..loaded.events.len()],
        loaded.events,
        "stored prefix changed"
    );
    assert_eq!(
        claimed.events.last().unwrap().event_type,
        "session/end-seed"
    );
    mounted.dispose().await.expect("dispose");
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
