//! The mh3sp server. [`start`] brings every configured role up; the binary and the tests both use it.

pub mod config;
mod database;
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
    pub opn: SocketAddr,
    pub lmp: SocketAddr,
    pub fmp: SocketAddr,
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

/// Opens the database (failing if it can't), runs migrations, binds
/// every configured listener and starts serving.
pub async fn start(config: &Config) -> Result<Running> {
    let db = database::open(&config.database_url).await?;
    sqlx::migrate!("../../migrations")
        .run(&db)
        .await
        .context("run migrations")?;

    let shutdown = CancellationToken::new();
    let mut tasks = JoinSet::new();
    let mut bound = Vec::new();
    for (role, addr) in [
        (Role::Opn, config.opn),
        (Role::Lmp, config.lmp),
        (Role::Fmp, config.fmp),
    ] {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind {role} on {addr}"))?;
        net::announce(role, &listener)?;
        bound.push(listener.local_addr()?);
        let tls = match role {
            Role::Opn => Some(tls::context(&config.tls_cert, &config.tls_key)?),
            _ => None,
        };
        tasks.spawn(net::serve(listener, role, tls, shutdown.clone()));
    }

    let listener = TcpListener::bind(config.health)
        .await
        .with_context(|| format!("bind health on {}", config.health))?;
    net::announce("health", &listener)?;
    let health = listener.local_addr()?;
    let deps = health::Deps { db };
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
