//! Game logic: what the server answers. No sockets: handlers get a frame and return the frames to send back;
//! `net` does the I/O.

pub mod commands;
pub mod fmp;
pub mod lmp;
pub mod opn;
pub mod players;
pub mod profile;
pub mod world;

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use bytes::BufMut;
use bytes::Bytes;
use sqlx::SqlitePool;
use tracing::{error, warn};
use wire::packets::{AnsServerTime, NtcCircleChat, NtcLayerChat, Packet, ReqConnection};
use wire::{DecodeError, EncodeError, Frame, Item, Kind, PacketId, PatData, PatWrite};

use crate::Shared;
use crate::accounts::Console;
use fmp::Fmp;
use lmp::Lmp;
use opn::Opn;
use players::Mailbox;
use profile::Profile;

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

/// A connection's state, by role.
pub enum Handler {
    Opn(Opn),
    Lmp(Lmp),
    Fmp(Fmp),
}

impl Handler {
    /// The handler, and what the server sends first.
    pub fn start(role: Role, mailbox: Mailbox) -> (Self, Handled) {
        match role {
            Role::Opn => {
                let (opn, frames) = Opn::start(mailbox.ip);
                (Handler::Opn(opn), frames)
            }
            Role::Lmp => (
                Handler::Lmp(Lmp::new(mailbox.ip)),
                req_connection().map(|f| vec![f]),
            ),
            Role::Fmp => (
                Handler::Fmp(Fmp::new(mailbox)),
                req_connection().map(|f| vec![f]),
            ),
        }
    }

    /// The encryption key OPN gave the console, if any.
    pub fn given_key(&self) -> Option<crate::keys::Key> {
        match self {
            Handler::Opn(opn) => opn.key,
            _ => None,
        }
    }

    /// A database failure never drops the player (architecture decision 9): a request is refused with a message,
    /// anything else goes unanswered; everything that lives in memory goes on.
    pub async fn handle(&mut self, shared: &std::sync::Arc<Shared>, req: &Frame) -> Handled {
        let handled = match self {
            Handler::Opn(opn) => opn.handle(shared, req).await,
            Handler::Lmp(lmp) => lmp.handle(shared, req).await,
            Handler::Fmp(fmp) => fmp.handle(shared, req).await,
        };
        match handled {
            Err(Fault::Client(_)) if unsupported(req.id) => {
                warn!(request = %req.id, "request not supported, refused");
                alert(answer(req.id), req.seq, crate::text::Text::NotSupported)
            }
            Err(Fault::Database(e)) => {
                error!(error = %format!("{e:#}"), request = %req.id, "database failed, request refused");
                match req.id.kind() {
                    Some(Kind::Req) => {
                        alert(answer(req.id), req.seq, crate::text::Text::Unavailable)
                    }
                    _ => Ok(Vec::new()),
                }
            }
            other => other,
        }
    }

    /// The console announced it leaves (`ReqShut`).
    pub fn leave(&mut self, shared: &Shared) {
        match self {
            Handler::Opn(_) => {}
            Handler::Lmp(lmp) => lmp.leave(shared),
            Handler::Fmp(fmp) => fmp.leave(shared),
        }
    }

    /// The connection is closed, whatever the reason.
    pub fn close(&mut self, shared: &Shared) {
        if let Handler::Fmp(fmp) = self {
            fmp.close(shared);
        }
    }
}

/// A logged-in console, owned by its connection; handed off to the next one by PAT ticket.
#[derive(Clone, Debug)]
pub struct Session {
    pub console: Console,
    pub capcom_id: Option<String>,
    /// The hunter's name, as the game sends it (bytes: it may be Shift-JIS).
    pub name: Bytes,
    /// The world server picked (1-based, `FmpInfo2`).
    pub world_server: Option<u32>,
    /// The hunter's public profile block, written by the console (`UserBinarySet`).
    pub profile: Profile,
}

impl Session {
    pub fn new(console: Console) -> Self {
        Session {
            console,
            capcom_id: None,
            name: Bytes::new(),
            world_server: None,
            profile: Profile::default(),
        }
    }
}

/// Why a connection is dropped.
#[derive(Debug)]
pub enum Fault {
    /// The console sent something wrong (logged at `warn`).
    Client(anyhow::Error),
    /// The server is at fault (logged at `error`).
    Server(anyhow::Error),
    /// The database failed (architecture decision 9): logged at `error`, the request refused with a message, the
    /// player kept (`Handler::handle`).
    Database(anyhow::Error),
}

impl From<DecodeError> for Fault {
    fn from(e: DecodeError) -> Self {
        Fault::Client(e.into())
    }
}

impl From<EncodeError> for Fault {
    fn from(e: EncodeError) -> Self {
        Fault::Server(e.into())
    }
}

