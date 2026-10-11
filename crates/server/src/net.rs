//! Listeners and connections: the connection lifecycle shared by every role (the pause, line checks, `ReqShut`),
//! the rest handed to the role's [`Handler`]. Each connection is one task that reads and writes, so frames are
//! always written whole and in order.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::anyhow;
use futures_util::{SinkExt, StreamExt};
use openssl::ssl::{Ssl, SslContext};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, interval_at};
use tokio_openssl::SslStream;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, warn};
use wire::packets::{AnsLineCheck, AnsShut, Packet, ReqLineCheck, ReqShut};
use wire::{Frame, FrameCodec, crypt};

use crate::Shared;
use crate::game::players::Mailbox;
use crate::game::{Fault, Handled, Handler, Role, frame, reply};
use crate::keys::Key;

/// A console that hasn't finished its TLS handshake by then is dropped.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait before the server's first message: the console resets a connection whose first message comes too early
/// (error 11613). Bisected on Dolphin (2026-10-08): fails at 0 and 20 ms, works from 45 ms; 250 ms leaves a
/// margin for a real Wii. Over TLS (OPN) the handshake already takes that long (0 ms worked); MH3SP waits 1.5 s.
const PAUSE: Duration = Duration::from_millis(250);
/// A line check is sent this often; one left unanswered until the next drops the connection. (The console gives
/// up after 90 s without one.)
const LINE_CHECK_EVERY: Duration = Duration::from_secs(30);
/// How long a write may wait for a console that stopped reading before the connection is dropped: as long as an
/// unanswered line check (provisional, the review's limits). Without it the console's task froze on a full socket and
/// stayed as a ghost.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// A connection the console is about to close (it sent `ReqShut`, or got a refusal's text): no more line checks or
/// mail, its `ReqShut` still answered, any other packet closes it, and so does this delay (the user, 2026-10-11: a
/// socket that should close is kept from normal commands). A console closes within 0.1 s (lab, 2026-10-11).
const CLOSE_GRACE: Duration = Duration::from_secs(10);

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Internal connection number: the only way logs identify a connection (no IP addresses).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Connections open at once, in total and from one IP address (provisional, the review's limits): a client opening
/// sockets in a loop (each TLS handshake costs an RSA operation) is refused at accept. A console holds one or two at
/// a time; several consoles may share an address.
const MAX_CONNECTIONS: usize = 2000;
const MAX_PER_ADDRESS: usize = 16;

/// The connections open: in total, and per IP address.
pub struct Connections {
    open: std::sync::Mutex<(usize, HashMap<IpAddr, usize>)>,
    /// `Config::loopback_exempt`.
    loopback_exempt: bool,
}

impl Connections {
    pub fn new(loopback_exempt: bool) -> Self {
        Connections {
            open: Default::default(),
            loopback_exempt,
        }
    }

    /// Counts a new connection from `ip`, unless a limit is reached (then `false`); `close` uncounts it.
    fn open(&self, ip: IpAddr) -> bool {
        let mut open = self.open.lock().unwrap();
        let (total, by_address) = &mut *open;
        let from_ip = by_address.entry(ip).or_default();
        // Loopback is this machine: the lab and the tests, every console on one address.
        let per_address =
            !(self.loopback_exempt && ip.is_loopback()) && *from_ip >= MAX_PER_ADDRESS;
        if *total >= MAX_CONNECTIONS || per_address {
            if *from_ip == 0 {
                by_address.remove(&ip);
            }
            return false;
        }
        *total += 1;
        *from_ip += 1;
        true
    }

    fn close(&self, ip: IpAddr) {
        let mut open = self.open.lock().unwrap();
        let (total, by_address) = &mut *open;
        *total -= 1;
        if let Some(n) = by_address.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                by_address.remove(&ip);
            }
        }
    }
}

/// A connection counted in `Shared::connections`, until dropped.
struct Counted {
    shared: Arc<Shared>,
    ip: IpAddr,
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.shared.connections.close(self.ip);
    }
}

