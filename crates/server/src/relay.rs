//! The relay (M4 D4, D5): for a pair of consoles that can't reach each other directly, NAT negotiation hands each
//! one the relay's address instead of its peer's, one port per pair; the relay forwards what one sends to the other.
//!
//! The game checks that datagrams come from the address it negotiated, so pings and game traffic go through the same
//! port. A console is recognised by its game socket's address (its port type 0 `INIT`), or by its IP while that
//! address isn't seen (a NAT may map each destination to another port); the first datagram from it sets it. Nothing
//! is forwarded for anyone else: never an open relay.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, warn};

use crate::metrics::{Metrics, add};

/// A pair silent for this long is closed and its port freed (the game itself drops a peer after 20 s).
const IDLE: Duration = Duration::from_secs(5 * 60);
/// Per console and pair: ~100 × the measured game traffic (~0.23 kB/s).
const BYTES_PER_SECOND: usize = 20_000;
/// Relayed pairs one address may have at once (a full city or party is 3 pairs per console).
const PAIRS_PER_ADDRESS: usize = 8;

pub struct Relays {
    /// The address consoles reach the relay at.
    external: Ipv4Addr,
    free: Mutex<Free>,
    metrics: Arc<Metrics>,
    shutdown: CancellationToken,
    /// Where each relay's task runs, so that stopping the server waits for it.
    tasks: TaskTracker,
}

struct Free {
    ports: Vec<u16>,
    /// Relayed pairs per console address.
    pairs: HashMap<IpAddr, usize>,
}

impl Relays {
    pub fn new(
        external: Ipv4Addr,
        ports: RangeInclusive<u16>,
        metrics: Arc<Metrics>,
        shutdown: CancellationToken,
        tasks: TaskTracker,
    ) -> Arc<Self> {
        Arc::new(Relays {
            external,
            free: Mutex::new(Free {
                ports: ports.rev().collect(),
                pairs: HashMap::new(),
            }),
            metrics,
            shutdown,
            tasks,
        })
    }

    /// Opens a relay between two consoles' game sockets: the address each must be told its peer is at. `None` if no
    /// port is free or a console has too many relayed pairs.
    pub fn open(self: &Arc<Self>, consoles: [SocketAddrV4; 2]) -> Option<SocketAddrV4> {
        let ips = consoles.map(|c| IpAddr::V4(*c.ip()));
        let (socket, port) = {
            let mut free = self.free.lock().unwrap();
            if ips
                .iter()
                .any(|ip| free.pairs.get(ip).copied().unwrap_or(0) >= PAIRS_PER_ADDRESS)
            {
                warn!("relay: too many pairs for one address");
                return None;
            }
            // A port another program holds, or one whose socket can't be set up, is skipped (and given back with
            // the others): nothing is taken until a socket is ready, so a failure leaves the ports and the pair
            // counts as they were.
            let mut skipped = Vec::new();
            let opened = loop {
                let Some(port) = free.ports.pop() else {
                    break None;
                };
                match open_socket(port) {
                    Ok(socket) => break Some((socket, port)),
                    Err(e) => {
                        debug!(port, error = %e, "relay: port skipped");
                        skipped.push(port);
                    }
                }
            };
            free.ports.extend(skipped);
            let opened = opened.or_else(|| {
                warn!("relay: no free port");
                None
            })?;
            for ip in ips {
                *free.pairs.entry(ip).or_default() += 1;
            }
            opened
        };
        info!(port, "relay opened");
        add(&self.metrics.relay_pairs_open, 1);
        let relays = self.clone();
        self.tasks.spawn(async move {
            forward(socket, consoles, &relays.metrics, &relays.shutdown).await;
            relays
                .metrics
                .relay_pairs_open
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            let mut free = relays.free.lock().unwrap();
            free.ports.push(port);
            for ip in ips {
                if let Some(n) = free.pairs.get_mut(&ip) {
                    *n -= 1;
                    if *n == 0 {
                        free.pairs.remove(&ip);
                    }
                }
            }
            info!(port, "relay closed");
        });
        Some(SocketAddrV4::new(self.external, port))
    }
}

/// The relay's socket on `port`, ready for the runtime.
fn open_socket(port: u16) -> std::io::Result<UdpSocket> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", port))?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket)
}

/// Forwards between the two consoles until the pair is idle.
async fn forward(
    socket: UdpSocket,
    consoles: [SocketAddrV4; 2],
    metrics: &Metrics,
    shutdown: &CancellationToken,
) {
    let mut seen: [Option<SocketAddr>; 2] = [None; 2];
    let mut budget = [Budget::default(), Budget::default()];
    let mut buf = [0; 1500];
    loop {
        let (n, from) = tokio::select! {
            _ = shutdown.cancelled() => return,
            received = tokio::time::timeout(IDLE, socket.recv_from(&mut buf)) => match received {
                Err(_) => return,
                Ok(Ok(received)) => received,
                Ok(Err(e)) => {
                    debug!(error = %e, "relay receive failed");
                    continue;
                }
            },
        };
        let Some(side) = side(&mut seen, consoles, from) else {
            continue;
        };
        if !budget[side].spend(n) {
            continue;
        }
        if let Some(to) = seen[1 - side] {
            match socket.send_to(&buf[..n], to).await {
                Ok(_) => add(&metrics.relay_bytes, n as u64),
                Err(e) => debug!(error = %e, "relay send failed"),
            }
        }
    }
}