impl From<sqlx::Error> for Fault {
    fn from(e: sqlx::Error) -> Self {
        Fault::Database(e.into())
    }
}

pub type Handled = Result<Vec<Frame>, Fault>;

pub fn frame<P: Packet>(seq: u16, packet: &P) -> Result<Frame, Fault> {
    let mut payload = Vec::new();
    packet.encode(&mut payload)?;
    Ok(Frame {
        seq,
        id: P::ID,
        payload: payload.into(),
    })
}

/// An announcement to every logged-in player (M5 D3), as MH3SP sends them: chat from a made-up sender, the text in
/// lines of at most 30 characters (the chat's width), cut between words (a longer word is cut). Town chat doesn't
/// show in a hunt (lab run 15): players on a quest get party chat (`NtcCircleChat`), which the game only shows from a
/// party member (`uniqueIdToMember` drops the rest): sent as from themselves, named "[MH3SP]". How many players.
pub fn announce(shared: &crate::Shared, text: &str) -> usize {
    let town = announcement(text, None);
    let world = shared.world();
    shared
        .players
        .broadcast(|capcom_id| match world.on_quest(capcom_id) {
            true => announcement(text, Some(capcom_id)),
            false => town.clone(),
        })
}

/// An announcement's frames: town chat, or party chat as from `party` (a member's Capcom ID).
pub fn announcement(text: &str, party: Option<&str>) -> Vec<Frame> {
    chat(
        text,
        party.unwrap_or("000000"),
        &Bytes::from_static(b"[MH3SP]"),
        party.is_some(),
    )
}

/// Chat lines of 30 characters as from `sender` (a Capcom ID: in a party, a member's) under `name`: town chat, or
/// party chat.
pub fn chat(text: &str, sender: &str, name: &Bytes, party: bool) -> Vec<Frame> {
    const WIDTH: usize = 30;
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while !word.is_empty() {
            let line_len = lines.last().map(|l| l.chars().count());
            match line_len {
                Some(n) if n + 1 + word.len() <= WIDTH => {
                    let line = lines.last_mut().expect("a line");
                    line.push(' ');
                    line.extend(word.drain(..));
                }
                _ => lines.push(word.drain(..word.len().min(WIDTH)).collect()),
            }
        }
    }
    let info = PatData::new()
        .with(0x01, Item::Long(0x00ff_fff0))
        .with(0x02, Item::Long(0))
        .with(
            0x03,
            Item::String(Bytes::copy_from_slice(sender.as_bytes())),
        )
        .with(0x04, Item::String(name.clone()));
    lines
        .into_iter()
        .filter_map(|line| {
            let (info, text) = (info.clone(), line.into());
            match party {
                true => frame(0, &NtcCircleChat { info, text }),
                false => frame(
                    0,
                    &NtcLayerChat {
                        channel: 1,
                        info,
                        text,
                    },
                ),
            }
            .ok()
        })
        .collect()
}

pub fn reply<P: Packet>(seq: u16, packet: &P) -> Handled {
    Ok(vec![frame(seq, packet)?])
}

/// `AnsServerTime`: seconds since the epoch, twice.
pub fn server_time(seq: u16) -> Handled {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    reply(
        seq,
        &AnsServerTime {
            server_time: now,
            current_time: now,
        },
    )
}

/// An answer refused with a message the console shows: the answer's id with kind byte `0x01`, then a flag (0 would
/// hide the message) and the text.
pub fn alert(answer: PacketId, seq: u16, message: crate::text::Text) -> Handled {
    let text = crate::text::text(message, crate::text::Language::default());
    let mut payload = Vec::new();
    payload.put_u32(1);
    payload.put_lp2_string(text.as_bytes())?;
    Ok(vec![Frame {
        seq,
        id: PacketId(answer.0 | 0x01),
        payload: payload.into(),
    }])
}

/// A request's answer id (kind byte `0x02`).
fn answer(request: PacketId) -> PacketId {
    PacketId(request.0 & !0xff00 | 0x0200)
}

/// Requests in the game's table a console never sends: the server sends them (`ReqTicket2`, `ReqWarning`), or only a
/// Japanese disc would (the four `Vulgarity…JAP`, absent from the PAL and US tables; such a disc is refused at OPN).
const NEVER_SENT: [u32; 6] = [
    0x6031_0100,
    0x6040_0100,
    0x6250_0100,
    0x6251_0100,
    0x6252_0100,
    0x6253_0100,
];

/// A request the game knows that the server has no type for (review, 2026-10-11, `docs/unk.md`): refused with a
/// message, which cancels only that request on the console, rather than a dropped connection (M5 D15's rule). The
/// alert, not status `0xFF`: the console drops itself on that one.
fn unsupported(id: PacketId) -> bool {
    id.kind() == Some(Kind::Req)
        && id.entry().is_some()
        && !NEVER_SENT.contains(&id.0)
        && !wire::packets::TYPED.iter().any(|&(typed, _)| typed == id)
}

