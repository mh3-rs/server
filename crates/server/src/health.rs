//! Health endpoints over minimal HTTP/1.0, so containers, load balancers and monitors can use them
//! without anything installed:
//!
//! - `GET /health/live`: 200 when the server's runtime answers. The container healthcheck; failing
//!   it justifies a restart.
//! - `GET /health/ready`: 200 when the database answers too, 503 (with the reason) otherwise.
//!   A database outage doesn't make the server unhealthy: players already in town or on a quest
//!   don't need it, a restart would drop them, and the pool reconnects by itself. It only means
//!   "don't send new players here".

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::debug;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct Deps {
    pub db: SqlitePool,
}

pub async fn serve(listener: TcpListener, deps: Deps, shutdown: CancellationToken) {
    loop {
        let stream = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
        };
        let deps = deps.clone();
        tokio::spawn(async move {
            if let Err(e) = answer(stream, &deps).await {
                debug!(error = %e, "health request failed");
            }
        });
    }
}

async fn answer(mut stream: TcpStream, deps: &Deps) -> Result<()> {
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(CHECK_TIMEOUT, stream.read(&mut buf)).await??;
    let request = std::str::from_utf8(&buf[..n]).unwrap_or_default();
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    let (status, body) = match path {
        "/health/live" => ("200 OK", "live\n".to_owned()),
        "/health/ready" => match not_ready(deps).await {
            None => ("200 OK", "ready\n".to_owned()),
            Some(reasons) => ("503 Service Unavailable", reasons),
        },
        _ => ("404 Not Found", "not found\n".to_owned()),
    };
    let response = format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

/// `None` when every dependency answers, otherwise one line per failure.
async fn not_ready(deps: &Deps) -> Option<String> {
    match tokio::time::timeout(CHECK_TIMEOUT, sqlx::query("SELECT 1").execute(&deps.db)).await {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("database: {e}\n")),
        Err(_) => Some("database: timed out\n".to_owned()),
    }
}

/// `mh3sp healthcheck`: asks a running server whether it is live. Blocking, no runtime needed.
pub fn check(addr: SocketAddr) -> Result<()> {
    // A server bound to 0.0.0.0 is reached through loopback.
    let addr = match addr.ip().is_unspecified() {
        true => SocketAddr::from(([127, 0, 0, 1], addr.port())),
        false => addr,
    };
    let mut stream = StdTcpStream::connect_timeout(&addr, CHECK_TIMEOUT)
        .with_context(|| format!("connect {addr}"))?;
    stream.set_read_timeout(Some(CHECK_TIMEOUT))?;
    stream.write_all(b"GET /health/live HTTP/1.0\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    match response.split_whitespace().nth(1) {
        Some("200") => Ok(()),
        _ => bail!(
            "not live: {}",
            response.lines().next().unwrap_or("no answer")
        ),
    }
}
