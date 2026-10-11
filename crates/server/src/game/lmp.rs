//! LMP (port 8201): the login. The console comes twice.
//!
//! - First visit (`NtcLogin` 1): the console is recognised by its support code, or gets one, and linked to its Wii
//!   by our Nintendo login's token (`nas.rs`, used up here); it gets a PAT ticket, the 6 Capcom ID slots and the
//!   server time.
//! - Second visit (`NtcLogin` 2), with the ticket: it plays a slot (its Capcom ID, created for an empty slot), gets
//!   a new ticket, the world server list and the FMP address.
//!
//! The session goes from one visit to the next through the ticket cache (decision 4).

use anyhow::anyhow;
use bytes::Bytes;
use tracing::{Span, info};
use wire::packets::*;
use wire::{Frame, Item, PatData};

use super::{Fault, Handled, Session, alert, reply, server_time, unexpected, world};
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
    /// Its Wii's MAC and serial HMAC, from our Nintendo login's token: kept once taken, should it ask again.
    device: Option<Vec<u8>>,
    region: Option<&'static str>,
    session: Option<Session>,
    /// The last ticket issued: the session is handed off under it when the console leaves.
    ticket: Option<String>,
    /// Where the console connects from (`Admission`).
    ip: std::net::IpAddr,
}

impl Lmp {
    pub fn new(ip: std::net::IpAddr) -> Self {
        Lmp {
            stage: Stage::Connection,
            support_code: None,
            device: None,
            region: None,
            session: None,
            ticket: None,
            ip,
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
                        if crate::access::banned_since(shared, session.console.id).await {
                            return Err(Fault::Client(anyhow!("banned console")));
                        }
                        self.session = Some(session);
                        self.stage = Stage::SecondVisit;
                        SECOND_VISIT
                    }
                    _ => {
                        if !shared.admission.may_log_in(self.ip) {
                            crate::metrics::add(&shared.metrics.limited, 1);
                            return Err(Fault::Client(anyhow!("first visit without OPN")));
                        }
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
                // Our Nintendo login's token (decision 13), used up here. Never logged.
                let data = ReqLoginInfo::decode(p)?.data;
                if self.device.is_none()
                    && let Some(Item::Binary(token) | Item::String(token)) = data.get(0x0a)
                {
                    self.device = shared.nas.remove(String::from_utf8_lossy(token).as_ref());
                }
                let Some(device) = self.device.clone() else {
                    info!("first visit refused: not through our NAS");
                    return alert(AnsLoginInfo::ID, seq, crate::text::Text::UnknownLogin);
                };
                let db = db(shared)?;
                let secret = shared
                    .config
                    .secret
                    .as_ref()
                    .ok_or_else(|| server("no secret"))?;
                let region = self.region.expect("set with the first visit");
                // Kept until the login succeeds: after a database failure the console may ask again (decision 9), and
                // must not be handed a new code in place of its own.
                let known = match &self.support_code {
                    Some(code) => accounts::known_console(db, secret, code)
                        .await
                        .map_err(Fault::Database)?,
                    None => None,
                };
                // A code the server doesn't know registers a console: a new one, as far as the cap goes.
                if known.is_none() && !shared.admission.may_create(self.ip) {
                    crate::metrics::add(&shared.metrics.limited, 1);
                    return Err(Fault::Client(anyhow!(
                        "too many new consoles from this address"
                    )));
                }
                let (console, code) = match (known, self.support_code.clone()) {
                    (Some(console), Some(code)) => (console, code),
                    (None, Some(code)) => (
                        accounts::console(db, secret, &code, region)
                            .await
                            .map_err(Fault::Database)?,
                        code,
                    ),
                    (_, None) => {
                        let (console, code) = accounts::new_console(db, secret, region)
                            .await
                            .map_err(Fault::Database)?;
                        info!(console = %console.public_id, region, "support code issued");
                        // Asked again (a later failure), it is this console's.
                        self.support_code = Some(code.clone());
                        (console, code)
                    }
                };
                Span::current().record("console", tracing::field::display(console.public_id));
                accounts::link_device(db, &console, &device)
                    .await
                    .map_err(Fault::Database)?;
                if crate::access::banned(db, console.id)
                    .await
                    .map_err(Fault::Database)?
                {
                    info!("banned console refused");
                    return alert(AnsLoginInfo::ID, seq, crate::text::Text::Banned);
                }
                info!(region, "logged in");
                self.session = Some(Session::new(console));
                reply(
                    seq,
                    &AnsLoginInfo {
                        status: 1,
                        message: Bytes::from_static(b"dummy_data"),
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
                    .map_err(Fault::Database)?;
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
                    Some(Item::String(name)) => Some(&name[..]),
                    _ => None,
                };
                let console = self.session(req)?.console;
                let (capcom_id, created, name) =
                    accounts::use_slot(db(shared)?, &console, r.slot, name)
                        .await
                        .map_err(Fault::Database)?;
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
                session.name = name.into();
                reply(
                    seq,
                    &AnsUserObject {
                        status: 1,
                        message: Bytes::from_static(b"Unk_UserObj_str"),
                        unused: 0,
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
                        first: 0,
                        count: world::SERVERS.len() as u32,
                    },
                )
            }
            (Stage::SecondVisit, ReqFmpListData::ID) => {
                let r = ReqFmpListData::decode(p)?;
                let servers = world::list(shared, r.first, r.count)?;
                reply(seq, &AnsFmpListData { first: 0, servers })
            }
            (Stage::SecondVisit, ReqFmpListFoot::ID) => {
                ReqFmpListFoot::decode(p)?;
                reply(seq, &AnsFmpListFoot {})
            }
            (Stage::SecondVisit, ReqFmpInfo::ID) => {
                let r = ReqFmpInfo::decode(p)?;
                // The first pick comes before the console sends its profile: the HR stored last time, if any.
                let session = self.session(req)?;
                let hr = match session.profile.hr {
                    0 => {
                        let capcom_id = session
                            .capcom_id
                            .as_deref()
                            .ok_or_else(|| Fault::Client(anyhow!("no Capcom ID chosen")))?;
                        let db = shared
                            .db
                            .pool()
                            .ok_or_else(|| Fault::Database(anyhow!("database not opened")))?;
                        accounts::stored_hr(db, capcom_id)
                            .await
                            .map_err(Fault::Database)?
                    }
                    hr => Some(hr),
                };
                if let Some(refusal) = world::hr_refusal(r.index, hr) {
                    info!(index = r.index, "world server refused: HR limit");
                    return alert(AnsFmpInfo::ID, seq, refusal);
                }
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
        .ok_or_else(|| Fault::Database(anyhow!("database not opened")))
}

fn server(message: &'static str) -> Fault {
    Fault::Server(anyhow!(message))
}