/// Accepts connections until `shutdown`.
pub async fn serve(
    listener: TcpListener,
    role: Role,
    tls: Option<SslContext>,
    shared: Arc<Shared>,
    shutdown: CancellationToken,
) {
    loop {
        let (tcp, peer) = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    // Usually out of file descriptors: back off instead of spinning.
                    warn!(%role, error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        if !shared.connections.open(peer.ip()) {
            crate::metrics::add(&shared.metrics.limited, 1);
            debug!(%role, "too many connections, refused");
            continue;
        }
        let counted = Counted {
            shared: shared.clone(),
            ip: peer.ip(),
        };
        let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
        let span = tracing::info_span!(
            "conn",
            conn,
            %role,
            console = tracing::field::Empty,
            capcom_id = tracing::field::Empty
        );
        let tls = tls.clone();
        let shared = shared.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(
            async move {
                let _counted = counted;
                match connection(tcp, peer.ip(), conn, role, tls, &shared, shutdown).await {
                    Ok(()) => debug!("disconnected"),
                    Err(Fault::Client(e)) => warn!(error = %format!("{e:#}"), "connection dropped"),
                    // `Handler::handle` answers database failures: one landing here is a bug, logged as the server's.
                    Err(Fault::Server(e) | Fault::Database(e)) => {
                        error!(error = %format!("{e:#}"), "connection dropped")
                    }
                }
            }
            .instrument(span),
        );
    }
}

async fn connection(
    tcp: TcpStream,
    ip: IpAddr,
    conn: u64,
    role: Role,
    tls: Option<SslContext>,
    shared: &Arc<Shared>,
    shutdown: CancellationToken,
) -> Result<(), Fault> {
    let server = |e: anyhow::Error| Fault::Server(e);
    tcp.set_nodelay(true).map_err(|e| server(e.into()))?;
    let io: Box<dyn Io> = match tls {
        Some(ctx) => {
            let ssl = Ssl::new(&ctx).map_err(|e| server(e.into()))?;
            let mut stream = SslStream::new(ssl, tcp).map_err(|e| server(e.into()))?;
            tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, Pin::new(&mut stream).accept())
                .await
                .map_err(|_| Fault::Client(anyhow!("TLS handshake timed out")))?
                .map_err(|e| Fault::Client(anyhow!("TLS handshake failed: {e}")))?;
            Box::new(stream)
        }
        None => Box::new(tcp),
    };
    debug!("connected");
    let mut framed = Framed::new(io, FrameCodec);

    tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        _ = tokio::time::sleep(PAUSE) => {}
    }
    let (mailbox, mut mail, kicked) = Mailbox::new(conn, ip);
    // With encryption on, every LMP and FMP payload is encrypted with the console's key (OPN has TLS).
    let encrypted = role != Role::Opn && shared.keys.is_some();
    let (mut handler, first) = Handler::start(role, mailbox);
    let result = serve_frames(
        &mut framed,
        Crypt {
            ip,
            encrypted,
            key: None,
        },
        &mut handler,
        first,
        &mut mail,
        &kicked,
        shared,
        &shutdown,
    )
    .await;
    handler.close(shared);
    result
}

/// A connection's encryption: on for LMP and FMP with encryption on, with the console's key once its first frame
/// told which (`keys.rs`).
struct Crypt {
    ip: IpAddr,
    encrypted: bool,
    key: Option<Key>,
}

/// Requests in, answers and mail out, until the connection ends.
// Every argument is the connection's own state, borrowed from `serve`: a struct bundling them would serve this one
// call.
#[allow(clippy::too_many_arguments)]
async fn serve_frames(
    framed: &mut Framed<Box<dyn Io>, FrameCodec>,
    mut crypt: Crypt,
    handler: &mut Handler,
    first: Handled,
    mail: &mut mpsc::Receiver<Frame>,
    kicked: &CancellationToken,
    shared: &Arc<Shared>,
    shutdown: &CancellationToken,
) -> Result<(), Fault> {
    // Encrypted, the opening uses the console's likely key (`keys.rs`): with none waiting at its IP, it can't be one
    // of ours. Another console's key, when several wait at one IP, is harmless: the console ignores the opening's
    // value (lab, 2026-10-10).
    let opening = match &shared.keys {
        Some(keys) if crypt.encrypted => Some(
            keys.likely(crypt.ip)
                .ok_or_else(|| Fault::Client(anyhow!("no key in transit")))?,
        ),
        _ => None,
    };
    if !send(framed, opening.as_ref(), first?).await? {
        return Ok(());
    }

    let mut line_check = interval_at(Instant::now() + LINE_CHECK_EVERY, LINE_CHECK_EVERY);
    let mut answered = true;
    let mut closing: Option<Instant> = None;
    loop {
        let out = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = kicked.cancelled() => {
                info!("kicked");
                return Ok(());
            }
            _ = tokio::time::sleep_until(closing.unwrap_or_else(Instant::now)), if closing.is_some() => {
                debug!("left open after its goodbye: closed");
                return Ok(());
            }
            Some(frame) = mail.recv(), if closing.is_none() => vec![frame],
            _ = line_check.tick(), if closing.is_none() => {
                if !answered {
                    info!("line check unanswered, disconnecting");
                    return Ok(());
                }
                answered = false;
                vec![frame(0, &ReqLineCheck {})?]
            }
            received = framed.next() => {
                let mut req = match received {
                    Some(Ok(req)) => req,
                    Some(Err(e)) => {
                        debug!(error = %e, "connection lost");
                        return Ok(());
                    }
                    None => return Ok(()),
                };
                match (&crypt.key, &shared.keys) {
                    (Some(key), _) => {
                        req.payload = crypt::decrypt(key, &req.payload)
                            .map_err(|e| Fault::Client(anyhow!("{} undecryptable: {e}", req.id)))?;
                    }
                    // The first frame: which console's key.
                    (None, Some(keys)) if crypt.encrypted => {
                        let (key, plain) = keys
                            .take(crypt.ip, &req.payload)
                            .ok_or_else(|| Fault::Client(anyhow!("no key in transit fits")))?;
                        crypt.key = Some(key);
                        req.payload = plain;
                    }
                    _ => {}
                }
                // Ids and sizes only, at every level: payloads carry support codes and tickets (never logged).
                debug!(packet = %req.id, seq = req.seq, len = req.payload.len(), "recv");
                if closing.is_some() {
                    if req.id != ReqShut::ID {
                        debug!(packet = %req.id, "a request after its goodbye: closed");
                        return Ok(());
                    }
                    shut(&req)?
                } else {
                match req.id {
                    AnsLineCheck::ID => {
                        AnsLineCheck::decode(&req.payload)?;
                        answered = true;
                        continue;
                    }
                    ReqShut::ID => {
                        handler.leave(shared);
                        // The console's key goes on to its next connection.
                        if let Some(keys) = &shared.keys
                            && let Some(key) = crypt.key.or(handler.given_key())
                        {
                            keys.put(crypt.ip, key);
                        }
                        closing = Some(Instant::now() + CLOSE_GRACE);
                        shut(&req)?
                    }
                    _ => {
                        let start = std::time::Instant::now();
                        let out = handler.handle(shared, &req).await?;
                        shared.metrics.request(start.elapsed());
                        if handler.finished() {
                            closing = Some(Instant::now() + CLOSE_GRACE);
                        }
                        out
                    }
                }
                }
            }
        };
        if !send(framed, crypt.key.as_ref(), out).await? {
            return Ok(());
        }
    }
}

