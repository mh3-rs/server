//! The public port (`public_port`, 8280): everything the server says to people and monitors, over plain HTTP. All of it
//! is safe to read, so an operator restricts it with their own proxy if they want to, and nothing on it changes the
//! server (the operator's commands go through [`crate::control`]):
//!
//! - `GET /patch` the players' guide, `GET /patch/<file>` each file of the patch, made on each request from the CA and
//!   the `domain` (`patch.rs`, `../docs/history/patch and CA.md` D12); `GET /errors` the known error codes.
//! - `GET /health/live`: 200 when the server's runtime answers and the database has opened once (at startup the
//!   server retries until it does). The container healthcheck; failing it justifies a restart.
//! - `GET /health/ready`: 200 when the database answers too and the server is configured, 503 (with the reasons)
//!   otherwise. Never ready in maintenance mode, always live.
//!   A database outage doesn't make the server unhealthy: players already in town or on a quest
//!   don't need it, a restart would drop them, and the pool reconnects by itself. It only means
//!   "don't send new players here".
//! - `GET /metrics`: what an operator watches, in JSON (`metrics.rs`); `GET /metrics/prometheus` the same for
//!   Prometheus (the `prometheus` feature).
//!
//! The Nintendo login, which the game calls on port 80, is another port and answers nothing else (`nas.rs`).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::Shared;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>, shutdown: CancellationToken) {
    let app = Router::new()
        .route("/patch", get(patch_page))
        .route("/patch/{*path}", get(patch_file))
        .route("/errors", get(errors_page))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics));
    #[cfg(feature = "prometheus")]
    let app = app.route("/metrics/prometheus", get(prometheus));
    let app = app.with_state(shared);
    let stopped = async move { shutdown.cancelled().await };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(stopped)
        .await
    {
        error!(error = %e, "the public port stopped");
    }
}

/// A plain-text answer: what the operator's commands and the probes read.
pub(crate) fn text(status: StatusCode, body: impl Into<String>) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain")], body.into()).into_response()
}

async fn patch_page(State(shared): State<Arc<Shared>>) -> Response {
    let Some(domain) = &shared.config.domain else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let html = [(header::CONTENT_TYPE, "text/html; charset=utf-8")];
    (html, crate::patch::page(&shared.config.name, domain)).into_response()
}

async fn errors_page(State(shared): State<Arc<Shared>>) -> Response {
    let html = [(header::CONTENT_TYPE, "text/html; charset=utf-8")];
    (html, crate::patch::errors(&shared.config.name)).into_response()
}

async fn patch_file(State(shared): State<Arc<Shared>>, Path(path): Path<String>) -> Response {
    let Some(domain) = &shared.config.domain else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let files =
        crate::tls::ca_der(&shared.config.tls_dir).and_then(|ca| crate::patch::files(domain, &ca));
    let files = match files {
        Ok(files) => files,
        Err(e) => {
            error!(error = %format!("{e:#}"), "the patch");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Some((path, bytes)) = files.into_iter().find(|(p, _)| *p == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let kind = match path.ends_with(".xml") {
        true => "application/xml",
        false => "application/octet-stream",
    };
    let name = path.rsplit('/').next().unwrap_or(&path);
    let headers = [
        (header::CONTENT_TYPE, kind.to_owned()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        ),
    ];
    (headers, bytes).into_response()
}

async fn live(State(shared): State<Arc<Shared>>) -> Response {
    match shared.maintenance().is_some() || shared.db.pool().is_some() {
        true => text(StatusCode::OK, "live\n"),
        false => text(
            StatusCode::SERVICE_UNAVAILABLE,
            "database: not opened yet\n",
        ),
    }
}

async fn ready(State(shared): State<Arc<Shared>>) -> Response {
    match not_ready(&shared).await {
        None => text(StatusCode::OK, "ready\n"),
        Some(reasons) => text(StatusCode::SERVICE_UNAVAILABLE, reasons),
    }
}

async fn metrics(State(shared): State<Arc<Shared>>) -> Response {
    match serde_json::to_string_pretty(&crate::metrics::snapshot(&shared)) {
        Ok(json) => ([(header::CONTENT_TYPE, "application/json")], json + "\n").into_response(),
        Err(e) => {
            error!(error = %e, "the metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(feature = "prometheus")]
async fn prometheus(State(shared): State<Arc<Shared>>) -> Response {
    text(
        StatusCode::OK,
        crate::metrics::prometheus(&crate::metrics::snapshot(&shared)),
    )
}

/// `None` when ready, otherwise one line per reason.
async fn not_ready(shared: &Shared) -> Option<String> {
    if shared.maintenance().is_some() {
        return Some("maintenance\n".to_owned());
    }
    let mut reasons = String::new();
    if !shared.configured() {
        reasons += "not configured: external_address and domain must be set\n";
    }
    if let Some(reason) = shared.db.not_ready().await {
        reasons += &reason;
        reasons += "\n";
    }
    (!reasons.is_empty()).then_some(reasons)
}

/// `mh3sp healthcheck`: asks a running server whether it is live. Blocking, no runtime needed.
pub fn check(addr: SocketAddr) -> Result<()> {
    // A server bound to 0.0.0.0 is reached through loopback.
    let addr = match addr.ip().is_unspecified() {
        true => SocketAddr::from(([127, 0, 0, 1], addr.port())),
        false => addr,
    };
    let mut stream = TcpStream::connect_timeout(&addr, CHECK_TIMEOUT)
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