/// Which console a datagram is from: its game socket's address, or the first datagram from its IP once.
fn side(
    seen: &mut [Option<SocketAddr>; 2],
    consoles: [SocketAddrV4; 2],
    from: SocketAddr,
) -> Option<usize> {
    if let Some(side) = seen.iter().position(|s| *s == Some(from)) {
        return Some(side);
    }
    let SocketAddr::V4(v4) = from else {
        return None;
    };
    // ponytail: two consoles behind one IP and a NAT that changes ports per destination can't be told apart until
    // each is seen at its game address; the first unseen one wins.
    let side = (0..2)
        .find(|&i| seen[i].is_none() && consoles[i] == v4)
        .or_else(|| (0..2).find(|&i| seen[i].is_none() && consoles[i].ip() == v4.ip()))?;
    seen[side] = Some(from);
    Some(side)
}

/// Bytes a console may send in the current second.
#[derive(Default)]
struct Budget {
    second: Option<Instant>,
    spent: usize,
}

impl Budget {
    fn spend(&mut self, n: usize) -> bool {
        let now = Instant::now();
        if self
            .second
            .is_none_or(|s| now - s >= Duration::from_secs(1))
        {
            self.second = Some(now);
            self.spent = 0;
        }
        self.spent += n;
        if self.spent > BYTES_PER_SECOND {
            if self.spent - n <= BYTES_PER_SECOND {
                warn!("relay: a console over its rate, dropping");
            }
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port nobody holds now, for a relay's range.
    fn free_port() -> u16 {
        let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        socket.local_addr().unwrap().port()
    }

    fn pair() -> [SocketAddrV4; 2] {
        [
            SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 1000),
            SocketAddrV4::new(Ipv4Addr::new(2, 2, 2, 2), 2000),
        ]
    }

    fn state(relays: &Relays) -> (usize, usize) {
        let free = relays.free.lock().unwrap();
        (free.ports.len(), free.pairs.len())
    }

    #[tokio::test]
    async fn a_closed_relay_gives_its_port_and_counts_back() {
        let shutdown = CancellationToken::new();
        let port = free_port();
        let metrics = Arc::new(Metrics::default());
        let tasks = TaskTracker::new();
        let relays = Relays::new(
            Ipv4Addr::LOCALHOST,
            port..=port,
            metrics,
            shutdown.clone(),
            tasks,
        );
        assert_eq!(state(&relays), (1, 0));
        let at = relays.open(pair()).unwrap();
        assert_eq!(at.port(), port);
        // Open: its port and one pair for each address are taken.
        assert_eq!(state(&relays), (0, 2));
        shutdown.cancel();
        for _ in 0..100 {
            if state(&relays) == (1, 0) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("not given back: {:?}", state(&relays));
    }

    #[tokio::test]
    async fn a_port_that_cannot_be_opened_is_skipped_and_given_back() {
        let (held, spare) = (free_port(), free_port());
        let holder = std::net::UdpSocket::bind(("0.0.0.0", held)).unwrap();
        let relays = Relays::new(
            Ipv4Addr::LOCALHOST,
            held.min(spare)..=held.max(spare),
            Arc::new(Metrics::default()),
            CancellationToken::new(),
            TaskTracker::new(),
        );
        // Every port in between is free too, but the one held is never taken, and stays on offer.
        let ports = relays.free.lock().unwrap().ports.len();
        let at = relays.open(pair()).unwrap();
        assert_ne!(at.port(), held);
        let (left, pairs) = state(&relays);
        assert_eq!((left, pairs), (ports - 1, 2));
        drop(holder);
    }

    #[tokio::test]
    async fn no_free_port_takes_nothing() {
        let held = free_port();
        let holder = std::net::UdpSocket::bind(("0.0.0.0", held)).unwrap();
        let relays = Relays::new(
            Ipv4Addr::LOCALHOST,
            held..=held,
            Arc::new(Metrics::default()),
            CancellationToken::new(),
            TaskTracker::new(),
        );
        assert_eq!(relays.open(pair()), None);
        // The held port is back on offer and no pair was counted.
        assert_eq!(state(&relays), (1, 0));
        drop(holder);
    }

    #[test]
    fn consoles_are_recognised_by_address_then_ip() {
        let a = SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 1000);
        let b = SocketAddrV4::new(Ipv4Addr::new(2, 2, 2, 2), 2000);
        let mut seen = [None; 2];
        // B's NAT mapped another port for the relay: recognised by IP, then by that address.
        let b_mapped = SocketAddr::from(([2, 2, 2, 2], 2001));
        assert_eq!(side(&mut seen, [a, b], b_mapped), Some(1));
        assert_eq!(side(&mut seen, [a, b], SocketAddr::V4(a)), Some(0));
        assert_eq!(side(&mut seen, [a, b], b_mapped), Some(1));
        // Anyone else: nothing.
        assert_eq!(
            side(&mut seen, [a, b], SocketAddr::from(([3, 3, 3, 3], 3000))),
            None
        );
        assert_eq!(
            side(&mut seen, [a, b], SocketAddr::from(([2, 2, 2, 2], 2002))),
            None
        );
    }
}
