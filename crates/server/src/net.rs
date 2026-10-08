//! Listeners and connections: the connection lifecycle shared by every role (the pause, line checks, `ReqShut`),
//! the rest handed to the role's [`Handler`]. Each connection is one task that reads and writes, so frames are
//! always written whole and in order.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::anyhow;
use futures_util::{SinkExt, StreamExt};
use openssl::ssl::{Ssl, SslContext};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, interval_at};
use tokio_openssl::SslStream;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, trace, warn};
use wire::packets::{AnsLineCheck, AnsShut, Packet, ReqLineCheck, ReqShut};
use wire::{Frame, FrameCodec};

use crate::Shared;
use crate::game::{Fault, Handled, Handler, Role, frame, reply};

/// A console that hasn't finished its TLS handshake by then is dropped.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait before the server's first message: the console resets a connection whose first message comes too early
/// (error 11613). Bisected on Dolphin (2026-10-08): fails at 0 and 20 ms, works from 45 ms; 250 ms leaves a
/// margin for a real Wii. Over TLS (OPN) the handshake already takes that long (0 ms worked); MH3SP waits 1.5 s.
const PAUSE: Duration = Duration::from_millis(250);
/// A line check is sent this often; one left unanswered until the next drops the connection. (The console gives
/// up after 90 s without one.)
const LINE_CHECK_EVERY: Duration = Duration::from_secs(30);

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Internal connection number: the only way logs identify a connection (no IP addresses).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Accepts connections until `shutdown`.
pub async fn serve(
    listener: TcpListener,
    role: Role,
    tls: Option<SslContext>,
    shared: Arc<Shared>,
    shutdown: CancellationToken,
) {
    loop {
        let tcp = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((tcp, _peer)) => tcp,
                Err(e) => {
                    // Usually out of file descriptors: back off instead of spinning.
                    warn!(%role, error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
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
                match connection(tcp, role, tls, &shared, shutdown).await {
                    Ok(()) => debug!("disconnected"),
                    Err(Fault::Client(e)) => warn!(error = %format!("{e:#}"), "connection dropped"),
                    Err(Fault::Server(e)) => {
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
    role: Role,
    tls: Option<SslContext>,
    shared: &Shared,
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
    let (mut handler, first) = Handler::start(role);
    if !send(&mut framed, first?).await {
        return Ok(());
    }

    let mut line_check = interval_at(Instant::now() + LINE_CHECK_EVERY, LINE_CHECK_EVERY);
    let mut answered = true;
    loop {
        let out = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = line_check.tick() => {
                if !answered {
                    info!("line check unanswered, disconnecting");
                    return Ok(());
                }
                answered = false;
                vec![frame(0, &ReqLineCheck {})?]
            }
            received = framed.next() => {
                let req = match received {
                    Some(Ok(req)) => req,
                    Some(Err(e)) => {
                        debug!(error = %e, "connection lost");
                        return Ok(());
                    }
                    None => return Ok(()),
                };
                debug!(packet = %req.id, seq = req.seq, len = req.payload.len(), "recv");
                trace!(payload = %hex(&req.payload), "recv payload");
                match req.id {
                    AnsLineCheck::ID => {
                        AnsLineCheck::decode(&req.payload)?;
                        answered = true;
                        continue;
                    }
                    ReqShut::ID => {
                        handler.leave(shared);
                        shut(&req)?
                    }
                    _ => handler.handle(shared, &req).await?,
                }
            }
        };
        if !send(&mut framed, out).await {
            return Ok(());
        }
    }
}

/// The console leaves (to another server, or for good): it closes the connection after the answer.
fn shut(req: &Frame) -> Handled {
    let kind = ReqShut::decode(&req.payload)?.kind;
    reply(req.seq, &AnsShut { kind })
}

/// Sends frames in order; `false` when the connection is gone.
async fn send(framed: &mut Framed<Box<dyn Io>, FrameCodec>, frames: Vec<Frame>) -> bool {
    for frame in frames {
        debug!(packet = %frame.id, seq = frame.seq, len = frame.payload.len(), "send");
        trace!(payload = %hex(&frame.payload), "send payload");
        if let Err(e) = framed.feed(frame).await {
            debug!(error = %e, "connection lost");
            return false;
        }
    }
    match framed.flush().await {
        Ok(()) => true,
        Err(e) => {
            debug!(error = %e, "connection lost");
            false
        }
    }
}

/// Logs where a role listens; the address is the server's own, not a player's.
pub fn announce(role: impl fmt::Display, listener: &TcpListener) -> anyhow::Result<()> {
    info!(%role, addr = %listener.local_addr()?, "listening");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
