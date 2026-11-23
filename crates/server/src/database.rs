//! The SQLite database: one file, shared with the website (same machine) when there is one.
//!
//! Outages (ADR 0009): at startup the server runs without it and retries until it opens and the migrations ran;
//! it is unhealthy until then. Afterwards a failing database only makes the server not ready.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
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
    pub async fn connect(
        self: &Arc<Self>,
        url: String,
        shutdown: CancellationToken,
        tasks: &TaskTracker,
    ) {
        match open(&url).await {
            Ok(pool) => self.set(pool),
            Err(e) => {
                error!(error = %format!("{e:#}"), "database unavailable, retrying");
                let this = self.clone();
                tasks.spawn(async move {
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
        // What is logged, and what is said to whoever asks (`/health/ready` is on the public port): the database's own
        // error stays in the log.
        let (logged, said) = match tokio::time::timeout(
            CHECK_TIMEOUT,
            sqlx::query("SELECT 1").execute(pool),
        )
        .await
        {
            Ok(Ok(_)) => (None, None),
            Ok(Err(e)) => (
                Some(format!("database: {e}")),
                Some("database: not answering".to_owned()),
            ),
            Err(_) => {
                let timed_out = "database: timed out".to_owned();
                (Some(timed_out.clone()), Some(timed_out))
            }
        };
        let was_up = self.up.swap(logged.is_none(), Ordering::Relaxed);
        match (&logged, was_up) {
            (Some(reason), true) => error!(%reason, "database down"),
            (None, false) => info!("database back"),
            _ => {}
        }
        said
    }
}

/// Opens (creating it if needed) the database at `url` (e.g. `sqlite://data/mh3sp.db`) and runs the migrations.
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
    let pool = SqlitePool::connect_with(options)
        .await
        .with_context(|| format!("open database {url}"))?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("run migrations")?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/health/ready` is on the public port: it says that the database doesn't answer, never what sqlx said (a pool
    /// closed under the server is how a failing `SELECT 1` is made here).
    #[tokio::test]
    async fn readiness_does_not_tell_what_the_database_said() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("ready.db").display());
        let database = Arc::new(Database::default());
        let tasks = TaskTracker::new();
        database
            .connect(url, CancellationToken::new(), &tasks)
            .await;
        assert_eq!(database.not_ready().await, None);
        let pool = database.pool().unwrap().clone();
        pool.close().await;
        // The failure is real, and carries sqlx's own words.
        let own = sqlx::query("SELECT 1").execute(&pool).await.unwrap_err();
        let own = own.to_string();
        assert!(!own.is_empty());
        let said = database.not_ready().await.unwrap();
        assert_eq!(said, "database: not answering");
        assert!(!said.contains(&own), "{said}");
    }

    /// The constraints the code doesn't check itself: system messages (no sender) are allowed, never as friend
    /// requests; a Japanese console's region is accepted.
    #[tokio::test]
    async fn the_schema_holds_what_it_promises() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let db = open(&url).await.unwrap();
        let run = |sql: &'static str| sqlx::query(sql).execute(&db);
        run(
            "INSERT INTO consoles (id, public_id, support_code_hmac, region)
             VALUES (1, zeroblob(16), zeroblob(32), 'RMHJ08')",
        )
        .await
        .unwrap();
        run(
            "INSERT INTO hunters (id, public_id, console_id, slot, capcom_id, name)
             VALUES (1, zeroblob(16), 1, 1, 'AAAAAA', x'')",
        )
        .await
        .unwrap();
        run("INSERT INTO messages (from_hunter, to_hunter, text, friend_request) VALUES (NULL, 1, x'', 0)")
            .await
            .unwrap();
        // Not delivered yet (its feature isn't built), and not in the way of the inbox.
        assert!(crate::accounts::inbox(&db, 1).await.unwrap().is_empty());
        assert!(
            run("INSERT INTO messages (from_hunter, to_hunter, text, friend_request) VALUES (NULL, 1, x'', 1)")
                .await
                .is_err()
        );
        assert!(
            run("INSERT INTO consoles (public_id, support_code_hmac, region)
                 VALUES (randomblob(16), randomblob(32), 'XXXX08')")
            .await
            .is_err()
        );
    }
}
