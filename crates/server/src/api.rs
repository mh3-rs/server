//! The API (the user's notes, 2026-10-11): the server is the only one to reach the database; the admin CLI (`cli`)
//! and the website are its clients. JSON, the messages in the `shared` crate. A request carries its token in the
//! `mh3sp_token` cookie (the website) or an `Authorization: Bearer` header (the CLI); tokens come from
//! `mh3sp user add` (`users.rs`). On `api_address` (127.0.0.1 by default: behind a reverse proxy, which brings the
//! TLS a `Secure` cookie needs), `api_port`.
//!
//! - `POST /api/session` (email and password: a session's token), `POST /api/logout` (the token revoked).
//! - `GET /api/me`: who the token belongs to (any user).
//! - Admins: `GET /api/players`, `GET /api/bans`; `POST /api/announce`, `/api/stop`, `/api/kick`, `/api/tell`, `/api/ban`, `/api/unban`, the chat
//!   commands'; `/api/bot/add`, `/api/bot/remove` (404 without the `bot` feature)
//!   (`game::commands`).

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use shared::{
    Announce, Ban, BanInfo, Kick, Me, Outcome, Player, Session, SignIn, Stop, Tell, Unban,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::Shared;
use crate::game::commands;
use crate::users::{self, Role, User};

type Refusal = (StatusCode, Json<Outcome>);

fn refusal(status: StatusCode, message: impl Into<String>) -> Refusal {
    let message = message.into();
    (status, Json(Outcome { message }))
}

/// A command's refusal: 503 for want of the database, 404 otherwise (no such player, nothing to undo).
fn refused(message: String) -> Refusal {
    match message == commands::UNAVAILABLE {
        true => refusal(StatusCode::SERVICE_UNAVAILABLE, message),
        false => refusal(StatusCode::NOT_FOUND, message),
    }
}

/// The longest announcement or message, in characters: 10 lines of chat. Each line is a frame per player, and a
/// mailbox of 256 full drops its player: a long paste must not drop everyone.
const MAX_TEXT: usize = 300;

/// A text to send to players: not empty, not too long.
fn text(text: &str) -> Result<&str, Refusal> {
    match text.trim() {
        "" => Err(refusal(StatusCode::BAD_REQUEST, "No text.")),
        t if t.chars().count() > MAX_TEXT => Err(refusal(
            StatusCode::BAD_REQUEST,
            format!("Too long: {MAX_TEXT} characters at most."),
        )),
        t => Ok(t),
    }
}

fn done(message: impl Into<String>) -> Json<Outcome> {
    let message = message.into();
    Json(Outcome { message })
}

pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>, shutdown: CancellationToken) {
    let app = Router::new()
        .route("/api/session", post(session))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/players", get(players))
        .route("/api/announce", post(announce))
        .route("/api/stop", post(stop))
        .route("/api/kick", post(kick))
        .route("/api/tell", post(tell))
        .route("/api/ban", post(ban))
        .route("/api/bans", get(bans))
        .route("/api/bot/add", post(bot_add))
        .route("/api/bot/remove", post(bot_remove))
        .route("/api/unban", post(unban))
        .layer(Extension(Gate::new()))
        .with_state(shared);
    let stopped = async move { shutdown.cancelled().await };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(stopped)
        .await
    {
        error!(error = %e, "API stopped");
    }
}

/// The request's token: the cookie, or a Bearer header.
fn token(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = || {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .find_map(|c| {
                c.trim()
                    .strip_prefix(shared::TOKEN_COOKIE)?
                    .strip_prefix('=')
            })
    };
    bearer.or_else(cookie).map(str::trim)
}

