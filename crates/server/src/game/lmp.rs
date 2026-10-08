//! LMP (port 8201): the login. The console comes twice.
//!
//! - First visit (`NtcLogin` 1): the console is recognised by its support code, or gets one; it gets a PAT ticket,
//!   the 6 Capcom ID slots and the server time.
//! - Second visit (`NtcLogin` 2), with the ticket: it plays a slot (its Capcom ID, created for an empty slot), gets
//!   a new ticket, the world server list and the FMP address.
//!
//! The session goes from one visit to the next through the ticket cache (decision 4).

use anyhow::anyhow;
use bytes::Bytes;
use tracing::{Span, info};
use wire::packets::*;
use wire::{Frame, Item, PatData};

use super::{Fault, Handled, Session, reply, server_time, unexpected};
use crate::Shared;
use crate::accounts::{self, SUPPORT_CODE_LEN, random_id};

/// `NtcLogin` statuses on LMP.
const FIRST_VISIT: u8 = 1;
const SECOND_VISIT: u8 = 2;

/// What a slot without a Capcom ID shows.
const EMPTY_SLOT: &[u8] = b"******";
const SLOTS: u32 = 6;

/// The world servers (MH3SP's): name and type (1 open, 2 rookie, 3 expert, 4 recruiting).
const WORLD_SERVERS: [(&str, u64); 12] = [
    ("Valor1", 1),
    ("Valor2", 1),
    ("Valor3", 1),
    ("Valor4", 1),
    ("Beginners1", 2),
    ("Beginners2", 2),
    ("Veterans1", 3),
    ("Veterans2", 3),
    ("Greed1", 4),
    ("Greed2", 4),
    ("Greed3", 4),
    ("Greed4", 4),
];
const WORLD_SERVER_CAPACITY: u32 = 2000;
const FMP_LIST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Connection,
    FirstVisit,
    SecondVisit,
}

pub struct Lmp {
    stage: Stage,
    /// From `AnsConnection` on a first visit: the console's code, unless it has none yet.
    support_code: Option<String>,
    region: Option<&'static str>,
    session: Option<Session>,
    /// The last ticket issued: the session is handed off under it when the console leaves.
    ticket: Option<String>,
}

