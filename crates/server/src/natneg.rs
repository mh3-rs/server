//! NAT negotiation (UDP 27901) and the QR availability check (UDP 27900), M4.
//!
//! Two consoles about to play together (a city, a quest) each send `INIT`s carrying the same cookie: once both are in,
//! each gets a `CONNECT` with the other's game socket address, then they punch through their NATs by pinging each
//! other, and `REPORT` how it went. The game socket's address is the source of its port type 0 `INIT` (the only one
//! sent from it). Only consoles logged in to FMP are answered. The negotiations are this task's alone: no lock.

use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};
use wire::natneg::{AVAILABLE, Body, Peer, Record, availability_request};

use crate::Shared;

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

pub async fn serve(socket: UdpSocket, shared: Arc<Shared>, shutdown: CancellationToken) {
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
        for (datagram, to) in handle(&mut negotiations, record, from, now) {
            send(&socket, &datagram, to.into()).await;
        }
    }
}

/// One pair's negotiation, by client index.
struct Negotiation {
    clients: [Client; 2],
    /// Both consoles told where the other is.
    paired: bool,
    last: Instant,
}

#[derive(Clone, Copy, Default)]
struct Client {
    /// The game socket's address (the source of its port type 0 `INIT`): where its peer pings it.
    game: Option<SocketAddrV4>,
    /// The NatNeg socket's address (the other `INIT`s).
    natneg: Option<SocketAddrV4>,
}

impl Client {
    /// Where to send it a `CONNECT` (the game accepts it on either socket).
    fn reach(&self) -> Option<SocketAddrV4> {
        self.natneg.or(self.game)
    }
}

/// What to send, and to whom, for a record from `from`.
fn handle(
    negotiations: &mut HashMap<u32, Negotiation>,
    record: Record,
    from: SocketAddrV4,
    now: Instant,
) -> Vec<(bytes::Bytes, SocketAddrV4)> {
    let cookie = record.cookie;
    let reply = |body| Record { cookie, body }.encode();
    match record.body {
        Body::Init { port, .. } => {
            let Some(index) = [0, 1]
                .into_iter()
                .find(|&i| i == usize::from(port.client_index))
            else {
                return vec![];
            };
            let n = negotiations.entry(cookie).or_insert_with(|| Negotiation {
                clients: Default::default(),
                paired: false,
                last: now,
            });
            n.last = now;
            let client = &mut n.clients[index];
            if port.kind == 0 {
                client.game = Some(from);
            } else {
                client.natneg = Some(from);
            }
            let mut out = vec![(reply(Body::InitAck(port)), from)];
            let (me, other) = (n.clients[index], n.clients[1 - index]);
            let (Some(my_game), Some(their_game)) = (me.game, other.game) else {
                return out;
            };
            // Both in: this console learns where its peer is (again on each `INIT`, as GameSpy's servers do: a
            // lost `CONNECT` is never asked for); the first time, the peer too.
            out.push((reply(Body::Connect(peer(their_game))), from));
            if !n.paired {
                n.paired = true;
                debug!(cookie, "NatNeg pair complete");
                if let Some(to) = other.reach() {
                    out.push((reply(Body::Connect(peer(my_game))), to));
                }
            }
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
            info!(cookie, success = result == 1, "NatNeg report");
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
