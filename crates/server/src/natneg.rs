//! NAT negotiation (UDP 27901) and the QR availability check (UDP 27900), M4.
//!
//! Two consoles about to play together (a city, a quest) each send `INIT`s carrying the same cookie: once both are in,
//! each gets a `CONNECT` with the other's game socket address, then they punch through their NATs by pinging each
//! other, and `REPORT` how it went. The game socket's address is the source of its port type 0 `INIT` (the only one
//! sent from it). A relayed pair is told the relay's address instead (`relay.rs`). Only consoles logged in to FMP are
//! answered. The negotiations are this task's alone: no lock.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};
use wire::natneg::{AVAILABLE, Body, Peer, Record, availability_request};

use crate::Shared;
use crate::config::Strategy;
use crate::game::players::{Nat, Players};
use crate::relay::Relays;

/// A negotiation nobody has spoken in for this long is forgotten (the game gives up waiting for `CONNECT` after
/// 60 s).
const FORGET_AFTER: Duration = Duration::from_secs(60);
/// `CONNECT`'s "got your data" byte, as GameSpy's servers send it.
const GOT_YOUR_DATA: u8 = 0x42;

/// Answers the QR availability check: the game is always available.
pub async fn serve_qr(socket: UdpSocket, shutdown: CancellationToken) {
    let mut buf = [0; 1500];
    loop {
        let (n, from) = tokio::select! {
            _ = shutdown.cancelled() => return,
            received = socket.recv_from(&mut buf) => match received {
                Ok(received) => received,
                Err(e) => {
                    debug!(error = %e, "QR receive failed");
                    continue;
                }
            },
        };
        if availability_request(&buf[..n]).is_some() {
            send(&socket, &AVAILABLE, from).await;
        }
    }
}

/// How pairs are connected.
pub struct Connector {
    pub strategy: Strategy,
    /// `None` without an IPv4 `EXTERNAL_ADDRESS`: direct only.
    pub relays: Option<Arc<Relays>>,
}

pub async fn serve(
    socket: UdpSocket,
    shared: Arc<Shared>,
    connector: Connector,
    shutdown: CancellationToken,
) {
    let mut negotiations: HashMap<u32, Negotiation> = HashMap::new();
    let mut buf = [0; 1500];
    loop {
        let (n, from) = tokio::select! {
            _ = shutdown.cancelled() => return,
            received = socket.recv_from(&mut buf) => match received {
                Ok(received) => received,
                Err(e) => {
                    debug!(error = %e, "NatNeg receive failed");
                    continue;
                }
            },
        };
        let SocketAddr::V4(from) = from else {
            continue;
        };
        let record = match Record::decode(&buf[..n]) {
            Ok(record) => record,
            Err(e) => {
                debug!(error = %e, "not a NatNeg record");
                continue;
            }
        };
        if !shared.players.online((*from.ip()).into()) {
            debug!("NatNeg from an address with no logged-in console, ignored");
            continue;
        }
        let now = Instant::now();
        negotiations.retain(|_, n| now - n.last < FORGET_AFTER);
        for (datagram, to) in handle(
            &connector,
            &shared.players,
            &mut negotiations,
            record,
            from,
            now,
        ) {
            send(&socket, &datagram, to.into()).await;
        }
    }
}

/// One pair's negotiation, by client index.
struct Negotiation {
    clients: [Client; 2],
    /// Once both are in: where each client is told its peer is (the peer's game socket, or the relay).
    told: Option<[SocketAddrV4; 2]>,
    relayed: bool,
    last: Instant,
}

/// Where a console's `INIT`s came from, by port type: 0 its game socket (where its peer pings it); 1 its NatNeg
/// socket, to natneg1; 2 and 3 the same socket, to natneg2 and natneg3.
#[derive(Clone, Copy, Default)]
struct Client([Option<SocketAddrV4>; 4]);

impl Client {
    /// In, once its game socket and its NatNeg socket (to natneg1 and natneg2) are known.
    fn game(&self) -> Option<SocketAddrV4> {
        self.0[1].and(self.0[2]).and(self.0[0])
    }

    /// Where to send it a `CONNECT` (the game accepts it on either socket).
    fn reach(&self) -> Option<SocketAddrV4> {
        self.0[1].or(self.0[0])
    }

    /// Its NAT maps each destination to another port (a symmetric NAT): seen when natneg2 is on another IP than
    /// natneg1 (else both are always the same: never a false alarm).
    fn symmetric(&self) -> bool {
        matches!(self.0, [_, Some(one), Some(two), _] if one.port() != two.port())
    }
}

