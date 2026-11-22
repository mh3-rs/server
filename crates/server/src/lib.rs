//! The mh3sp server. [`start`] brings every role up; the binary and the tests both use it.
//!
//! Concurrency, who owns what:
//! - **A connection** is one task (`net`): it alone reads and writes its socket, and owns its `Session` (the
//!   logged-in console, its hunter and profile). Between two connections a session sits in `Shared::tickets`,
//!   moved out by the next connection (never shared).
//! - **Frames for another player** go through that player's mailbox (`game::players`): a bounded channel its task
//!   drains to its socket. Senders never wait: a full mailbox drops its player (a memory guard).
//! - **The world** (who is where, cities, quests) is one `std::sync::Mutex` (`Shared::world`), held for a lookup
//!   and mailbox pushes, never across an `.await`. Never a tokio mutex: nothing awaits while holding state.
//! - **The registry** of logged-in players (`Shared::players`, a `DashMap`) maps Capcom IDs to mailboxes.
//! - **The database** is the only state outside the process; handlers `.await` it without holding any lock.
//!   Slow work (argon2) runs on `spawn_blocking`.

mod access;
mod accounts;
pub mod check;
pub mod config;
mod content;
pub mod control;
pub mod database;
mod game;
mod keys;
mod metrics;
mod nas;
mod natneg;
mod net;
pub mod patch;
pub mod public;
mod relay;
mod stop;
mod text;
mod tls;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub use config::Config;

/// The release running: the workspace's version, bumped by `scripts/release.sh`. `--version`, the startup log line and
/// the metrics all report it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use config::Strategy;
use database::Database;
use game::players::Players;
use game::{Role, Session};
use relay::Relays;

/// How long a session waits for the console's next connection (ADR 0004).
const HANDOFF_TTL: Duration = Duration::from_secs(120);
/// How long a jump to another world server may take to finish (M5 D13).
const JUMP_TTL: Duration = Duration::from_secs(60);

/// What every connection shares.
struct Shared {
    config: Config,
    db: Arc<Database>,
    /// The LMP and FMP ports consoles are handed: the ones listened on.
    lmp_port: u16,
    fmp_port: u16,
    /// Our Nintendo login's tokens, waiting for their console (`nas.rs`).
    nas: nas::Tokens,
    /// Sessions between two connections, by PAT ticket.
    tickets: moka::sync::Cache<String, Session>,
    /// With encryption on: the consoles' keys between two connections (`keys.rs`). Secrets: never logged.
    keys: Option<keys::Keys>,
    /// Jumps to another world server under way (M5 D13), by ticket: who, and where to.
    jumps: moka::sync::Cache<u32, (String, game::world::Target)>,
    /// Players logged in on FMP, by Capcom ID.
    players: Players,
    /// Where the players are.
    ///
    /// ponytail: one lock for the whole world. Held for microseconds (a lookup, frames dropped into mailboxes;
    /// never across an `.await` or a socket write): ~500 players relaying ~4 messages a second is ~0.4 % of a
    /// core. If the relay-latency test (p99 < 10 ms) or production shows contention, split per world server first
    /// (nothing crosses that boundary but population counts); finer locks (gate, city, quest) would need lock
    /// ordering for every move between levels.
    world: std::sync::Mutex<game::world::World>,
    /// When the server started (the reference of the game's day/night cycle).
    started: SystemTime,
    metrics: Arc<metrics::Metrics>,
    /// The relay's port range (M4): relayed pairs.
    relays: Option<Arc<Relays>>,
    /// A stop under way (`stop.rs`): cancelling it lifts it.
    stop: std::sync::Mutex<Option<CancellationToken>>,
    /// Stops the whole server.
    shutdown: CancellationToken,
    /// The TCP connections open (`net.rs`), for their limits.
    connections: net::Connections,
    /// Who may reach LMP's first visit and create consoles.
    admission: game::Admission,
}

impl Shared {
    /// The world, locked. A panic while it was held (a bug) may have left it half-changed: the server stops then (the
    /// container restarts it), rather than serve from it, or answer `/health/live` while FMP is dead. A panic
    /// anywhere else ends only its connection.
    fn world(&self) -> std::sync::MutexGuard<'_, game::world::World> {
        self.world.lock().unwrap_or_else(|_| {
            error!("the world lock is poisoned by a panic: stopping");
            std::process::abort()
        })
    }

    /// The server's clock (the system's, shifted by `clock_offset`).
    fn now(&self) -> SystemTime {
        let offset = Duration::from_secs(self.config.clock_offset.unsigned_abs());
        match self.config.clock_offset >= 0 {
            true => SystemTime::now() + offset,
            false => SystemTime::now() - offset,
        }
    }

    fn maintenance(&self) -> Option<&str> {
        self.config.maintenance.as_deref()
    }

    fn configured(&self) -> bool {
        self.config.external_address.is_some() && self.config.domain.is_some()
    }
}

