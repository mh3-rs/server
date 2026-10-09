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
use tracing::{debug, info, warn};

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
    shutdown: CancellationToken,
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
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Relays {
            external,
            free: Mutex::new(Free {
                ports: ports.rev().collect(),
                pairs: HashMap::new(),
            }),
            shutdown,
        })
    }

    /// Opens a relay between two consoles' game sockets: the address each must be told its peer is at. `None` if no
    /// port is free or a console has too many relayed pairs.
    pub fn open(self: &Arc<Self>, consoles: [SocketAddrV4; 2]) -> Option<SocketAddrV4> {
        let ips = consoles.map(|c| IpAddr::V4(*c.ip()));
        let socket = {
            let mut free = self.free.lock().unwrap();
            if ips
                .iter()
                .any(|ip| free.pairs.get(ip).copied().unwrap_or(0) >= PAIRS_PER_ADDRESS)
            {
                warn!("relay: too many pairs for one address");
                return None;
            }
            // A port another program holds is skipped (and given back with the others).
            let mut skipped = Vec::new();
            let socket = loop {
                let Some(port) = free.ports.pop() else {
                    break None;
                };
                match std::net::UdpSocket::bind(("0.0.0.0", port)) {
                    Ok(socket) => break Some(socket),
                    Err(_) => skipped.push(port),
                }
            };
            free.ports.extend(skipped);
            let socket = socket.or_else(|| {
                warn!("relay: no free port");
                None
            })?;
            for ip in ips {
                *free.pairs.entry(ip).or_default() += 1;
            }
            socket
        };
        let port = socket.local_addr().ok()?.port();
        socket.set_nonblocking(true).ok()?;
        let socket = UdpSocket::from_std(socket).ok()?;
        info!(port, "relay opened");
        let relays = self.clone();
        tokio::spawn(async move {
            forward(socket, consoles, &relays.shutdown).await;
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

/// Forwards between the two consoles until the pair is idle.
async fn forward(socket: UdpSocket, consoles: [SocketAddrV4; 2], shutdown: &CancellationToken) {
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
        if let Some(to) = seen[1 - side]
            && let Err(e) = socket.send_to(&buf[..n], to).await
        {
            debug!(error = %e, "relay send failed");
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
