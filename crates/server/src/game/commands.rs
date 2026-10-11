//! Chat commands (the user's notes, 2026-10-11): a town or party chat line starting with `/` is for the server, never
//! relayed. The answer reaches the sender alone, from "[MH3SP]". Anyone may `/tell`; the rest is for the admins (the
//! configuration's `admins`, Capcom IDs), and answers "Unknown command" to the others, like a command that doesn't
//! exist. Each admin command is one function, which the API (`api.rs`) calls too.
//!
//! - `/tell <player> <message>`: to that player alone, as chat from the sender (a player: a Capcom ID, or a name
//!   when only one player has it); not delivered when the target has the sender on their black list.
//! - `/announce <text>`, `/stop <delay|cancel>`, `/kick <player>`, `/ban <Capcom ID> <reason>` (the player's
//!   console, kicked), `/unban <Capcom ID>`, `/bot add` (a bot in my party), `/bot remove
//!   <bot>`.

use std::sync::Arc;

use bytes::Bytes;
use tracing::info;
use wire::Frame;

use crate::Shared;

/// A command's answer to its sender (`None`: not a command, chat as usual).
pub fn run(shared: &Arc<Shared>, me: &str, name: &Bytes, line: &[u8]) -> Option<String> {
    // Decoded leniently: a command in the game's own encoding must not fall back to chat (a private `/tell` shown to
    // everyone).
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches('\0').trim().strip_prefix('/')?;
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    let admin = shared
        .config
        .admins
        .iter()
        .any(|a| a.eq_ignore_ascii_case(me));
    Some(match (command.to_ascii_lowercase().as_str(), admin) {
        ("tell", _) => tell(shared, me, name, rest),
        ("announce", true) if !rest.is_empty() => {
            let players = super::announce(shared, rest);
            info!(admin = me, players, "announcement");
            format!("Sent to {players} players.")
        }
        ("stop", true) if rest == "cancel" => match crate::stop::cancel(shared) {
            true => "Stop cancelled.".to_owned(),
            false => "No stop to cancel.".to_owned(),
        },
        ("stop", true) => match crate::stop::parse(rest) {
            Some(delay) => {
                info!(admin = me, "stop asked");
                crate::stop::begin(shared, delay);
                format!("Stopping in {}.", crate::stop::human(delay))
            }
            None => "/stop 5min, 1h30m, 30s, or cancel".to_owned(),
        },
        ("kick", true) => match find(shared, rest) {
            Ok(target) => {
                shared.players.kick(&target);
                info!(admin = me, player = %target, "kicked");
                format!("{target} kicked.")
            }
            Err(refusal) => refusal,
        },
        ("ban", true) => match rest.split_once(' ') {
            Some((who, reason)) => later(shared, me, ban(shared, who, reason.trim(), None), "ban"),
            None => "/ban <player> <reason>".to_owned(),
        },
        ("unban", true) if !rest.is_empty() => later(shared, me, unban(shared, rest), "unban"),
        #[cfg(feature = "bot")]
        ("bot", true) => match rest.split_once(' ').unwrap_or((rest, "")) {
            ("add", "") => match super::fmp::add_bot(shared, me) {
                Ok(bot) => format!("Bot {bot} joined your party."),
                Err(refusal) => format!("No bot: {refusal:?}."),
            },
            ("remove", bot) => match super::fmp::remove_bot(shared, bot.trim()) {
                true => format!("Bot {} left.", bot.trim()),
                false => "No such bot.".to_owned(),
            },
            _ => "/bot add, or /bot remove <bot>".to_owned(),
        },
        _ => "Unknown command.".to_owned(),
    })
}

/// Runs a command that needs the database in its own task (chat is handled without waiting): its answer reaches the
/// sender when done, and the chat's own answer is empty.
fn later(
    shared: &Arc<Shared>,
    me: &str,
    command: impl Future<Output = Result<String, String>> + Send + 'static,
    name: &'static str,
) -> String {
    let (shared, me) = (shared.clone(), me.to_owned());
    tokio::spawn(async move {
        let answer = command.await.unwrap_or_else(|refusal| refusal);
        info!(player = %me, command = name, "done");
        from_server(&shared, &me, &answer);
    });
    String::new()
}

/// Bans the console of a player, by Capcom ID only (a name could hit someone else), and kicks them; `by`: the API's
/// user, `None` for the operator.
pub fn ban(
    shared: &Arc<Shared>,
    capcom_id: &str,
    reason: &str,
    by: Option<uuid::Uuid>,
) -> impl Future<Output = Result<String, String>> + Send + 'static {
    let target = capcom_id.trim().to_ascii_uppercase();
    let (shared, reason) = (shared.clone(), reason.to_owned());
    async move {
        let db = shared.db.pool().ok_or(UNAVAILABLE)?;
        match crate::access::ban(db, &target, &reason, by).await {
            Ok(true) => {
                shared.players.kick(&target);
                info!(player = %target, "banned");
                Ok(format!("{target} banned."))
            }
            Ok(false) => Err(format!("No Capcom ID {target}.")),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "database failed, ban refused");
                Err(UNAVAILABLE.to_owned())
            }
        }
    }
}

