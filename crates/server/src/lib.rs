//! The mh3sp server. [`start`] brings every role up; the binary and the tests both use it.

mod accounts;
pub mod config;
mod database;
mod game;
pub mod health;
mod net;
mod text;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub use config::Config;
use database::Database;
use game::{Role, Session};

/// How long a session waits for the console's next connection (decision 4).
const HANDOFF_TTL: Duration = Duration::from_secs(120);

/// What every connection shares.
struct Shared {
    config: Config,
    db: Arc<Database>,
    /// The LMP and FMP ports consoles are handed: the ones listened on.
    lmp_port: u16,
    fmp_port: u16,
    /// Sessions between two connections, by PAT ticket.
    tickets: moka::sync::Cache<String, Session>,
}

impl Shared {
    fn maintenance(&self) -> Option<&str> {
        self.config.maintenance.as_deref()
    }

    fn configured(&self) -> bool {
        self.config.external_address.is_some() && self.config.natneg_hostname.is_some()
    }
}

/// A started server. Dropping it doesn't stop it: call [`Running::stop`].
pub struct Running {
    /// Bound addresses (useful when the config asked for port 0). LMP and FMP are off in maintenance mode.
    pub opn: SocketAddr,
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

/// Binds every listener and starts serving. A database that doesn't open is retried in the background (the
/// server is unhealthy until it does); a listener that can't bind fails the start.
pub async fn start(config: &Config) -> Result<Running> {
    if config.maintenance.is_none() && config.secret.is_none() {
        bail!("SECRET or SECRET__FILE must be set (the support codes' pepper)");
    }
    let shutdown = CancellationToken::new();
    let roles: &[(Role, SocketAddr)] = match config.maintenance {
        Some(_) => &[(Role::Opn, config.opn)],
        None => &[
            (Role::Opn, config.opn),
            (Role::Lmp, config.lmp),
            (Role::Fmp, config.fmp),
        ],
    };
    let mut listeners = Vec::new();
    for &(role, addr) in roles {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind {role} on {addr}"))?;
        net::announce(role, &listener)?;
        listeners.push((role, listener));
    }
    let bound = |role: Role| {
        listeners
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, l)| l.local_addr())
            .transpose()
    };
    let (opn, lmp, fmp) = (
        bound(Role::Opn)?.expect("always bound"),
        bound(Role::Lmp)?,
        bound(Role::Fmp)?,
    );

    let db = Arc::new(Database::default());
    match &config.maintenance {
        Some(_) => info!("maintenance mode: every console is refused"),
        None => {
            db.connect(config.database_url.clone(), shutdown.clone())
                .await
        }
    }
    let shared = Arc::new(Shared {
        config: config.clone(),
        db,
        lmp_port: lmp.map_or(0, |a| a.port()),
        fmp_port: fmp.map_or(0, |a| a.port()),
        tickets: moka::sync::Cache::builder()
            .time_to_live(HANDOFF_TTL)
            .build(),
    });
    if config.maintenance.is_none() && !shared.configured() {
        error!(
            "not configured: EXTERNAL_ADDRESS and NATNEG_HOSTNAME must be set; every console is refused"
        );
    }

    let mut tasks = JoinSet::new();
    for (role, listener) in listeners {
        let tls = match role {
            Role::Opn => Some(tls::context(&config.tls_cert, &config.tls_key)?),
            _ => None,
        };
        tasks.spawn(net::serve(
            listener,
            role,
            tls,
            shared.clone(),
            shutdown.clone(),
        ));
    }

    let listener = TcpListener::bind(config.health)
        .await
        .with_context(|| format!("bind health on {}", config.health))?;
    net::announce("health", &listener)?;
    let health = listener.local_addr()?;
    tasks.spawn(health::serve(listener, shared, shutdown.clone()));

    info!("server started");
    Ok(Running {
        opn,
        lmp,
        fmp,
        health,
        shutdown,
        tasks,
    })
}