/// The signed-in user, an admin when `admin`.
async fn user(shared: &Shared, headers: &HeaderMap, admin: bool) -> Result<User, Refusal> {
    let Some(token) = token(headers) else {
        return Err(refusal(StatusCode::UNAUTHORIZED, "No token."));
    };
    let Some(db) = shared.db.pool() else {
        return Err(refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "Not available right now.",
        ));
    };
    let user = match users::authenticate(db, token).await {
        Ok(Some(user)) => user,
        Ok(None) => return Err(refusal(StatusCode::UNAUTHORIZED, "Invalid token.")),
        Err(e) => {
            error!(error = %format!("{e:#}"), "database failed, API request refused");
            return Err(refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "Not available right now.",
            ));
        }
    };
    match admin && user.role != Role::Admin {
        true => Err(refusal(StatusCode::FORBIDDEN, "Admins only.")),
        false => Ok(user),
    }
}

async fn me(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Result<Json<Me>, Refusal> {
    let user = user(&shared, &headers, false).await?;
    Ok(Json(Me {
        id: user.public_id.to_string(),
        role: user.role.name().to_owned(),
    }))
}

async fn players(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
) -> Result<Json<Vec<Player>>, Refusal> {
    user(&shared, &headers, true).await?;
    Ok(Json(commands::online(&shared)))
}

async fn announce(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Announce>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    let players = crate::game::announce(&shared, text(&r.text)?);
    info!(user = %user.public_id, players, "announcement");
    Ok(done(format!("Sent to {players} players.")))
}

async fn stop(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Stop>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    if r.delay.trim() == "cancel" {
        return match crate::stop::cancel(&shared) {
            true => Ok(done("Stop cancelled.")),
            false => Err(refusal(StatusCode::CONFLICT, "No stop to cancel.")),
        };
    }
    let Some(delay) = crate::stop::parse(&r.delay) else {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "A delay like 5min, 1h30m, 30s, or cancel.",
        ));
    };
    info!(user = %user.public_id, "stop asked");
    crate::stop::begin(&shared, delay);
    Ok(done(format!("Stopping in {}.", crate::stop::human(delay))))
}

async fn kick(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Kick>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    let target =
        commands::find(&shared, r.player.trim()).map_err(|m| refusal(StatusCode::NOT_FOUND, m))?;
    shared.players.kick(&target);
    info!(user = %user.public_id, player = %target, "kicked");
    Ok(done(format!("{target} kicked.")))
}

async fn tell(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Tell>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    let message = text(&r.text)?;
    info!(user = %user.public_id, "tell asked");
    let target =
        commands::find(&shared, r.player.trim()).map_err(|m| refusal(StatusCode::NOT_FOUND, m))?;
    commands::from_server(&shared, &target, message);
    Ok(done(format!("Sent to {target}.")))
}

async fn ban(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Ban>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    if r.reason.trim().is_empty() {
        return Err(refusal(StatusCode::BAD_REQUEST, "No reason."));
    }
    info!(user = %user.public_id, "ban asked");
    commands::ban(&shared, &r.player, r.reason.trim(), Some(user.public_id))
        .await
        .map(done)
        .map_err(refused)
}

async fn unban(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<Unban>,
) -> Result<Json<Outcome>, Refusal> {
    let user = user(&shared, &headers, true).await?;
    info!(user = %user.public_id, "unban asked");
    commands::unban(&shared, &r.player)
        .await
        .map(done)
        .map_err(refused)
}

async fn bans(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
) -> Result<Json<Vec<BanInfo>>, Refusal> {
    user(&shared, &headers, true).await?;
    let db = shared
        .db
        .pool()
        .ok_or_else(|| refused(commands::UNAVAILABLE.into()))?;
    let bans = crate::access::bans(db).await.map_err(|e| {
        error!(error = %format!("{e:#}"), "database failed, API request refused");
        refused(commands::UNAVAILABLE.into())
    })?;
    Ok(Json(
        bans.into_iter()
            .map(|(id, ids, reason, since)| BanInfo {
                id: id.to_string(),
                capcom_ids: ids
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                reason,
                since,
            })
            .collect(),
    ))
}

