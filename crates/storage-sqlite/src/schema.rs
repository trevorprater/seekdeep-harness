//! Physical schema and open-time configuration for `SQLite` storage.

use std::{fs::OpenOptions, path::PathBuf};

use rusqlite::Connection;
use seekdeep_storage::{StorageError, StorageErrorCode};
use serde::{Deserialize, Serialize};

/// Current physical database-layout version.
pub const STORAGE_SQLITE_SCHEMA_VERSION: u32 = 1;

/// Durable `SQLite` journal modes accepted by the source config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JournalMode {
    /// Write-ahead logging, suited to local disks.
    #[default]
    Wal,
    /// Delete the rollback journal after transactions.
    Delete,
    /// Truncate the rollback journal after transactions.
    Truncate,
    /// Retain and invalidate the rollback journal.
    Persist,
}

impl JournalMode {
    pub(crate) const fn pragma(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
        }
    }
}

/// Opens and completely configures a database, stamping its schema last.
///
/// # Errors
///
/// Preserves filesystem and `SQLite` failures and classifies an incompatible
/// physical schema as `version-mismatch`.
pub fn open_database(path: &str, journal_mode: JournalMode) -> anyhow::Result<Connection> {
    let (connection, diagnostic_path) = if path == ":memory:" {
        (Connection::open_in_memory()?, String::from(":memory:"))
    } else {
        let actual = absolute(path)?;
        let parent = actual
            .parent()
            .ok_or_else(|| anyhow::anyhow!("sqlite database path has no parent"))?;
        create_directory(parent)?;
        create_database_file(&actual)?;
        let diagnostic = actual.to_string_lossy().into_owned();
        (Connection::open(&actual)?, diagnostic)
    };
    if let Err(error) = configure_database(&connection, &diagnostic_path, journal_mode) {
        drop(connection);
        return Err(error);
    }
    Ok(connection)
}

fn absolute(path: &str) -> std::io::Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(unix)]
fn create_directory(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_directory(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_database_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn create_database_file(path: &std::path::Path) -> std::io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn configure_database(
    database: &Connection,
    path: &str,
    journal_mode: JournalMode,
) -> anyhow::Result<()> {
    database.pragma_update(None, "foreign_keys", "ON")?;
    database.pragma_update(None, "journal_mode", journal_mode.pragma())?;
    let on_disk: u32 =
        database.query_row("PRAGMA user_version", [], |row| row.get("user_version"))?;
    if on_disk != 0 && on_disk != STORAGE_SQLITE_SCHEMA_VERSION {
        return Err(StorageError::new(
            StorageErrorCode::VersionMismatch,
            format!(
                "storage database at \"{path}\" has schema version {on_disk}, incompatible with this build ({STORAGE_SQLITE_SCHEMA_VERSION})"
            ),
        )
        .into());
    }
    database.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS units (
          name    TEXT PRIMARY KEY,
          version INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS unit_globals (
          unit  TEXT PRIMARY KEY REFERENCES units(name),
          value TEXT NOT NULL
        ) STRICT;
        ",
    )?;
    if on_disk == 0 {
        database.pragma_update(None, "user_version", STORAGE_SQLITE_SCHEMA_VERSION)?;
    }
    Ok(())
}

/// Derives the physical record table for two validated identifiers.
#[must_use]
pub fn record_table_name(unit: &str, table: &str) -> String {
    format!("u_{unit}_{table}")
}
