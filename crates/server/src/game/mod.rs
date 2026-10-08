//! Game logic: what the server answers. No sockets: handlers get a frame and return the frames to send back;
//! `net` does the I/O.

pub mod opn;

use std::fmt;

use anyhow::anyhow;
use sqlx::SqlitePool;
use wire::packets::{Packet, ReqConnection};
use wire::{DecodeError, EncodeError, Frame, PacketId};

use crate::Shared;
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
pub enum Session {
    Opn(Opn),
    /// LMP (M1) and FMP (M2) only open the connection so far.
    Unhandled,
}

impl Session {
    /// The session, and what the server sends first.
    pub fn start(role: Role) -> (Self, Handled) {
        match role {
            Role::Opn => {
                let (opn, frames) = Opn::start();
                (Session::Opn(opn), frames)
            }
            Role::Lmp | Role::Fmp => (Session::Unhandled, req_connection().map(|f| vec![f])),
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        match self {
            Session::Opn(opn) => opn.handle(shared, req).await,
            Session::Unhandled => Err(unexpected(req.id)),
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