/// A started server. Dropping it doesn't stop it: call [`Running::stop`].
pub struct Running {
    /// Bound addresses (useful when the config asked for port 0). LMP and FMP are off in maintenance mode.
    pub opn: SocketAddr,
    pub lmp: Option<SocketAddr>,
    pub fmp: Option<SocketAddr>,
    /// The public port.
    pub public: SocketAddr,
    /// The Nintendo login (HTTP).
    pub nas: SocketAddr,
    /// UDP; off in maintenance mode.
    pub qr: Option<SocketAddr>,
    pub natneg: Option<SocketAddr>,
    shutdown: CancellationToken,
    tasks: JoinSet<()>,
    shared: Arc<Shared>,
}

impl Running {
    /// Stops in `delay`, warning players (`mh3sp stop`, SIGTERM).
    pub fn stop_in(&self, delay: Duration) {
        stop::begin(&self.shared, delay);
    }

    /// Cancelled when the server stops (a `mh3sp stop` that came to its end).
    pub fn stopping(&self) -> CancellationToken {
        self.shutdown.clone()
    }

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
        bail!("secret or secret__file must be set (the support codes' pepper)");
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
                .await;
            content::check(&config.content_dir);
        }
    }
    let metrics: Arc<metrics::Metrics> = Default::default();
    // The relay's range (M4).
    let relays = match (
        &config.maintenance,
        config
            .external_address
            .as_deref()
            .map(str::parse::<Ipv4Addr>),
    ) {
        (None, Some(Ok(external))) => Some(Relays::new(
            external,
            config.relay_ports.clone(),
            metrics.clone(),
            shutdown.clone(),
        )),
        (None, _) => {
            if config.strategy != Strategy::P2p {
                error!(
                    "the relay needs external_address as an IPv4 address: consoles connect directly"
                );
            }
            None
        }
        (Some(_), _) => None,
    };
    let shared = Arc::new(Shared {
        config: config.clone(),
        db,
        lmp_port: lmp.map_or(0, |a| a.port()),
        fmp_port: fmp.map_or(0, |a| a.port()),
        nas: nas::tokens(),
        tickets: moka::sync::Cache::builder()
            .time_to_live(HANDOFF_TTL)
            .build(),
        keys: config.encryption.then(keys::Keys::default),
        jumps: moka::sync::Cache::builder().time_to_live(JUMP_TTL).build(),
        players: Players::default(),
        world: Default::default(),
        started: SystemTime::now(),
        stop: Default::default(),
        metrics,
        relays,
        shutdown: shutdown.clone(),
        connections: net::Connections::new(config.loopback_exempt),
        admission: game::Admission::new(config.loopback_exempt),
    });
    if config.maintenance.is_none() && !shared.configured() {
        error!("not configured: external_address and domain must be set; every console is refused");
    }

    tls::prepare(&config.tls_dir)?;
    let mut tasks = JoinSet::new();
    for (role, listener) in listeners {
        let tls = match role {
            Role::Opn => Some(tls::context(&config.tls_dir)?),
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

    // The Nintendo login, in maintenance mode too: the console must reach OPN to read why it's refused.
    let listener = TcpListener::bind(config.nas)
        .await
        .with_context(|| format!("bind the NAS on {}", config.nas))?;
    net::announce("nas", &listener)?;
    let nas = listener.local_addr()?;
    tasks.spawn(nas::serve(listener, shared.clone(), shutdown.clone()));

    // The game's UDP services (M4): its availability check, NAT negotiation.
    let (mut qr, mut natneg) = (None, None);
    if config.maintenance.is_none() {
        let socket = UdpSocket::bind(config.qr)
            .await
            .with_context(|| format!("bind QR on {}", config.qr))?;
        qr = Some(socket.local_addr()?);
        info!(role = "qr", addr = %socket.local_addr()?, "listening");
        tasks.spawn(natneg::serve_qr(socket, shutdown.clone()));
        let socket = UdpSocket::bind(config.natneg)
            .await
            .with_context(|| format!("bind NatNeg on {}", config.natneg))?;
        natneg = Some(socket.local_addr()?);
        info!(role = "natneg", addr = %socket.local_addr()?, "listening");
        let connector = natneg::Connector {
            strategy: config.strategy,
            relays: shared.relays.clone(),
            metrics: shared.metrics.clone(),
            loopback_exempt: config.loopback_exempt,
        };
        tasks.spawn(natneg::serve(
            socket,
            shared.clone(),
            connector,
            shutdown.clone(),
        ));
    }

    // A key of `mh3sp.toml` that left the server (the quest bots' port, the API's): the configuration keeps starting
    // the server (no breaking change since the image is public), and the operator is told.
    for key in &config.ignored {
        tracing::warn!(
            key,
            "this key of mh3sp.toml was removed from the server: ignored"
        );
    }

    let listener = TcpListener::bind(config.public)
        .await
        .with_context(|| format!("bind the public port on {}", config.public))?;
    net::announce("public", &listener)?;
    let public = listener.local_addr()?;
    tasks.spawn(public::serve(listener, shared.clone(), shutdown.clone()));
    control::start(&config.control, &shared, &shutdown, &mut tasks);

    info!(version = VERSION, "server started");
    Ok(Running {
        opn,
        lmp,
        fmp,
        public,
        nas,
        qr,
        natneg,
        shutdown,
        tasks,
        shared,
    })
}
