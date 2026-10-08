//! The mh3sp server. [`start`] brings every configured role up; the binary and the tests both use it.

pub mod config;
pub mod health;
mod net;
mod tls;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub use config::Config;
use net::Role;

/// A started server. Dropping it doesn't stop it: call [`Running::stop`].
pub struct Running {
    /// Bound addresses (useful when the config asked for port 0).
    pub opn: Option<SocketAddr>,
    pub lmp: Option<SocketAddr>,
    pub fmp: Option<SocketAddr>,
    pub health: SocketAddr,
    shutdown: CancellationToken,
    tasks: JoinSet<()>,
}

impl Running {
    /// Stops accepting, closes connections, and waits for every task.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Connects to Postgres and Valkey (failing if either is unreachable), runs migrations, binds
/// every configured listener and starts serving.
pub async fn start(config: &Config) -> Result<Running> {
    let db = sqlx::PgPool::connect(&config.database.url)
        .await
        .context("connect to Postgres")?;
    sqlx::migrate!("../../migrations")
        .run(&db)
        .await
        .context("run migrations")?;
    let valkey = redis::Client::open(config.valkey.url.as_str()).context("Valkey URL")?;
    let mut valkey = redis::aio::ConnectionManager::new(valkey)
        .await
        .context("connect to Valkey")?;
    redis::cmd("PING")
        .query_async::<String>(&mut valkey)
        .await
        .context("Valkey PING")?;

    let shutdown = CancellationToken::new();
    let mut tasks = JoinSet::new();
    let mut bound = [None; 3];
    for (i, (role, addr)) in [
        (Role::Opn, config.listen.opn),
        (Role::Lmp, config.listen.lmp),
        (Role::Fmp, config.listen.fmp),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(addr) = addr else { continue };
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind {role} on {addr}"))?;
        net::announce(role, &listener)?;
        bound[i] = Some(listener.local_addr()?);
        let tls = match role {
            Role::Opn => Some(tls::context(&config.tls.cert, &config.tls.key)?),
            _ => None,
        };
        tasks.spawn(net::serve(listener, role, tls, shutdown.clone()));
    }

    let listener = TcpListener::bind(config.listen.health)
        .await
        .with_context(|| format!("bind health on {}", config.listen.health))?;
    net::announce("health", &listener)?;
    let health = listener.local_addr()?;
    let deps = health::Deps { db, valkey };
    tasks.spawn(health::serve(listener, deps, shutdown.clone()));

    info!("server ready");
    Ok(Running {
        opn: bound[0],
        lmp: bound[1],
        fmp: bound[2],
        health,
        shutdown,
        tasks,
    })
}
