//! Health endpoints over minimal HTTP/1.0, so containers, load balancers and monitors can use them
//! without anything installed:
//!
//! - `GET /health/live`: 200 when the server's runtime answers and the database has opened once (at startup the
//!   server retries until it does). The container healthcheck; failing it justifies a restart.
//! - `GET /health/ready`: 200 when the database answers too and the server is configured, 503 (with the reasons)
//!   otherwise. Never ready in maintenance mode, always live.
//!   A database outage doesn't make the server unhealthy: players already in town or on a quest
//!   don't need it, a restart would drop them, and the pool reconnects by itself. It only means
//!   "don't send new players here".
//! - `POST /announce` (body: the text): an announcement to every logged-in player (`mh3sp announce`).
//! - `POST /stop` (body: a delay like `5min`, or `cancel`): stopping with warning (`mh3sp stop`, `stop.rs`).
//!
//! This port is for the operator: never expose it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::Shared;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>, shutdown: CancellationToken) {
    loop {
        let stream = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = answer(stream, &shared).await {
                debug!(error = %e, "health request failed");
            }
        });
    }
}

async fn answer(mut stream: TcpStream, shared: &Arc<Shared>) -> Result<()> {
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(CHECK_TIMEOUT, stream.read(&mut buf)).await??;
    let request = std::str::from_utf8(&buf[..n]).unwrap_or_default();
    let mut words = request.split_whitespace();
    let (method, path) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    );
    let content = request
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body)
        .trim();
    let (status, body) = match (method, path) {
        ("POST", "/announce") => match content {
            "" => ("400 Bad Request", "no text\n".to_owned()),
            // The whole request must fit the one read.
            _ if n == buf.len() => ("413 Content Too Large", "too long\n".to_owned()),
            text => {
                let players = shared.players.broadcast(&crate::game::announcement(text));
                info!(players, %text, "announcement");
                ("200 OK", format!("sent to {players} players\n"))
            }
        },
        ("POST", "/stop") => match content {
            "cancel" => match crate::stop::cancel(shared) {
                true => ("200 OK", "stop cancelled\n".to_owned()),
                false => ("409 Conflict", "no stop to cancel\n".to_owned()),
            },
            delay => match crate::stop::parse(delay) {
                Some(delay) => {
                    crate::stop::begin(shared, delay);
                    let when = crate::stop::human(delay);
                    ("200 OK", format!("stopping in {when}\n"))
                }
                None => (
                    "400 Bad Request",
                    "a delay like 5min, 1h30m, 30s, or cancel\n".to_owned(),
                ),
            },
        },
        (_, path) => match path {
            "/health/live" => match shared.maintenance().is_some() || shared.db.pool().is_some() {
                true => ("200 OK", "live\n".to_owned()),
                false => (
                    "503 Service Unavailable",
                    "database: not opened yet\n".to_owned(),
                ),
            },
            "/health/ready" => match not_ready(shared).await {
                None => ("200 OK", "ready\n".to_owned()),
                Some(reasons) => ("503 Service Unavailable", reasons),
            },
            _ => ("404 Not Found", "not found\n".to_owned()),
        },
    };
    let response = format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

/// `None` when ready, otherwise one line per reason.
async fn not_ready(shared: &Shared) -> Option<String> {
    if shared.maintenance().is_some() {
        return Some("maintenance\n".to_owned());
    }
    let mut reasons = String::new();
    if !shared.configured() {
        reasons += "not configured: EXTERNAL_ADDRESS and NATNEG_HOSTNAME must be set\n";
    }
    if let Some(reason) = shared.db.not_ready().await {
        reasons += &reason;
        reasons += "\n";
    }
    (!reasons.is_empty()).then_some(reasons)
}

/// `mh3sp healthcheck`: asks a running server whether it is live. Blocking, no runtime needed.
pub fn check(addr: SocketAddr) -> Result<()> {
    let response = request(addr, "GET /health/live HTTP/1.0\r\n\r\n")?;
    match response.split_whitespace().nth(1) {
        Some("200") => Ok(()),
        _ => bail!(
            "not live: {}",
            response.lines().next().unwrap_or("no answer")
        ),
    }
}

/// `mh3sp announce "<text>"`: an announcement to every player of a running server; what it answered.
pub fn announce(addr: SocketAddr, text: &str) -> Result<String> {
    let response = request(
        addr,
        &format!(
            "POST /announce HTTP/1.0\r\nContent-Length: {}\r\n\r\n{text}",
            text.len()
        ),
    )?;
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    match head.split_whitespace().nth(1) {
        Some("200") => Ok(body.trim().to_owned()),
        _ => bail!("refused: {}", body.trim()),
    }
}

/// `mh3sp stop <delay|cancel>`: stopping a running server with warning; what it answered.
pub fn stop(addr: SocketAddr, delay: &str) -> Result<String> {
    let response = request(
        addr,
        &format!(
            "POST /stop HTTP/1.0\r\nContent-Length: {}\r\n\r\n{delay}",
            delay.len()
        ),
    )?;
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    match head.split_whitespace().nth(1) {
        Some("200") => Ok(body.trim().to_owned()),
        _ => bail!("refused: {}", body.trim()),
    }
}

/// One request to a running server's health port; its whole response. Blocking, no runtime needed.
fn request(addr: SocketAddr, request: &str) -> Result<String> {
    // A server bound to 0.0.0.0 is reached through loopback.
    let addr = match addr.ip().is_unspecified() {
        true => SocketAddr::from(([127, 0, 0, 1], addr.port())),
        false => addr,
    };
    let mut stream = StdTcpStream::connect_timeout(&addr, CHECK_TIMEOUT)
        .with_context(|| format!("connect {addr}"))?;
    stream.set_read_timeout(Some(CHECK_TIMEOUT))?;
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}