pub fn unexpected(id: PacketId) -> Fault {
    Fault::Client(anyhow!("unexpected packet {id}"))
}

/// The connection's opening (OPN's after its disc checks).
fn req_connection() -> Result<Frame, Fault> {
    frame(0, &ReqConnection { unk: 0 })
}

/// Who may reach LMP's first visit (the review, 2026-10-11): OPN runs the disc, patch and access checks, so a first
/// visit (a support code checked, or a console created) comes from an address that passed them lately; and an address
/// creates a few consoles an hour at most. Loopback is this machine (the lab, the tests): neither applies.
pub struct Admission {
    passed: moka::sync::Cache<std::net::IpAddr, ()>,
    created: moka::sync::Cache<std::net::IpAddr, u32>,
    /// `Config::loopback_exempt`.
    loopback_exempt: bool,
}

/// How long after OPN an address may come to LMP: seconds in practice.
const PASSED_FOR: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// New consoles per address (provisional, the review's limits): a household's few, not a loop. The hour restarts
/// with each creation.
const CREATED_PER_HOUR: u32 = 5;

impl Admission {
    pub fn new(loopback_exempt: bool) -> Self {
        Admission {
            passed: moka::sync::Cache::builder()
                .time_to_live(PASSED_FOR)
                .build(),
            created: moka::sync::Cache::builder()
                .time_to_live(std::time::Duration::from_secs(60 * 60))
                .build(),
            loopback_exempt,
        }
    }

    fn exempt(&self, ip: std::net::IpAddr) -> bool {
        self.loopback_exempt && ip.is_loopback()
    }

    /// The address passed OPN's checks.
    pub fn passed(&self, ip: std::net::IpAddr) {
        self.passed.insert(ip, ());
    }

    pub fn may_log_in(&self, ip: std::net::IpAddr) -> bool {
        self.exempt(ip) || self.passed.contains_key(&ip)
    }

    /// Whether the address may create one more console, counted if so.
    pub fn may_create(&self, ip: std::net::IpAddr) -> bool {
        if self.exempt(ip) {
            return true;
        }
        let created = self.created.get(&ip).unwrap_or(0);
        if created >= CREATED_PER_HOUR {
            return false;
        }
        self.created.insert(ip, created + 1);
        true
    }
}

/// Whether a console may play here. Everyone until the website's access modes (M8).
pub async fn allowed(_db: &SqlitePool, _support_code: Option<&[u8]>) -> sqlx::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 33 untyped requests of the review (`docs/unk.md`): 6 never sent, 4 typed since, 23 refused with the alert.
    #[test]
    fn unknown_requests_are_refused() {
        let refused: Vec<_> = PacketId::all().filter(|&id| unsupported(id)).collect();
        assert_eq!(refused.len(), 23, "{refused:?}");
        assert!(refused.contains(&PacketId(0x6535_0100)));
        assert!(!unsupported(PacketId(0x6031_0100)));
        assert!(!unsupported(wire::packets::ReqShut::ID));
        assert!(!unsupported(PacketId(0x6014_0100)));
    }

    #[test]
    fn a_first_visit_needs_opn_and_creations_are_capped() {
        let admission = Admission::new(true);
        let (ip, local) = (
            std::net::IpAddr::from([10, 0, 0, 1]),
            std::net::IpAddr::from([127, 0, 0, 1]),
        );
        assert!(!admission.may_log_in(ip));
        admission.passed(ip);
        assert!(admission.may_log_in(ip));
        assert!((0..CREATED_PER_HOUR).all(|_| admission.may_create(ip)));
        assert!(!admission.may_create(ip));
        assert!(!admission.may_create(ip));
        // Another address has its own count; loopback is the lab's.
        assert!(admission.may_create(std::net::IpAddr::from([10, 0, 0, 2])));
        assert!(admission.may_log_in(local));
        assert!((0..=CREATED_PER_HOUR).all(|_| admission.may_create(local)));
    }

    #[test]
    fn announcements_fit_the_chat() {
        let lines = |text: &str| -> Vec<String> {
            announcement(text, None)
                .iter()
                .map(|f| {
                    String::from_utf8(NtcLayerChat::decode(&f.payload).unwrap().text.to_vec())
                        .unwrap()
                })
                .collect()
        };
        assert_eq!(
            lines("The server restarts in 5 minutes, finish your quests please"),
            [
                "The server restarts in 5",
                "minutes, finish your quests",
                "please"
            ]
        );
        // A word longer than a line is cut.
        assert_eq!(lines(&"x".repeat(35)), ["x".repeat(30), "x".repeat(5)]);
        assert!(lines("   ").is_empty());
    }
}
