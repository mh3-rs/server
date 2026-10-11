//! Our own Nintendo login (NAS, architecture decision 13, `../docs/history/nintendo login.md`). Before OPN the game posts `/ac` three
//! times over plain HTTP (MH3SP's patch turns its `https` into `http`): `acctcreate` when its NAND holds no NAS user
//! id, `login`, then `svcloc`. The `servicetoken` `svcloc` hands out comes back at OPN (`ReqAuthenticationToken`)
//! and at LMP's first visit (`ReqLoginInfo` item 0x0a): it tells the server which Wii the console is on, its MAC and
//! serial (`devices`). A token lives in memory, used up at LMP or after [`TOKEN_TTL`]. Never logged: the token, the
//! MAC, the serial.
//!
//! The replies carry only what the game reads (`dwc_nasfunc.c`): `returncd` (1–99 succeeds), the new `userid` on
//! `acctcreate` (written to NAND), the `servicetoken` on `svcloc`. Open in maintenance mode: the console must reach
//! OPN to read the maintenance text.
//!
//! The same port, the domain's port 80, serves the players' patch (`patch.rs`, `../docs/history/patch and CA.md`
//! D12): a page at `/patch`, each file at `/patch/<path>`, made on each request from the CA and the `domain`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::Shared;
use crate::accounts::{hmac, random_id};

/// How long a `servicetoken` waits for its console at LMP (D3).
pub const TOKEN_TTL: Duration = Duration::from_secs(300);
/// Tokens waiting at once.
///
/// ponytail: a flood of `svcloc` evicts real consoles' tokens (they log in again); per-address limits if it happens.
pub const TOKENS: u64 = 10_000;
/// Nintendo's form: `NDS` and 80 characters, the length every recording has (the replays swap it in place).
const TOKEN_LEN: usize = 80;

/// Tokens waiting for their console: token → its Wii's MAC and serial HMAC.
pub type Tokens = moka::sync::Cache<String, Vec<u8>>;

pub fn tokens() -> Tokens {
    moka::sync::Cache::builder()
        .time_to_live(TOKEN_TTL)
        .max_capacity(TOKENS)
        .build()
}

pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>, shutdown: CancellationToken) {
    let app = Router::new()
        .route("/ac", post(ac))
        .route("/patch", get(patch_page))
        .route("/errors", get(errors_page))
        .route("/patch/{*path}", get(patch_file))
        // A form is a few hundred bytes.
        .layer(DefaultBodyLimit::max(4096))
        .with_state(shared);
    let stopped = async move { shutdown.cancelled().await };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(stopped)
        .await
    {
        error!(error = %e, "NAS stopped");
    }
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

async fn ac(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    // A refusal is the game's error 234xx: say why, never the form's values (the MAC, the serial).
    let Some(form) = wire::nas::decode(&body) else {
        warn!("NAS: refused, the form doesn't decode");
        return StatusCode::BAD_REQUEST.into_response();
    };
    let field = |key: &str| {
        form.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_slice())
    };
    let action = field("action").unwrap_or_default();
    debug!(action = %String::from_utf8_lossy(action), "NAS");
    let reply = match action {
        b"acctcreate" => {
            let user_id = user_id();
            wire::nas::encode(&[("returncd", b"002"), ("userid", user_id.as_bytes())])
        }
        b"login" => wire::nas::encode(&[("returncd", b"001")]),
        b"svcloc" => {
            // `macadr`: 12 lower-case hex digits; `csnum` is absent when the Wii has no serial set.
            let Some(mac) =
                field("macadr").filter(|m| m.len() == 12 && m.iter().all(u8::is_ascii_hexdigit))
            else {
                warn!("NAS: refused, svcloc without a MAC address");
                return StatusCode::BAD_REQUEST.into_response();
            };
            let serial = field("csnum").unwrap_or_default();
            let token = format!("NDS{}", random_id(TOKEN_LEN));
            // No secret in maintenance mode: no console gets past OPN anyway.
            if let Some(secret) = &shared.config.secret {
                let device = format!(
                    "{}{}",
                    String::from_utf8_lossy(mac).to_ascii_lowercase(),
                    String::from_utf8_lossy(serial)
                );
                shared.nas.insert(token.clone(), hmac(secret, &device));
            }
            wire::nas::encode(&[("returncd", b"007"), ("servicetoken", token.as_bytes())])
        }
        _ => {
            // A client's bytes: escaped and cut, so a log line can't be forged or flooded.
            let action = String::from_utf8_lossy(&action[..action.len().min(32)]);
            warn!(action = %action.escape_debug(), "NAS: refused, an unknown action");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    ([(header::CONTENT_TYPE, "text/plain")], reply).into_response()
}

/// A NAS user id: 13 random digits (D7; the bias of the modulo is under 10⁻⁶).
fn user_id() -> String {
    let mut bytes = [0u8; 8];
    openssl::rand::rand_bytes(&mut bytes).expect("OpenSSL random source");
    format!("{:013}", u64::from_le_bytes(bytes) % 10_000_000_000_000)
}
