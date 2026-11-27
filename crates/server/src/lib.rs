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
pub mod address;
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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
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
    stop: stop::Stop,
    /// The tasks that outlive their request: `Running::stop` waits for them (each ends when `shutdown` is cancelled).
    tasks: TaskTracker,
    /// Stops the whole server.
    shutdown: CancellationToken,
    /// The TCP connections open (`net.rs`), for their limits.
    connections: net::Connections,
    /// Who may reach LMP's first visit and create consoles.
    admission: game::Admission,
    /// What the world's invariants found broken, each once (`invariants` feature).
    #[cfg(feature = "invariants")]
    violations: std::sync::Mutex<Vec<String>>,
}

/// The world, locked.
#[cfg(not(feature = "invariants"))]
type WorldGuard<'a> = std::sync::MutexGuard<'a, game::world::World>;

/// The world, locked: with the `invariants` feature, releasing it checks the world and records any violation (logged
/// in the request that made it, which is the one holding the lock). It never panics: a panic under the lock stops the
/// whole process, and the test must be able to say what broke.
#[cfg(feature = "invariants")]
struct WorldGuard<'a> {
    guard: std::sync::MutexGuard<'a, game::world::World>,
    violations: &'a std::sync::Mutex<Vec<String>>,
}

#[cfg(feature = "invariants")]
impl std::ops::Deref for WorldGuard<'_> {
    type Target = game::world::World;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

#[cfg(feature = "invariants")]
impl std::ops::DerefMut for WorldGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

#[cfg(feature = "invariants")]
impl Drop for WorldGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        for violation in self.guard.check() {
            let mut seen = self.violations.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.contains(&violation) {
                error!(%violation, "the world's invariants are broken");
                seen.push(violation);
            }
        }
    }
}

impl Shared {
    /// The world, locked. A panic while it was held (a bug) may have left it half-changed: the server stops then (the
    /// container restarts it), rather than serve from it, or answer `/health/live` while FMP is dead. A panic
    /// anywhere else ends only its connection.
    fn world(&self) -> WorldGuard<'_> {
        let guard = self.world.lock().unwrap_or_else(|_| {
            error!("the world lock is poisoned by a panic: stopping");
            std::process::abort()
        });
        #[cfg(feature = "invariants")]
        let guard = WorldGuard {
            guard,
            violations: &self.violations,
        };
        guard
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

    /// What the world's invariants found broken since the start, each once (`World::check`).
    #[cfg(feature = "invariants")]
    pub fn violations(&self) -> Vec<String> {
        self.shared.violations.lock().unwrap().clone()
    }

    /// Where the server's long-lived tasks run (connections, relays, a stop's countdown, the database's retry):
    /// `stop` waits for them. A test puts a task of its own there to see that it does.
    pub fn tasks(&self) -> TaskTracker {
        self.shared.tasks.clone()
    }

    /// Cancelled when the server stops (a `mh3sp stop` that came to its end).
    pub fn stopping(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Stops accepting, closes connections, and waits for every task: the listeners, then the connections (each one
    /// closes its handler, so a hunter leaves the world), the relays, a stop's countdown and the database's retry. A
    /// connection that doesn't read is dropped after `net::WRITE_TIMEOUT` at the latest.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        while self.tasks.join_next().await.is_some() {}
        self.shared.tasks.close();
        self.shared.tasks.wait().await;
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

    // Every task that outlives its request (connections, relays, a stop's countdown, the database's retry) runs on
    // this, so that `Running::stop` waits for them.
    let tracker = TaskTracker::new();
    let db = Arc::new(Database::default());
    match &config.maintenance {
        Some(_) => info!("maintenance mode: every console is refused"),
        None => {
            db.connect(config.database_url.clone(), shutdown.clone(), &tracker)
                .await;
            content::check(&config.content_dir);
        }
    }
    let metrics: Arc<metrics::Metrics> = Default::default();
    // The relay's range (M4).
    let external = config
        .external_address
        .as_deref()
        .map(|value| (value, config::External::parse(value)));
    let relays = match (&config.maintenance, external) {
        (None, Some((_, config::External::Ipv4(external)))) => Some(Relays::new(
            external,
            config.relay_ports.clone(),
            metrics.clone(),
            shutdown.clone(),
            tracker.clone(),
        )),
        // Looked up now (`address::LOOKUP_CAP` at most), before any console is served; and again when a pair needs the relay.
        (None, Some((_, config::External::Domain(name)))) if config.strategy != Strategy::P2p => {
            Some(
                Relays::resolving(
                    address::Domain::new(name, config.test_roots.clone()),
                    config.relay_ports.clone(),
                    metrics.clone(),
                    shutdown.clone(),
                    tracker.clone(),
                )
                .await,
            )
        }
        (None, external) => {
            if config.strategy != Strategy::P2p {
                match external {
                    Some((value, config::External::Other)) => error!(
                        "external_address {value:?} is neither an IPv4 address nor a domain name: the relay is off, consoles connect directly"
                    ),
                    _ => error!(
                        "the relay needs external_address as an IPv4 address or a domain: consoles connect directly"
                    ),
                }
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
        tasks: tracker,
        metrics,
        relays,
        shutdown: shutdown.clone(),
        connections: net::Connections::new(config.loopback_exempt),
        admission: game::Admission::new(config.loopback_exempt),
        #[cfg(feature = "invariants")]
        violations: Default::default(),
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

#[cfg(all(test, feature = "invariants"))]
mod tests {
    use bytes::Bytes;

    use super::*;
    use crate::game::profile::Profile;

    /// A broken world is recorded once, whoever locks it next, and nothing panics: the lock stays usable (a panic
    /// under it stops the whole process).
    #[test]
    fn a_broken_world_is_recorded_once_and_the_lock_stays_usable() {
        let world = std::sync::Mutex::new(game::world::World::default());
        let violations = std::sync::Mutex::new(Vec::new());
        let lock = || WorldGuard {
            guard: world.lock().unwrap(),
            violations: &violations,
        };
        {
            let mut world = lock();
            let entered = world.enter("AAAAAA", 1, 1, Bytes::new(), Profile::default());
            entered.unwrap();
            // The one way left to break it: write a layer by hand, for a city nobody created.
            world.place("AAAAAA").unwrap().at = game::world::Layer::City {
                gate: 1,
                city: 1,
                standing: false,
                party: Some(game::world::Member {
                    circle: 1,
                    number: 1,
                    slot: 0,
                    ready: false,
                }),
            };
        }
        let expected = [
            "AAAAAA is in city (1, 1, 1), which doesn't exist".to_owned(),
            "AAAAAA is in quest 1 of (1, 1, 1), which doesn't exist".to_owned(),
        ];
        assert_eq!(*violations.lock().unwrap(), expected);
        // Locked and released again, still broken: not recorded twice.
        drop(lock());
        assert_eq!(*violations.lock().unwrap(), expected);
        // Mended: nothing new, and the world still answers.
        lock().place("AAAAAA").unwrap().at = game::world::Layer::Root;
        assert_eq!(*violations.lock().unwrap(), expected);
        assert_eq!(lock().city_of("AAAAAA"), None);
    }
}