/// What to send, and to whom, for a record from `from`.
fn handle(
    connector: &Connector,
    players: &Players,
    negotiations: &mut HashMap<u32, Negotiation>,
    record: Record,
    from: SocketAddrV4,
    now: Instant,
) -> Vec<(bytes::Bytes, SocketAddrV4)> {
    let cookie = record.cookie;
    let reply = |body| Record { cookie, body }.encode();
    match record.body {
        Body::Init { port, .. } => {
            let (Some(index), Some(kind)) = (
                [0, 1]
                    .into_iter()
                    .find(|&i| i == usize::from(port.client_index)),
                [0, 1, 2, 3]
                    .into_iter()
                    .find(|&k| k == usize::from(port.kind)),
            ) else {
                return vec![];
            };
            let n = negotiations.entry(cookie).or_insert_with(|| Negotiation {
                clients: Default::default(),
                told: None,
                relayed: false,
                last: now,
            });
            n.last = now;
            n.clients[index].0[kind] = Some(from);
            let mut out = vec![(reply(Body::InitAck(port)), from)];
            let [Some(first), Some(second)] = n.clients.map(|c| c.game()) else {
                return out;
            };
            // Both in: this console learns where its peer is (again on each `INIT`, as GameSpy's servers do: a
            // lost `CONNECT` is never asked for); the first time, the peer too.
            let told = match n.told {
                Some(told) => told,
                None => {
                    let (told, relayed) = connect(n.clients, [first, second], connector, players);
                    n.told = Some(told);
                    n.relayed = relayed;
                    if let Some(to) = n.clients[1 - index].reach() {
                        out.push((reply(Body::Connect(peer(told[1 - index]))), to));
                    }
                    told
                }
            };
            out.insert(1, (reply(Body::Connect(peer(told[index]))), from));
            out
        }
        Body::ConnectAck(..) => {
            trace!(cookie, "CONNECT acknowledged");
            vec![]
        }
        Body::Report {
            port,
            result,
            nat_type,
            mapping,
            ..
        } => {
            let success = result == 1;
            info!(cookie, success, "NatNeg report");
            // A direct pair tells how both NATs fare: a failure sends both to the relay next time (which side
            // can't punch is unknown), the player's retry going through it (the game never retries by itself).
            if let Some(n) = negotiations.get(&cookie).filter(|n| !n.relayed) {
                let nat = if success { Nat::Direct } else { Nat::Relay };
                for game in n.clients.iter().filter_map(Client::game) {
                    players.learn(IpAddr::V4(*game.ip()), nat);
                }
            }
            // The report's first bytes echoed, result cleared (GameSpy's servers' answer).
            let nat = nat_type.to_be_bytes();
            let mapping = mapping.to_be_bytes();
            let tail = [0, nat[0], nat[1], nat[2], nat[3], mapping[0], mapping[1]];
            vec![(reply(Body::ReportAck(port, tail)), from)]
        }
        other => {
            debug!(cookie, record = ?other, "unexpected NatNeg record");
            vec![]
        }
    }
}

/// Where each of a pair's consoles (their game sockets) is told its peer is: each other, or the relay; and whether
/// relayed. `hybrid` relays a pair when either console can't punch: a symmetric NAT seen now, or a failure before.
fn connect(
    clients: [Client; 2],
    consoles: [SocketAddrV4; 2],
    connector: &Connector,
    players: &Players,
) -> ([SocketAddrV4; 2], bool) {
    let ips = consoles.map(|c| IpAddr::V4(*c.ip()));
    let needs_relay = match connector.strategy {
        Strategy::P2p => false,
        Strategy::Relay => true,
        Strategy::Hybrid => {
            for (client, ip) in clients.iter().zip(ips) {
                if client.symmetric() {
                    info!("NatNeg: a symmetric NAT, relayed");
                    players.learn(ip, Nat::Relay);
                }
            }
            ips.iter().any(|&ip| players.nat(ip) == Some(Nat::Relay))
        }
    };
    let relay = match needs_relay {
        true => connector.relays.as_ref().and_then(|r| r.open(consoles)),
        false => None,
    };
    debug!(relayed = relay.is_some(), "NatNeg pair complete");
    match relay {
        Some(relay) => ([relay, relay], true),
        None => ([consoles[1], consoles[0]], false),
    }
}

fn peer(addr: SocketAddrV4) -> Peer {
    Peer {
        addr,
        got_your_data: GOT_YOUR_DATA,
        finished: 0,
    }
}

async fn send(socket: &UdpSocket, datagram: &[u8], to: SocketAddr) {
    if let Err(e) = socket.send_to(datagram, to).await {
        debug!(error = %e, "NatNeg send failed");
    }
}
