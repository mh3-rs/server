//! Game logic: what the server answers. No sockets: handlers get a frame and return the frames to send back;
//! `net` does the I/O.

pub mod lmp;
pub mod opn;

use std::fmt;

use anyhow::anyhow;
use sqlx::SqlitePool;
use wire::packets::{Packet, ReqConnection};
use wire::{DecodeError, EncodeError, Frame, PacketId};

use crate::Shared;
use crate::accounts::Console;
use lmp::Lmp;
use opn::Opn;

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
    /// FMP (M2) only opens the connection so far.
    Unhandled,
}

impl Handler {
    /// The handler, and what the server sends first.
    pub fn start(role: Role) -> (Self, Handled) {
        match role {
            Role::Opn => {
                let (opn, frames) = Opn::start();
                (Handler::Opn(opn), frames)
            }
            Role::Lmp => (Handler::Lmp(Lmp::new()), req_connection().map(|f| vec![f])),
            Role::Fmp => (Handler::Unhandled, req_connection().map(|f| vec![f])),
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        match self {
            Handler::Opn(opn) => opn.handle(shared, req).await,
            Handler::Lmp(lmp) => lmp.handle(shared, req).await,
            Handler::Unhandled => Err(unexpected(req.id)),
        }
    }

    /// The console announced it leaves (`ReqShut`).
    pub fn leave(&mut self, shared: &Shared) {
        if let Handler::Lmp(lmp) = self {
            lmp.leave(shared);
        }
    }
}

/// A logged-in console, owned by its connection; handed off to the next one by PAT ticket.
#[derive(Clone, Debug)]
pub struct Session {
    pub console: Console,
    pub capcom_id: Option<String>,
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
