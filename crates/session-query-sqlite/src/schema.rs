//! Safe opening and schema ownership for the disposable `SQLite` search index.

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::Connection;

/// Current derived-index schema version.
pub const SESSION_QUERY_SQLITE_SCHEMA_VERSION: i64 = 8;
/// `SQLite` application id protecting unrelated databases from derived resets.
pub const SESSION_QUERY_SQLITE_APPLICATION_ID: i64 = 0x4453_4851;

const DERIVED_USER_TABLES: &[&str] = &[
    "persisted_docs",
    "persisted_docs_config",
    "persisted_docs_content",
    "persisted_docs_data",
    "persisted_docs_docsize",
    "persisted_docs_idx",
    "persisted_sessions",
    "search_state",
];

/// Supported `SQLite` journal modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalMode {
    /// Write-ahead log.
    Wal,
    /// Rollback journal removed after commit.
    Delete,
    /// Rollback journal truncated after commit.
    Truncate,
    /// Rollback journal retained after commit.
    Persist,
}

impl JournalMode {
    const fn sql(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
        }
    }
}

/// Opens, validates, and initializes persistent and connection-local schemas.
///
/// # Errors
///
/// Returns filesystem, `SQLite`, foreign-database, or unrecognized-table failures.
pub fn open_search_database(path: &str, journal_mode: JournalMode) -> anyhow::Result<Connection> {
    let actual = if path == ":memory:" {
        PathBuf::from(path)
    } else {
        std::path::absolute(path)?
    };
    if path != ":memory:" {
        create_database_file(&actual)?;
    }
    let connection = Connection::open(if path == ":memory:" {
        Path::new(":memory:")
    } else {
        &actual
    })?;
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let tables = list_user_tables(&connection)?;
    let display = actual.display();
    anyhow::ensure!(
        application_id == 0 || application_id == SESSION_QUERY_SQLITE_APPLICATION_ID,
        "session-search database at \"{display}\" belongs to another application"
    );
    anyhow::ensure!(
        application_id != 0 || tables.is_empty(),
        "session-search database at \"{display}\" is not an empty or recognized derived index"
    );
    if application_id == SESSION_QUERY_SQLITE_APPLICATION_ID {
        let unknown = tables
            .iter()
            .filter(|table| !DERIVED_USER_TABLES.contains(&table.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            unknown.is_empty(),
            "session-search database at \"{display}\" has unrecognized user tables: {}",
            unknown.join(", ")
        );
        if version != SESSION_QUERY_SQLITE_SCHEMA_VERSION {
            reset_derived_schema(&connection, &tables)?;
        }
    }
    connection.execute_batch(&format!("PRAGMA journal_mode = {}", journal_mode.sql()))?;
    ensure_persistent_schema(&connection)?;
    ensure_temporary_schema(&connection)?;
    Ok(connection)
}

fn create_database_file(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn list_user_tables(connection: &Connection) -> anyhow::Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT GLOB 'sqlite_*' ORDER BY name",
    )?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?)
}

fn reset_derived_schema(connection: &Connection, tables: &[String]) -> anyhow::Result<()> {
    for table in tables {
        connection.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote_identifier(table)))?;
    }
    connection.execute_batch("PRAGMA user_version = 0")?;
    Ok(())
}

fn ensure_persistent_schema(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(&format!(
        r"
        PRAGMA application_id = {SESSION_QUERY_SQLITE_APPLICATION_ID};
        CREATE TABLE IF NOT EXISTS search_state (
          singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
          global_generation INTEGER NOT NULL
        ) STRICT;
        INSERT OR IGNORE INTO search_state (singleton, global_generation) VALUES (1, 0);
        CREATE TABLE IF NOT EXISTS persisted_sessions (
          id TEXT PRIMARY KEY,
          version INTEGER NOT NULL,
          created_at INTEGER NOT NULL,
          cwd TEXT,
          parent_session TEXT,
          seed_length INTEGER,
          delegation_depth INTEGER,
          agent_preset TEXT,
          revision TEXT NOT NULL,
          generation INTEGER NOT NULL
        ) STRICT;
        CREATE VIRTUAL TABLE IF NOT EXISTS persisted_docs USING fts5(
          text,
          session_id UNINDEXED,
          seq UNINDEXED,
          type UNINDEXED,
          time UNINDEXED,
          surface UNINDEXED,
          codepoint_length UNINDEXED,
          tokenize = 'unicode61'
        );
        PRAGMA user_version = {SESSION_QUERY_SQLITE_SCHEMA_VERSION};
        "
    ))?;
    Ok(())
}

fn ensure_temporary_schema(connection: &Connection) -> anyhow::Result<()> {
    connection.execute_batch(
        r"
        CREATE TEMP TABLE IF NOT EXISTS live_sessions (
          id TEXT PRIMARY KEY,
          version INTEGER NOT NULL,
          created_at INTEGER NOT NULL,
          cwd TEXT,
          parent_session TEXT,
          seed_length INTEGER,
          delegation_depth INTEGER,
          agent_preset TEXT,
          fingerprint TEXT NOT NULL,
          persisted INTEGER NOT NULL CHECK (persisted IN (0, 1)),
          generation INTEGER NOT NULL
        ) STRICT;
        CREATE VIRTUAL TABLE IF NOT EXISTS temp.live_docs USING fts5(
          text,
          session_id UNINDEXED,
          seq UNINDEXED,
          type UNINDEXED,
          time UNINDEXED,
          surface UNINDEXED,
          codepoint_length UNINDEXED,
          tokenize = 'unicode61'
        );
        ",
    )?;
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