impl Lmp {
    pub fn new() -> Self {
        Lmp {
            stage: Stage::Connection,
            support_code: None,
            region: None,
            session: None,
            ticket: None,
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let (seq, p) = (req.seq, &req.payload);
        match (self.stage, req.id) {
            (Stage::Connection, AnsConnection::ID) => {
                let data = AnsConnection::decode(p)?.data;
                let status = match data.get(0x02) {
                    Some(Item::Binary(ticket) | Item::String(ticket)) => {
                        let ticket = String::from_utf8_lossy(ticket);
                        let session = shared
                            .tickets
                            .remove(ticket.as_ref())
                            .ok_or_else(|| Fault::Client(anyhow!("unknown or expired ticket")))?;
                        Span::current().record(
                            "console",
                            tracing::field::display(session.console.public_id),
                        );
                        self.session = Some(session);
                        self.stage = Stage::SecondVisit;
                        SECOND_VISIT
                    }
                    _ => {
                        self.support_code = match data.get(0x01) {
                            Some(Item::String(code)) if code.len() == SUPPORT_CODE_LEN => {
                                Some(String::from_utf8_lossy(code).into_owned())
                            }
                            Some(_) => {
                                return Err(Fault::Client(anyhow!("malformed support code")));
                            }
                            None => None,
                        };
                        self.region = Some(region(&data)?);
                        self.stage = Stage::FirstVisit;
                        FIRST_VISIT
                    }
                };
                reply(seq, &NtcLogin { status })
            }
            (Stage::FirstVisit, ReqLoginInfo::ID) => {
                // The Nintendo login's token: checking it is the Nintendo login server's job. Never logged.
                ReqLoginInfo::decode(p)?;
                let db = db(shared)?;
                let secret = shared
                    .config
                    .secret
                    .as_ref()
                    .ok_or_else(|| server("no secret"))?;
                let region = self.region.expect("set with the first visit");
                let (console, code) = match self.support_code.take() {
                    Some(code) => (
                        accounts::console(db, secret, &code, region)
                            .await
                            .map_err(Fault::Server)?,
                        code,
                    ),
                    None => {
                        let (console, code) = accounts::new_console(db, secret, region)
                            .await
                            .map_err(Fault::Server)?;
                        info!(console = %console.public_id, region, "support code issued");
                        (console, code)
                    }
                };
                Span::current().record("console", tracing::field::display(console.public_id));
                info!(region, "logged in");
                self.session = Some(Session {
                    console,
                    capcom_id: None,
                });
                reply(
                    seq,
                    &AnsLoginInfo {
                        need_ticket: 1,
                        unk: Bytes::from_static(b"dummy_data"),
                        data: PatData::new().with(0x07, Item::String(code.into())),
                    },
                )
            }
            (Stage::FirstVisit | Stage::SecondVisit, ReqTicket::ID) => {
                ReqTicket::decode(p)?;
                self.session(req)?;
                let ticket = random_id(SUPPORT_CODE_LEN);
                self.ticket = Some(ticket.clone());
                reply(
                    seq,
                    &AnsTicket {
                        ticket: ticket.into(),
                    },
                )
            }
            (Stage::FirstVisit, ReqUserListHead::ID) => {
                let r = ReqUserListHead::decode(p)?;
                reply(
                    seq,
                    &AnsUserListHead {
                        first: r.first,
                        count: r.count,
                    },
                )
            }
            (Stage::FirstVisit, ReqUserListData::ID) => {
                let r = ReqUserListData::decode(p)?;
                let console = self.session(req)?.console;
                let used = accounts::slots(db(shared)?, &console)
                    .await
                    .map_err(Fault::Server)?;
                let last = r.first.saturating_add(r.count).min(SLOTS + 1);
                let users = (r.first.max(1)..last)
                    .map(|slot| {
                        let user = PatData::new().with(0x01, Item::Long(slot));
                        match used.iter().find(|s| s.slot == slot) {
                            Some(s) => user
                                .with(0x02, Item::String(s.capcom_id.clone().into()))
                                .with(0x03, Item::String(s.name.clone().into())),
                            None => user.with(0x02, Item::String(Bytes::from_static(EMPTY_SLOT))),
                        }
                    })
                    .collect();
                reply(
                    seq,
                    &AnsUserListData {
                        first: r.first,
                        users,
                    },
                )
            }
            (Stage::FirstVisit, ReqUserListFoot::ID) => {
                ReqUserListFoot::decode(p)?;
                reply(seq, &AnsUserListFoot {})
            }
            (Stage::FirstVisit, ReqServerTime::ID) => {
                ReqServerTime::decode(p)?;
                server_time(seq)
            }
            (Stage::SecondVisit, ReqUserObject::ID) => {
                let r = ReqUserObject::decode(p)?;
                if !(1..=SLOTS).contains(&r.slot) {
                    return Err(Fault::Client(anyhow!("no Capcom ID slot {}", r.slot)));
                }
                let name = match r.user.get(0x03) {
                    Some(Item::String(name)) => name.clone(),
                    _ => Bytes::new(),
                };
                let console = self.session(req)?.console;
                let (capcom_id, created) = accounts::use_slot(db(shared)?, &console, r.slot, &name)
                    .await
                    .map_err(Fault::Server)?;
                Span::current().record("capcom_id", capcom_id.as_str());
                info!(
                    slot = r.slot,
                    "{}",
                    if created {
                        "Capcom ID created"
                    } else {
                        "Capcom ID chosen"
                    }
                );
                let user = set(r.user, 0x02, Item::String(capcom_id.clone().into()));
                self.session.as_mut().expect("checked above").capcom_id = Some(capcom_id);
                reply(
                    seq,
                    &AnsUserObject {
                        need_ticket: 1,
                        unk: Bytes::from_static(b"Unk_UserObj_str"),
                        unk2: 0,
                        user,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListVersion::ID) => {
                ReqFmpListVersion::decode(p)?;
                reply(
                    seq,
                    &AnsFmpListVersion {
                        version: FMP_LIST_VERSION,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListHead::ID) => {
                ReqFmpListHead::decode(p)?;
                reply(
                    seq,
                    &AnsFmpListHead {
                        unk: 0,
                        count: WORLD_SERVERS.len() as u32,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListData::ID) => {
                let r = ReqFmpListData::decode(p)?;
                let address = external_address(shared)?;
                let servers = WORLD_SERVERS
                    .iter()
                    .zip(1u32..)
                    .skip(r.first.saturating_sub(1) as usize)
                    .take(r.count as usize)
                    .map(|(&(name, kind), index)| {
                        PatData::new()
                            .with(0x01, Item::Long(index))
                            .with(0x02, Item::String(address.clone()))
                            .with(0x03, Item::Word(shared.fmp_port))
                            .with(0x07, Item::LongLong(kind))
                            // Populations come with the world servers (M2).
                            .with(0x08, Item::Long(0))
                            .with(0x09, Item::Long(WORLD_SERVER_CAPACITY))
                            .with(0x0a, Item::String(Bytes::from_static(name.as_bytes())))
                            // Unknown; MH3SP's values.
                            .with(0x0b, Item::String(Bytes::from_static(b"X")))
                            .with(0x0c, Item::Long(0x1234_5678))
                    })
                    .collect();
                reply(seq, &AnsFmpListData { unk: 0, servers })
            }
            (Stage::SecondVisit, ReqFmpListFoot::ID) => {
                ReqFmpListFoot::decode(p)?;
                reply(seq, &AnsFmpListFoot {})
            }
            (Stage::SecondVisit, ReqFmpInfo::ID) => {
                let r = ReqFmpInfo::decode(p)?;
                if !(1..=WORLD_SERVERS.len() as u32).contains(&r.index) {
                    return Err(Fault::Client(anyhow!("no world server {}", r.index)));
                }
                let address = external_address(shared)?;
                let mut data = PatData::new();
                for field in r.fields {
                    data = match field {
                        0x02 => data.with(field, Item::String(address.clone())),
                        0x03 => data.with(field, Item::Word(shared.fmp_port)),
                        _ => {
                            return Err(Fault::Client(anyhow!(
                                "FMP info field {field:#x} unknown"
                            )));
                        }
                    };
                }
                reply(seq, &AnsFmpInfo { data })
            }
            (_, id) => Err(unexpected(id)),
        }
    }

    /// The console leaves: its session waits for the next connection under the last ticket issued.
    pub fn leave(&mut self, shared: &Shared) {
        if let (Some(ticket), Some(session)) = (self.ticket.take(), self.session.take()) {
            shared.tickets.insert(ticket, session);
        }
    }

    fn session(&self, req: &Frame) -> Result<&Session, Fault> {
        self.session
            .as_ref()
            .ok_or_else(|| Fault::Client(anyhow!("{} before logging in", req.id)))
    }
}

/// The disc's region, from the media version the console sends (`AnsConnection` 0x07).
fn region(data: &PatData) -> Result<&'static str, Fault> {
    match data.get(0x07) {
        Some(Item::String(v)) if &v[..] == b"1002121503" => Ok("RMHP08"),
        Some(Item::String(v)) if &v[..] == b"1001301045" => Ok("RMHE08"),
        _ => Err(Fault::Client(anyhow!("unsupported media version"))),
    }
}

/// Sets `field` in place, or appends it.
fn set(mut data: PatData, field: u8, item: Item) -> PatData {
    match data.0.iter_mut().find(|(f, _)| *f == field) {
        Some(entry) => entry.1 = item,
        None => data.0.push((field, item)),
    }
    data
}

fn db(shared: &Shared) -> Result<&sqlx::SqlitePool, Fault> {
    shared
        .db
        .pool()
        .ok_or_else(|| server("database not opened"))
}

fn external_address(shared: &Shared) -> Result<Bytes, Fault> {
    let address = shared
        .config
        .external_address
        .as_ref()
        .ok_or_else(|| server("EXTERNAL_ADDRESS unset"))?;
    Ok(Bytes::copy_from_slice(address.as_bytes()))
}

fn server(message: &'static str) -> Fault {
    Fault::Server(anyhow!(message))
}
