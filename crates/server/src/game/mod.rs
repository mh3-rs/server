//! Game logic: what the server answers. No sockets: handlers get a frame and return the frames to send back;
//! `net` does the I/O.

pub mod fmp;
pub mod lmp;
pub mod opn;
pub mod players;
pub mod world;

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use bytes::Bytes;
use sqlx::SqlitePool;
use wire::packets::{AnsServerTime, Packet, ReqConnection};
use wire::{DecodeError, EncodeError, Frame, PacketId};

use crate::Shared;
use crate::accounts::Console;
use fmp::Fmp;
use lmp::Lmp;
use opn::Opn;
use players::Mailbox;

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
                let (opn, frames) = Opn::start();
                (Handler::Opn(opn), frames)
            }
            Role::Lmp => (Handler::Lmp(Lmp::new()), req_connection().map(|f| vec![f])),
            Role::Fmp => (
                Handler::Fmp(Fmp::new(mailbox)),
                req_connection().map(|f| vec![f]),
            ),
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        match self {
            Handler::Opn(opn) => opn.handle(shared, req).await,
            Handler::Lmp(lmp) => lmp.handle(shared, req).await,
            Handler::Fmp(fmp) => fmp.handle(shared, req).await,
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
    pub profile: Box<[u8; PROFILE_SIZE]>,
}

/// The size of the hunter profile block.
pub const PROFILE_SIZE: usize = 0x100;

impl Session {
    pub fn new(console: Console) -> Self {
        Session {
            console,
            capcom_id: None,
            name: Bytes::new(),
            world_server: None,
            profile: Box::new([0; PROFILE_SIZE]),
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
        Fault::Server(e.into())
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

pub fn unexpected(id: PacketId) -> Fault {
    Fault::Client(anyhow!("unexpected packet {id}"))
}

/// The first message of every connection but OPN's, which checks the disc first.
fn req_connection() -> Result<Frame, Fault> {
    frame(0, &ReqConnection { unk: 0 })
}

/// Whether a console may play here. Everyone until the website's access modes (M8).
pub async fn allowed(_db: &SqlitePool, _support_code: Option<&[u8]>) -> sqlx::Result<bool> {
    Ok(true)
}