/// Lifts the bans on a player's console (by Capcom ID: a banned player is offline).
pub fn unban(
    shared: &Arc<Shared>,
    capcom_id: &str,
) -> impl Future<Output = Result<String, String>> + Send + 'static {
    let (shared, target) = (shared.clone(), capcom_id.trim().to_ascii_uppercase());
    async move {
        let db = shared.db.pool().ok_or(UNAVAILABLE)?;
        match crate::access::unban(db, &target, "lifted by an admin").await {
            Ok(true) => {
                info!(player = %target, "unbanned");
                Ok(format!("{target} unbanned."))
            }
            Ok(false) => Err(format!("{target} isn't banned.")),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "database failed, unban refused");
                Err(UNAVAILABLE.to_owned())
            }
        }
    }
}

/// A refusal for want of the database (the API answers it with 503).
pub const UNAVAILABLE: &str = "Not available right now.";

/// `/tell <player> <message>`: answered once the target's black list is read; a blocked sender is told it was sent
/// (as mail is: a block isn't revealed).
fn tell(shared: &Arc<Shared>, me: &str, name: &Bytes, rest: &str) -> String {
    let Some((who, message)) = rest.split_once(' ') else {
        return "/tell <player> <message>".to_owned();
    };
    let target = match find(shared, who) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    let (shared2, me2, name, message) = (
        shared.clone(),
        me.to_owned(),
        name.clone(),
        message.trim().to_owned(),
    );
    let deliver = async move {
        let db = shared2.db.pool().ok_or(UNAVAILABLE)?;
        let blocked = crate::accounts::blocks_capcom_id(db, &target, &me2)
            .await
            .map_err(|e| {
                tracing::error!(error = %format!("{e:#}"), "database failed, tell refused");
                UNAVAILABLE.to_owned()
            })?;
        if !blocked {
            let on_quest = shared2.world().on_quest(&target);
            // On a quest, chat reaches a player as party chat, from a member: themselves, as announcements do.
            let sender = if on_quest { &target } else { &me2 };
            for frame in super::chat(&message, sender, &name, on_quest) {
                shared2.players.send(&target, frame);
            }
        }
        let shown = display(&shared2, &target);
        Ok(format!("To {}: sent.", String::from_utf8_lossy(&shown)))
    };
    later(shared, me, deliver, "tell")
}

/// A logged-in player, by Capcom ID or by a name only one of them has (ASCII case ignored).
pub fn find(shared: &Shared, who: &str) -> Result<String, String> {
    let world = shared.world();
    let everyone = world.everyone();
    if let Some((id, _)) = everyone.iter().find(|(id, _)| id.eq_ignore_ascii_case(who)) {
        return Ok((*id).to_owned());
    }
    let named: Vec<_> = everyone
        .iter()
        .filter(|(_, p)| p.name.eq_ignore_ascii_case(who.as_bytes()))
        .collect();
    match named[..] {
        [(id, _)] => Ok((*id).to_owned()),
        [] => Err(format!("No player {who} online.")),
        _ => Err(format!("Several players named {who}: use a Capcom ID.")),
    }
}

/// A player's name, or their Capcom ID once gone.
fn display(shared: &Shared, capcom_id: &str) -> Bytes {
    let world = shared.world();
    let everyone = world.everyone();
    everyone
        .iter()
        .find(|(id, _)| *id == capcom_id)
        .map_or_else(
            || Bytes::copy_from_slice(capcom_id.as_bytes()),
            |(_, p)| p.name.clone(),
        )
}

/// Who is online, the earliest arrived first (`GET /api/players`).
pub fn online(shared: &Shared) -> Vec<shared::Player> {
    let world = shared.world();
    world
        .everyone()
        .into_iter()
        .map(|(id, place)| shared::Player {
            capcom_id: id.to_owned(),
            name: String::from_utf8_lossy(&place.name).into_owned(),
            server: super::world::SERVERS
                .get(place.server as usize - 1)
                .map_or_else(|| place.server.to_string(), |(name, _)| (*name).to_owned()),
        })
        .collect()
}

/// A message to one player from "[MH3SP]" (`POST /api/tell`).
pub fn from_server(shared: &Shared, target: &str, text: &str) {
    for frame in answer(shared, target, text) {
        shared.players.send(target, frame);
    }
}

/// The answer's frames for its sender.
pub fn answer(shared: &Shared, me: &str, text: &str) -> Vec<Frame> {
    let on_quest = shared.world().on_quest(me);
    super::announcement(text, on_quest.then_some(me))
}
