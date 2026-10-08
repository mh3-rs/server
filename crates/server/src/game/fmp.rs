//! FMP (port 8202): the world servers. The console comes twice with the ticket of its last LMP visit: once to pick a
//! world server, then to play on it (gates, cities, quests).

use std::collections::HashMap;

use anyhow::anyhow;
use bytes::Bytes;
use tracing::{Span, info, trace};
use wire::packets::*;
use wire::{Frame, Item, PatData};

use super::players::Mailbox;
use super::{Fault, Handled, PROFILE_SIZE, Session, reply, server_time, unexpected, world};
use crate::Shared;
use crate::content::{self, Binary};
use crate::text::Language;

/// `NtcLogin` status on FMP.
const LOGGED_IN: u8 = 3;

pub struct Fmp {
    mailbox: Mailbox,
    /// The session and the ticket it came with (it goes back under the same one: FMP issues none).
    login: Option<(Session, String)>,
    /// Content being downloaded, by type: every chunk of a download comes from the same bytes.
    downloads: HashMap<u8, Binary>,
}

impl Fmp {
    pub fn new(mailbox: Mailbox) -> Self {
        Fmp {
            mailbox,
            login: None,
            downloads: HashMap::new(),
        }
    }

    pub async fn handle(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let (seq, p) = (req.seq, &req.payload);
        match (&self.login, req.id) {
            (None, AnsConnection::ID) => {
                let data = AnsConnection::decode(p)?.data;
                let Some(Item::Binary(ticket) | Item::String(ticket)) = data.get(0x02) else {
                    return Err(Fault::Client(anyhow!("no ticket")));
                };
                let ticket = String::from_utf8_lossy(ticket).into_owned();
                let session = shared
                    .tickets
                    .remove(&ticket)
                    .ok_or_else(|| Fault::Client(anyhow!("unknown or expired ticket")))?;
                let capcom_id = session
                    .capcom_id
                    .clone()
                    .ok_or_else(|| Fault::Client(anyhow!("no Capcom ID chosen")))?;
                let span = Span::current();
                span.record(
                    "console",
                    tracing::field::display(session.console.public_id),
                );
                span.record("capcom_id", capcom_id.as_str());
                shared.players.register(&capcom_id, self.mailbox.clone());
                info!("joined FMP");
                self.login = Some((session, ticket));
                reply(seq, &NtcLogin { status: LOGGED_IN })
            }
            (None, id) => Err(unexpected(id)),
            (Some(_), _) => self.request(shared, req),
        }
    }