/// Sign-in attempts per email, for 15 minutes: five failures, and it waits (argon2 is slow on purpose, but a password
/// could still be guessed at leisure). Counted before checking, so parallel requests can't all slip under the limit;
/// a success clears the count. And at most one argon2 check per CPU at once (each takes ~19 MiB): a flood waits
/// instead of eating the memory.
#[derive(Clone)]
struct Gate {
    attempts: moka::sync::Cache<String, u32>,
    hashing: Arc<tokio::sync::Semaphore>,
}

impl Gate {
    const MAX: u32 = 5;

    fn new() -> Self {
        let attempts = moka::sync::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(15 * 60))
            .max_capacity(100_000)
            .build();
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            attempts,
            hashing: Arc::new(tokio::sync::Semaphore::new(cpus)),
        }
    }
}

async fn session(
    State(shared): State<Arc<Shared>>,
    Extension(gate): Extension<Gate>,
    Json(r): Json<SignIn>,
) -> Result<Json<Session>, Refusal> {
    let email = r.email.trim().to_lowercase();
    // The longest an email can be (RFC 5321), and a generous password: bounded keys and work.
    if email.len() > 254 || r.password.len() > 1024 {
        return Err(refusal(StatusCode::BAD_REQUEST, "Too long."));
    }
    let attempts = gate
        .attempts
        .entry(email.clone())
        .and_upsert_with(|n| n.map_or(1, |n| n.into_value() + 1))
        .into_value();
    if attempts > Gate::MAX {
        crate::metrics::add(&shared.metrics.limited, 1);
        return Err(refusal(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts: try again later.",
        ));
    }
    let db = shared
        .db
        .pool()
        .ok_or_else(|| refused(commands::UNAVAILABLE.into()))?;
    let _permit = gate
        .hashing
        .acquire()
        .await
        .map_err(|_| refused(commands::UNAVAILABLE.into()))?;
    match users::sign_in(db, &email, &r.password).await {
        Ok(Some(token)) => {
            gate.attempts.invalidate(&email);
            Ok(Json(Session { token }))
        }
        Ok(None) => Err(refusal(
            StatusCode::UNAUTHORIZED,
            "Wrong email or password.",
        )),
        Err(e) => {
            error!(error = %format!("{e:#}"), "database failed, sign-in refused");
            Err(refused(commands::UNAVAILABLE.into()))
        }
    }
}

async fn logout(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
) -> Result<Json<Outcome>, Refusal> {
    user(&shared, &headers, false).await?;
    let token = token(&headers).unwrap_or_default();
    let db = shared
        .db
        .pool()
        .ok_or_else(|| refused(commands::UNAVAILABLE.into()))?;
    users::sign_out(db, token).await.map_err(|e| {
        error!(error = %format!("{e:#}"), "database failed, sign-out refused");
        refused(commands::UNAVAILABLE.into())
    })?;
    Ok(done("Signed out."))
}

#[cfg(feature = "bot")]
async fn bot_add(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<shared::BotAdd>,
) -> Result<Json<Outcome>, Refusal> {
    user(&shared, &headers, true).await?;
    let player =
        commands::find(&shared, r.player.trim()).map_err(|m| refusal(StatusCode::NOT_FOUND, m))?;
    crate::game::fmp::add_bot(&shared, &player)
        .map(|bot| done(format!("Bot {bot} joined {player}'s party.")))
        .map_err(|refusal_| refusal(StatusCode::CONFLICT, format!("No bot: {refusal_:?}.")))
}

#[cfg(feature = "bot")]
async fn bot_remove(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(r): Json<shared::BotRemove>,
) -> Result<Json<Outcome>, Refusal> {
    user(&shared, &headers, true).await?;
    match crate::game::fmp::remove_bot(&shared, r.bot.trim()) {
        true => Ok(done(format!("Bot {} left.", r.bot.trim()))),
        false => Err(refusal(StatusCode::NOT_FOUND, "No such bot.")),
    }
}

/// Without the quest bots: as if the route didn't exist.
#[cfg(not(feature = "bot"))]
async fn bot_add() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[cfg(not(feature = "bot"))]
async fn bot_remove() -> StatusCode {
    StatusCode::NOT_FOUND
}
