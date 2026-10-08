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

use super::{Fault, Handled, Session, reply, server_time, unexpected, world};
use crate::Shared;
use crate::accounts::{self, SUPPORT_CODE_LEN, random_id};

/// `NtcLogin` statuses on LMP.
const FIRST_VISIT: u8 = 1;
const SECOND_VISIT: u8 = 2;

/// What a slot without a Capcom ID shows.
const EMPTY_SLOT: &[u8] = b"******";
const SLOTS: u32 = 6;

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
                self.session = Some(Session::new(console));
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
                let session = self.session.as_mut().expect("checked above");
                session.capcom_id = Some(capcom_id);
                session.name = name;
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
                        version: world::LIST_VERSION,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListHead::ID) => {
                ReqFmpListHead::decode(p)?;
                reply(
                    seq,
                    &AnsFmpListHead {
                        unk: 0,
                        count: world::SERVERS.len() as u32,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListData::ID) => {
                let r = ReqFmpListData::decode(p)?;
                let servers = world::list(shared, r.first, r.count)?;
                reply(seq, &AnsFmpListData { unk: 0, servers })
            }
            (Stage::SecondVisit, ReqFmpListFoot::ID) => {
                ReqFmpListFoot::decode(p)?;
                reply(seq, &AnsFmpListFoot {})
            }
            (Stage::SecondVisit, ReqFmpInfo::ID) => {
                let r = ReqFmpInfo::decode(p)?;
                let data = world::fmp_info(shared, r.index, &r.fields)?;
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

fn server(message: &'static str) -> Fault {
    Fault::Server(anyhow!(message))
}