    fn request(&mut self, shared: &Shared, req: &Frame) -> Handled {
        let (seq, p) = (req.seq, &req.payload);
        let (session, _) = self.login.as_mut().expect("logged in");
        match req.id {
            ReqServerTime::ID => {
                ReqServerTime::decode(p)?;
                server_time(seq)
            }
            ReqBinaryVersion::ID => {
                let r = ReqBinaryVersion::decode(p)?;
                let version = binary(shared, r.kind)?.version;
                reply(seq, &AnsBinaryVersion { unk: 0, version })
            }
            ReqBinaryHead::ID => {
                let r = ReqBinaryHead::decode(p)?;
                let binary = binary(shared, r.kind)?;
                let head = AnsBinaryHead {
                    version: binary.version,
                    size: binary.bytes.len() as u32,
                };
                self.downloads.insert(r.kind, binary);
                reply(seq, &head)
            }
            ReqBinaryData::ID => {
                // The version the console names is the one `AnsBinaryHead` gave: the download's.
                let r = ReqBinaryData::decode(p)?;
                let binary = match self.downloads.get(&r.kind) {
                    Some(b) => b.clone(),
                    None => binary(shared, r.kind)?,
                };
                let start = (r.offset as usize).min(binary.bytes.len());
                let end = start
                    .saturating_add(r.size as usize)
                    .min(binary.bytes.len());
                reply(
                    seq,
                    &AnsBinaryData {
                        version: binary.version,
                        offset: r.offset,
                        size: r.size,
                        chunk: Bytes::copy_from_slice(&binary.bytes[start..end]),
                    },
                )
            }
            ReqBinaryFoot::ID => {
                let r = ReqBinaryFoot::decode(p)?;
                self.downloads.remove(&r.kind);
                reply(seq, &AnsBinaryFoot {})
            }
            ReqUserSearchInfoMine::ID => {
                ReqUserSearchInfoMine::decode(p)?;
                // Two numbers the game uses to tell players apart in relayed town data: stable per Capcom ID.
                let capcom_id = session.capcom_id.as_deref().unwrap_or_default();
                let hash = openssl::sha::sha256(capcom_id.as_bytes());
                let number =
                    |i: usize| u32::from_be_bytes([hash[i], hash[i + 1], hash[i + 2], hash[i + 3]]);
                let info = PatData::new()
                    .with(0x0f, Item::Long(number(0)))
                    .with(0x10, Item::Long(number(4)));
                reply(seq, &AnsUserSearchInfoMine { info })
            }
            ReqLayerStart::ID => {
                ReqLayerStart::decode(p)?;
                // MH3SP's placeholder layer: the console only needs the fields to exist.
                let layer = PatData::new()
                    .with(0x05, Item::Word(1))
                    .with(0x03, Item::String(Bytes::from_static(b"LayerStart")))
                    .with(0x06, Item::Long(0))
                    .with(0x09, Item::Long(4))
                    .with(0x10, Item::Byte(1))
                    .with(0x11, Item::Long(500))
                    .with(0x12, Item::Byte(1));
                reply(seq, &AnsLayerStart { layer })
            }
            ReqLayerEnd::ID => {
                ReqLayerEnd::decode(p)?;
                reply(seq, &AnsLayerEnd {})
            }
            ReqCircleInfoNoticeSet::ID => {
                ReqCircleInfoNoticeSet::decode(p)?;
                reply(seq, &AnsCircleInfoNoticeSet {})
            }
            NtcCheatCheck::ID => {
                // The game's integrity report: read with anti-cheat (M6).
                trace!(len = p.len(), "cheat check");
                Ok(vec![])
            }
            ReqUserBinarySet::ID => {
                let r = ReqUserBinarySet::decode(p)?;
                let start = r.offset as usize;
                let end = start + r.data.len();
                if end > PROFILE_SIZE {
                    return Err(Fault::Client(anyhow!(
                        "profile write past {PROFILE_SIZE} bytes ({start}..{end})"
                    )));
                }
                session.profile[start..end].copy_from_slice(&r.data);
                reply(seq, &AnsUserBinarySet {})
            }
            ReqUserSearchSet::ID => {
                // What the player can be searched by (M3).
                ReqUserSearchSet::decode(p)?;
                reply(seq, &AnsUserSearchSet {})
            }
            ReqFmpListVersion2::ID => {
                ReqFmpListVersion2::decode(p)?;
                reply(
                    seq,
                    &AnsFmpListVersion2 {
                        version: world::LIST_VERSION,
                    },
                )
            }
            ReqFmpListHead2::ID => {
                ReqFmpListHead2::decode(p)?;
                reply(
                    seq,
                    &AnsFmpListHead2 {
                        unk: 0,
                        count: world::SERVERS.len() as u32,
                    },
                )
            }
            ReqFmpListData2::ID => {
                let r = ReqFmpListData2::decode(p)?;
                let servers = world::list(shared, r.first, r.count)?;
                reply(seq, &AnsFmpListData2 { unk: 0, servers })
            }
            ReqFmpListFoot2::ID => {
                ReqFmpListFoot2::decode(p)?;
                reply(seq, &AnsFmpListFoot2 {})
            }
            ReqFmpInfo2::ID => {
                let r = ReqFmpInfo2::decode(p)?;
                let data = world::fmp_info(shared, r.index, &r.fields)?;
                session.world_server = Some(r.index);
                info!(
                    world_server = world::SERVERS[r.index as usize - 1].0,
                    "world server picked"
                );
                reply(seq, &AnsFmpInfo2 { data })
            }
            id => Err(unexpected(id)),
        }
    }

    /// The console leaves for its next connection: the session waits under its ticket.
    pub fn leave(&mut self, shared: &Shared) {
        if let Some((session, ticket)) = self.login.take() {
            self.unregister(shared, &session);
            shared.tickets.insert(ticket, session);
        }
    }

    /// The connection is closed.
    pub fn close(&mut self, shared: &Shared) {
        if let Some((session, _)) = self.login.take() {
            self.unregister(shared, &session);
        }
    }

    fn unregister(&self, shared: &Shared, session: &Session) {
        if let Some(capcom_id) = &session.capcom_id {
            shared.players.unregister(capcom_id, self.mailbox.conn);
        }
    }
}

/// Content binary `kind`, built now.
fn binary(shared: &Shared, kind: u8) -> Result<Binary, Fault> {
    content::binary(kind, shared.started, Language::default())
        .ok_or_else(|| Fault::Client(anyhow!("content type {kind:#x} unknown")))
}
