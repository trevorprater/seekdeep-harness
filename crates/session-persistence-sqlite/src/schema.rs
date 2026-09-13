//! SQLite schema and stored-row reconstruction.

use std::path::Path;

use anyhow::Context as _;
use rusqlite::{Connection, OptionalExtension as _, params};
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId, SessionOrigin, SurfaceOp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// On-disk table-layout version.
pub const SCHEMA_VERSION: i64 = 15;
/// `SQLite` application id protecting unrelated databases from persistence writes.
pub const SESSION_PERSISTENCE_SQLITE_APPLICATION_ID: i64 = 0x4453_4850;

/// Durability-preserving `SQLite` journal modes accepted by the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JournalMode {
    /// Write-ahead log (default).
    #[default]
    Wal,
    /// Delete rollback journals after commit.
    Delete,
    /// Truncate rollback journals after commit.
    Truncate,
    /// Retain rollback journals after commit.
    Persist,
}

impl JournalMode {
    const fn pragma(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
        }
    }
}

/// One materialized session metadata row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// Session id.
    pub id: String,
    /// Event vocabulary version.
    pub version: i64,
    /// Unix epoch milliseconds.
    pub created_at: i64,
    /// Working directory.
    pub cwd: Option<String>,
    /// Parent id.
    pub parent_session: Option<String>,
    /// Seed boundary.
    pub seed_length: Option<i64>,
    /// Origin marker.
    pub origin: Option<String>,
    /// Stable materialization identity.
    pub incarnation: String,
    /// Monotonic mutation revision.
    pub revision: i64,
    /// Delegation depth.
    pub delegation_depth: Option<i64>,
    /// Composition preset.
    pub agent_preset: Option<String>,
}

/// One physical event row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    /// Event sequence.
    pub seq: i64,
    /// Extensible event type.
    pub event_type: String,
    /// Unix epoch milliseconds.
    pub time: i64,
    /// Event payload JSON text.
    pub data: String,
    /// Source event sequences JSON text.
    pub source_event_seqs: Option<String>,
    /// Surface operation JSON text.
    pub surface_op: Option<String>,
    /// True marker encoded as integer one.
    pub ignorable: Option<i64>,
}

/// Opens, validates, initializes, and configures one session database.
///
/// # Errors
///
/// Rejects non-owned schemas, foreign application ids, `SQLite` failures, and
/// unsupported journal modes returned by the medium.
pub fn open_database(path: &Path, journal_mode: JournalMode) -> anyhow::Result<Connection> {
    let mut database = Connection::open(path)?;
    if let Err(error) = configure_database(&mut database, path, journal_mode) {
        let _ = database.close();
        return Err(error);
    }
    Ok(database)
}

fn configure_database(
    database: &mut Connection,
    path: &Path,
    journal_mode: JournalMode,
) -> anyhow::Result<()> {
    database.pragma_update(None, "foreign_keys", true)?;
    let transaction =
        database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let on_disk: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application_id: i64 =
        transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_object_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?;
    let display = path.display();
    if on_disk == 0 && (application_id != 0 || user_object_count > 0) {
        anyhow::bail!(
            "session database at \"{display}\" has an unversioned schema or application identity"
        );
    }
    if on_disk != 0 && on_disk != SCHEMA_VERSION {
        anyhow::bail!(
            "session database at \"{display}\" has schema version {on_disk}, incompatible with this build ({SCHEMA_VERSION})"
        );
    }
    if on_disk == SCHEMA_VERSION && application_id != SESSION_PERSISTENCE_SQLITE_APPLICATION_ID {
        anyhow::bail!(
            "session database at \"{display}\" has application id {application_id}, expected {SESSION_PERSISTENCE_SQLITE_APPLICATION_ID}"
        );
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS persistence_state (
           singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
           store_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS sessions (
           id TEXT PRIMARY KEY,
           version INTEGER NOT NULL,
           created_at INTEGER NOT NULL,
           cwd TEXT,
           parent_session TEXT,
           seed_length INTEGER,
           origin TEXT,
           delegation_depth INTEGER,
           agent_preset TEXT,
           incarnation TEXT NOT NULL,
           revision INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS events (
           session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
           seq INTEGER NOT NULL,
           type TEXT NOT NULL,
           time INTEGER NOT NULL,
           data TEXT NOT NULL,
           source_event_seqs TEXT,
           surface_op TEXT,
           ignorable INTEGER,
           PRIMARY KEY (session_id, seq)
         ) STRICT;",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO persistence_state (singleton, store_id) VALUES (1, ?1)",
        [Uuid::new_v4().to_string()],
    )?;
    if on_disk == 0 {
        transaction.pragma_update(
            None,
            "application_id",
            SESSION_PERSISTENCE_SQLITE_APPLICATION_ID,
        )?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    transaction.commit()?;

    let selected: String = database.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    database.pragma_update(None, "journal_mode", journal_mode.pragma())?;
    let actual: String = database.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(journal_mode.pragma())
            || (path == Path::new(":memory:") && actual.eq_ignore_ascii_case("memory")),
        "SQLite refused journal mode {} (was {selected}, became {actual})",
        journal_mode.pragma()
    );
    Ok(())
}

/// Reads the singleton store identity.
///
/// # Errors
///
/// Rejects a missing or empty identity and propagates `SQLite` failures.
pub fn store_identity(database: &Connection, path: &Path) -> anyhow::Result<String> {
    let identity = database
        .query_row(
            "SELECT store_id FROM persistence_state WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let identity = identity
        .filter(|value| !value.is_empty())
        .with_context(|| {
            format!(
                "session database at \"{}\" has no valid store identity",
                path.display()
            )
        })?;
    Ok(identity)
}

/// Reconstructs a session header from a physical row.
///
/// # Errors
///
/// Rejects negative, out-of-range, or foreign closed-enum fields.
pub fn row_to_meta(row: &SessionRow) -> anyhow::Result<SessionHeader> {
    let created_at = u64::try_from(row.created_at)
        .ok()
        .filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or_else(|| {
            anyhow::anyhow!("stored session createdAt must be a non-negative safe integer")
        })?;
    let version = u32::try_from(row.version).context("stored session version must be a uint32")?;
    let seed_length = optional_nonnegative(row.seed_length, "seedLength")?;
    let delegation_depth = optional_nonnegative(row.delegation_depth, "delegationDepth")?;
    let origin = match row.origin.as_deref() {
        None => None,
        Some("subagent") => Some(SessionOrigin::Subagent),
        Some(value) => anyhow::bail!("stored session origin has unknown value {value:?}"),
    };
    Ok(SessionHeader {
        version,
        id: SessionId::new(row.id.clone()),
        created_at,
        cwd: row.cwd.clone(),
        parent_session: row.parent_session.clone().map(SessionId::new),
        seed_length,
        origin,
        delegation_depth,
        agent_preset: row.agent_preset.clone(),
    })
}

fn optional_nonnegative(value: Option<i64>, field: &str) -> anyhow::Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value)
                .ok()
                .filter(|value| *value <= 9_007_199_254_740_991)
                .ok_or_else(|| {
                    anyhow::anyhow!("stored session {field} must be a non-negative safe integer")
                })
        })
        .transpose()
}

