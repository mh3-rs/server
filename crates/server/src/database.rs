//! The SQLite database: one file, shared with the website (same machine) when there is one.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};

/// Opens (creating it if needed) the database at `url` (e.g. `sqlite://data/mh3sp.db`).
pub async fn open(url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(url)
        .with_context(|| format!("database URL {url}"))?
        .create_if_missing(true)
        // Readers never block the writer, also across processes (the website, later).
        .journal_mode(SqliteJournalMode::Wal)
        // Off by default in SQLite: the schema's REFERENCES mean nothing without it.
        .foreign_keys(true)
        // Wait for another writer instead of failing at once.
        .busy_timeout(Duration::from_secs(5));
    SqlitePool::connect_with(options)
        .await
        .with_context(|| format!("open database {url}"))
}
