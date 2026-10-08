//! Listeners and connections. In M0 a connection only reads and logs frames; game logic starts in M1.

use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use openssl::ssl::{Ssl, SslContext};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_openssl::SslStream;
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, trace, warn};
use wire::FrameCodec;

/// A console that hasn't finished its TLS handshake by then is dropped.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Opn,
    Lmp,
    Fmp,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Role::Opn => "opn",
            Role::Lmp => "lmp",
            Role::Fmp => "fmp",
        })
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Internal connection number: the only way logs identify a connection (no IP addresses).
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// Accepts connections until `shutdown`.
pub async fn serve(
    listener: TcpListener,
    role: Role,
    tls: Option<SslContext>,
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
        let span = tracing::info_span!("conn", conn, %role, capcom_id = tracing::field::Empty);
        let tls = tls.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(
            async move {
                if let Err(e) = connection(tcp, tls, shutdown).await {
                    warn!(error = %format!("{e:#}"), "connection dropped");
                }
            }
            .instrument(span),
        );
    }
}

async fn connection(
    tcp: TcpStream,
    tls: Option<SslContext>,
    shutdown: CancellationToken,
) -> Result<()> {
    tcp.set_nodelay(true)?;
    let io: Box<dyn Io> = match tls {
        Some(ctx) => {
            let mut stream = SslStream::new(Ssl::new(&ctx)?, tcp)?;
            tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, Pin::new(&mut stream).accept())
                .await
                .map_err(|_| anyhow::anyhow!("TLS handshake timed out"))?
                .map_err(|e| anyhow::anyhow!("TLS handshake failed: {e}"))?;
            Box::new(stream)
        }
        None => Box::new(tcp),
    };
    debug!("connected");
    let mut frames = FramedRead::new(io, FrameCodec);
    loop {
        let frame = tokio::select! {
            _ = shutdown.cancelled() => break,
            frame = frames.next() => match frame {
                Some(frame) => frame?,
                None => break,
            },
        };
        debug!(packet = %frame.id, seq = frame.seq, len = frame.payload.len(), "recv");
        trace!(payload = %hex(&frame.payload), "recv payload");
    }
    debug!("disconnected");
    Ok(())
}

/// Logs where a role listens; the address is the server's own, not a player's.
pub fn announce(role: impl fmt::Display, listener: &TcpListener) -> Result<()> {
    info!(%role, addr = %listener.local_addr()?, "listening");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