/// The console leaves (to another server, or for good): it closes the connection after the answer.
fn shut(req: &Frame) -> Handled {
    let kind = ReqShut::decode(&req.payload)?.kind;
    reply(req.seq, &AnsShut { kind })
}

/// Sends frames in order, encrypted with `key` if any: `false` when the connection is gone, or the console stopped
/// reading for `WRITE_TIMEOUT`.
async fn send(
    framed: &mut Framed<Box<dyn Io>, FrameCodec>,
    key: Option<&[u8; crypt::KEY_LEN]>,
    frames: Vec<Frame>,
) -> Result<bool, Fault> {
    match tokio::time::timeout(WRITE_TIMEOUT, write(framed, key, frames)).await {
        Ok(sent) => sent,
        Err(_) => {
            info!("console not reading, disconnecting");
            Ok(false)
        }
    }
}

async fn write(
    framed: &mut Framed<Box<dyn Io>, FrameCodec>,
    key: Option<&[u8; crypt::KEY_LEN]>,
    frames: Vec<Frame>,
) -> Result<bool, Fault> {
    for mut frame in frames {
        debug!(packet = %frame.id, seq = frame.seq, len = frame.payload.len(), "send");
        if let Some(key) = key {
            frame.payload =
                crypt::encrypt(key, &frame.payload).map_err(|e| Fault::Server(e.into()))?;
        }
        if let Err(e) = framed.feed(frame).await {
            debug!(error = %e, "connection lost");
            return Ok(false);
        }
    }
    match framed.flush().await {
        Ok(()) => Ok(true),
        Err(e) => {
            debug!(error = %e, "connection lost");
            Ok(false)
        }
    }
}

/// Logs where a role listens; the address is the server's own, not a player's.
pub fn announce(role: impl fmt::Display, listener: &TcpListener) -> anyhow::Result<()> {
    info!(%role, addr = %listener.local_addr()?, "listening");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_limited_per_address() {
        let connections = Connections::new(true);
        let (a, b) = (IpAddr::from([10, 0, 0, 1]), IpAddr::from([10, 0, 0, 2]));
        for _ in 0..MAX_PER_ADDRESS {
            assert!(connections.open(a));
        }
        assert!(!connections.open(a));
        assert!(connections.open(b));
        connections.close(a);
        assert!(connections.open(a));
        for _ in 0..MAX_PER_ADDRESS {
            connections.close(a);
        }
        connections.close(b);
        assert_eq!(connections.open.lock().unwrap().0, 0);
        assert!(connections.open.lock().unwrap().1.is_empty());
        let local = IpAddr::from([127, 0, 0, 1]);
        assert!((0..=MAX_PER_ADDRESS).all(|_| connections.open(local)));
    }

    /// A console that stops reading: the write gives up after `WRITE_TIMEOUT` instead of waiting forever.
    #[tokio::test(start_paused = true)]
    async fn a_console_not_reading_is_dropped() {
        // A 64-byte pipe whose other end never reads.
        let (ours, _theirs) = tokio::io::duplex(64);
        let mut framed = Framed::new(Box::new(ours) as Box<dyn Io>, FrameCodec);
        let big = Frame {
            seq: 0,
            id: AnsLineCheck::ID,
            payload: bytes::Bytes::from(vec![0; 1024]),
        };
        let start = Instant::now();
        assert!(!send(&mut framed, None, vec![big]).await.unwrap());
        assert_eq!(start.elapsed(), WRITE_TIMEOUT);
        // A small one fits and goes.
        let (ours, _theirs) = tokio::io::duplex(64);
        let mut framed = Framed::new(Box::new(ours) as Box<dyn Io>, FrameCodec);
        let small = frame(0, &ReqLineCheck {}).unwrap();
        assert!(send(&mut framed, None, vec![small]).await.unwrap());
    }
}
