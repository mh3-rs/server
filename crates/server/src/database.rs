//! The SQLite database: one file, shared with the website (same machine) when there is one.
//!
//! Outages (decision 9): at startup the server runs without it and retries until it opens and the migrations ran;
//! it is unhealthy until then. Afterwards a failing database only makes the server not ready.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

const RETRY_EVERY: Duration = Duration::from_secs(2);
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Database {
    /// Set once the database opened and the migrations ran; never unset.
    pool: OnceLock<SqlitePool>,
    /// Whether the last check succeeded, so an outage is logged once, not at every check.
    up: AtomicBool,
}

impl Database {
    /// Opens the database, or keeps retrying in the background until it opens or `shutdown`.
    pub async fn connect(self: &Arc<Self>, url: String, shutdown: CancellationToken) {
        match open(&url).await {
            Ok(pool) => self.set(pool),
            Err(e) => {
                error!(error = %format!("{e:#}"), "database unavailable, retrying");
                let this = self.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = tokio::time::sleep(RETRY_EVERY) => {}
                        }
                        if let Ok(pool) = open(&url).await {
                            return this.set(pool);
                        }
                    }
                });
            }
        }
    }

    fn set(&self, pool: SqlitePool) {
        let _ = self.pool.set(pool);
        self.up.store(true, Ordering::Relaxed);
        info!("database ready");
    }

    /// The pool, once the database opened at least once.
    pub fn pool(&self) -> Option<&SqlitePool> {
        self.pool.get()
    }

    /// `None` when the database answers, otherwise why not.
    pub async fn not_ready(&self) -> Option<String> {
        let Some(pool) = self.pool() else {
            return Some("database: not opened yet".to_owned());
        };
        let reason = match tokio::time::timeout(
            CHECK_TIMEOUT,
            sqlx::query("SELECT 1").execute(pool),
        )
        .await
        {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(format!("database: {e}")),
            Err(_) => Some("database: timed out".to_owned()),
        };
        let was_up = self.up.swap(reason.is_none(), Ordering::Relaxed);
        match (&reason, was_up) {
            (Some(reason), true) => error!(%reason, "database down"),
            (None, false) => info!("database back"),
            _ => {}
        }
        reason
    }
}

/// Opens (creating it if needed) the database at `url` (e.g. `sqlite://data/mh3sp.db`) and runs the migrations.
async fn open(url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(url)
        .with_context(|| format!("database URL {url}"))?
        .create_if_missing(true)
        // Readers never block the writer, also across processes (the website, later).
        .journal_mode(SqliteJournalMode::Wal)
        // Off by default in SQLite: the schema's REFERENCES mean nothing without it.
        .foreign_keys(true)
        // Wait for another writer instead of failing at once.
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePool::connect_with(options)
        .await
        .with_context(|| format!("open database {url}"))?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("run migrations")?;
    Ok(pool)
}