/// Reconstructs one event from its physical row.
///
/// # Errors
///
/// Rejects invalid JSON or a negative sequence.
pub fn row_to_event(row: &EventRow) -> anyhow::Result<SessionEvent> {
    Ok(SessionEvent {
        event_type: row.event_type.clone(),
        seq: u64::try_from(row.seq).context("stored session event seq must be non-negative")?,
        time: row.time,
        data: serde_json::from_str(&row.data)?,
        source_event_seqs: row
            .source_event_seqs
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
        surface_op: row
            .surface_op
            .as_deref()
            .map(serde_json::from_str::<SurfaceOp>)
            .transpose()?,
        ignorable: (row.ignorable == Some(1)).then_some(true),
    })
}

/// Finds the valid contiguous prefix and the first torn-tail sequence.
///
/// # Errors
///
/// A hole through the last committed `turn/end` is corruption.
pub fn scan_rows(rows: &[EventRow], base: u64) -> anyhow::Result<(Vec<SessionEvent>, Option<u64>)> {
    let parsed: Vec<Option<SessionEvent>> = rows.iter().map(|row| row_to_event(row).ok()).collect();
    let last_turn_end = parsed.iter().enumerate().rev().find_map(|(index, event)| {
        (event.is_some() && rows[index].event_type == "turn/end").then_some(index)
    });
    let mut preserved = Vec::new();
    for (index, event) in parsed.into_iter().enumerate() {
        let index_u64 = u64::try_from(index).context("stored session row count exceeds u64")?;
        let expected = base
            .checked_add(index_u64)
            .ok_or_else(|| anyhow::anyhow!("stored session event sequence overflow"))?;
        let Some(event) = event else {
            if last_turn_end.is_some_and(|boundary| index <= boundary) {
                anyhow::bail!(
                    "corrupt session log: unparsable committed event at seq {}",
                    rows[index].seq
                );
            }
            break;
        };
        if event.seq != expected {
            if last_turn_end.is_some_and(|boundary| index <= boundary) {
                anyhow::bail!(
                    "corrupt session log: seq gap in committed region (expected {expected}, got {})",
                    event.seq
                );
            }
            break;
        }
        preserved.push(event);
    }
    let preserved_len = u64::try_from(preserved.len())
        .context("stored session preserved event count exceeds u64")?;
    let torn_from = if preserved.len() < rows.len() {
        Some(
            base.checked_add(preserved_len)
                .ok_or_else(|| anyhow::anyhow!("stored session event sequence overflow"))?,
        )
    } else {
        None
    };
    Ok((preserved, torn_from))
}

/// Reads one complete session row by id.
///
/// # Errors
///
/// Propagates `SQLite` decoding failures.
pub fn session_row(database: &Connection, id: &SessionId) -> anyhow::Result<Option<SessionRow>> {
    database
        .query_row(
            "SELECT id, version, created_at, cwd, parent_session, seed_length, origin, incarnation, revision, delegation_depth, agent_preset FROM sessions WHERE id = ?1",
            params![id.as_str()],
            |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    version: row.get(1)?,
                    created_at: row.get(2)?,
                    cwd: row.get(3)?,
                    parent_session: row.get(4)?,
                    seed_length: row.get(5)?,
                    origin: row.get(6)?,
                    incarnation: row.get(7)?,
                    revision: row.get(8)?,
                    delegation_depth: row.get(9)?,
                    agent_preset: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}
