//! FMP (port 8202): the world servers. The console comes twice with the ticket of its last LMP visit: once to pick a
//! world server, then to play on it (gates, cities, quests).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use bytes::{Buf, BufMut, Bytes};
use sqlx::SqlitePool;
use tracing::{Span, debug, error, info, trace, warn};
use wire::packets::*;
use wire::{Frame, Item, OptionalFields, PatData, PatWrite, Reader};

use super::commands;
use super::players::Mailbox;
use super::world::{CircleId, NewCircle, World};
use super::{Fault, Handled, Session, alert, frame, reply, server_time, unexpected, world};
use crate::Shared;
use crate::accounts;
use crate::content::{self, Binary};
use crate::text::{Language, Text, text};

/// `NtcLogin` status on FMP.
const LOGGED_IN: u8 = 3;

pub struct Fmp {
    mailbox: Mailbox,
    /// The session and the ticket it came with (it goes back under the same one: FMP issues none).
    login: Option<(Session, String)>,
    /// Content being downloaded, by type: every chunk of a download comes from the same bytes.
    downloads: HashMap<u8, Binary>,
    /// The players found by the last layer search (`LayerUserListHead`), read by `LayerUserListData`.
    search: Vec<String>,
    /// The version constant the console reported at login (ConnectionData item 3: 4 on PAL, 3 on US), which starts
    /// the paths it can jump to (M5 D11).
    version: u32,
    /// Rate limits (boxed: they would make this the one large handler).
    limits: Box<Limits>,
}

/// Rate limits (provisional, the review's): what one console sends the others, chat, and quest joins (a password is
/// checked with argon2). A flood would fill the others' mailboxes, which drops *them*.
struct Limits {
    relayed: Bucket,
    chat: Bucket,
    joins: Bucket,
}

/// The game's own traffic peaks at 9 relayed messages in a second per console (42 in ten), a chat line a second, a
/// quest join a second (the corpus): the limits leave room.
const RELAYED: (f64, f64) = (40.0, 20.0);
const CHAT: (f64, f64) = (5.0, 1.0);
const JOINS: (f64, f64) = (5.0, 1.0);

/// A token bucket: `burst` at once, refilled at `per_second`.
struct Bucket {
    tokens: f64,
    burst: f64,
    per_second: f64,
    last: std::time::Instant,
}

impl Bucket {
    fn new((burst, per_second): (f64, f64)) -> Self {
        Bucket {
            tokens: burst,
            burst,
            per_second,
            last: std::time::Instant::now(),
        }
    }

    /// Whether one more is allowed now.
    fn take(&mut self) -> bool {
        let now = std::time::Instant::now();
        let refill = now.duration_since(self.last).as_secs_f64() * self.per_second;
        self.tokens = (self.tokens + refill).min(self.burst);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl Fmp {
    pub fn new(mailbox: Mailbox) -> Self {
        Fmp {
            mailbox,
            login: None,
            downloads: HashMap::new(),
            search: Vec::new(),
            version: 0,
            limits: Box::new(Limits {
                relayed: Bucket::new(RELAYED),
                chat: Bucket::new(CHAT),
                joins: Bucket::new(JOINS),
            }),
        }
    }

    pub async fn handle(&mut self, shared: &Arc<Shared>, req: &Frame) -> Handled {
        let (seq, p) = (req.seq, &req.payload);
        match (&self.login, req.id) {
            (None, AnsConnection::ID) => {
                let data = AnsConnection::decode(p)?.data;
                let Some(Item::Binary(ticket) | Item::String(ticket)) = data.get(0x02) else {
                    return Err(Fault::Client(anyhow!("no ticket")));
                };
                let ticket = String::from_utf8_lossy(ticket).into_owned();
                let Some(Item::Long(version)) = data.get(0x03) else {
                    return Err(Fault::Client(anyhow!("no version constant")));
                };
                self.version = *version;
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
                // Banned between two connections (on the server selection, or between LMP and FMP).
                if crate::access::banned_since(shared, session.console.id).await {
                    return Err(Fault::Client(anyhow!("banned console")));
                }
                shared.players.register(&capcom_id, self.mailbox.clone());
                // Back from the server selection with a pick: the player enters that world server.
                if let Some(server) = session.world_server {
                    let mut world = shared.world();
                    world.enter(
                        &capcom_id,
                        self.mailbox.conn,
                        server,
                        session.name.clone(),
                        session.profile.clone(),
                    );
                    info!(
                        world_server = world::SERVERS[server as usize - 1].0,
                        "entered world server"
                    );
                } else {
                    info!("joined FMP");
                }
                self.login = Some((session, ticket));
                reply(seq, &NtcLogin { status: LOGGED_IN })
            }
            (None, id) => Err(unexpected(id)),
            (Some(_), _) => {
                let bucket = match req.id {
                    NtcLayerChat::ID | NtcCircleChat::ID => Some(&mut self.limits.chat),
                    NtcLayerUserPosition::ID
                    | NtcLayerBinary::ID
                    | NtcLayerBinary2::ID
                    | NtcCircleBinary::ID
                    | NtcCircleBinary2::ID => Some(&mut self.limits.relayed),
                    ReqCircleJoin::ID => Some(&mut self.limits.joins),
                    _ => None,
                };
                if let Some(bucket) = bucket
                    && shared.config.rate_limits
                    && !bucket.take()
                {
                    crate::metrics::add(&shared.metrics.limited, 1);
                    debug!(packet = %req.id, "over its rate, dropped");
                    return match req.id {
                        ReqCircleJoin::ID => alert(AnsCircleJoin::ID, seq, Text::TooFast),
                        _ => Ok(vec![]),
                    };
                }
                let handled = match self.social(shared, req).await {
                    Some(handled) => handled,
                    None => self.request(shared, req),
                };
                let (session, _) = self.login.as_ref().expect("logged in");
                deliver_mail(shared, &self.mailbox, session, req, handled).await
            }
        }
    }

    /// The requests that need the database (profile, quests, messages, friends, black list); `None` for anything else.
    async fn social(&mut self, shared: &Shared, req: &Frame) -> Option<Handled> {
        if req.id == ReqUserBinarySet::ID {
            let (session, _) = self.login.as_mut().expect("logged in");
            return Some(profile_set(shared, session, req).await);
        }
        if req.id == ReqCircleCreate::ID || req.id == ReqCircleJoin::ID {
            let (session, _) = self.login.as_ref().expect("logged in");
            return Some(quests(shared, session, req).await);
        }
        let ids = [
            ReqTell::ID,
            ReqFriendList::ID,
            ReqFriendAdd::ID,
            ReqFriendAccept::ID,
            ReqFriendDelete::ID,
            ReqBlackList::ID,
            ReqBlackAdd::ID,
            ReqBlackDelete::ID,
        ];
        if !ids.contains(&req.id) {
            return None;
        }
        let (session, _) = self.login.as_ref().expect("logged in");
        Some(social(shared, session, req).await)
    }

    fn request(&mut self, shared: &Arc<Shared>, req: &Frame) -> Handled {
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
                reply(seq, &AnsBinaryVersion { kind: 0, version })
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
                let (a, b) = search_ids(capcom_id(session));
                let info = PatData::new()
                    .with(0x0f, Item::Long(a))
                    .with(0x10, Item::Long(b));
                reply(seq, &AnsUserSearchInfoMine { info })
            }
            ReqLayerUserSearchHead::ID => {
                // The player search: everyone under a layer (a gate's cities included), optionally by Capcom ID or
                // name; kept for the data request.
                let r = ReqLayerUserSearchHead::decode(p)?;
                let (server, gate, city) = world::parse_path(&r.layer)
                    .ok_or_else(|| Fault::Client(anyhow!("malformed layer path")))?;
                let world = shared.world();
                self.search = world
                    .players_under(server, gate, city)
                    .into_iter()
                    .filter(|(id, p)| {
                        (r.capcom_id.is_empty() || id.as_bytes() == &r.capcom_id[..])
                            && (r.name.is_empty() || p.name == r.name)
                    })
                    .skip(r.first.saturating_sub(1) as usize)
                    .take(r.count as usize)
                    .map(|(id, _)| id.to_owned())
                    .collect();
                reply(
                    seq,
                    &AnsLayerUserSearchHead {
                        first: 0,
                        count: self.search.len() as u32,
                    },
                )
            }
            ReqLayerUserSearchData::ID => {
                ReqLayerUserSearchData::decode(p)?;
                let users = found_users(shared, &self.search, false);
                reply(seq, &AnsLayerUserSearchData { first: 0, users })
            }
            ReqLayerUserSearchFoot::ID => {
                ReqLayerUserSearchFoot::decode(p)?;
                reply(seq, &AnsLayerUserSearchFoot {})
            }
            ReqUserSearchHead::ID => {
                // Player Search (Online menu): everyone else on every world server, by part of their Capcom ID or
                // name (any case), as MH3SP but without the searcher (M5 D9); nothing without either. Kept for the
                // data requests.
                let r = ReqUserSearchHead::decode(p)?;
                let me = capcom_id(session);
                let world = shared.world();
                let contains = |haystack: &[u8], needle: &[u8]| {
                    haystack
                        .windows(needle.len())
                        .any(|w| w.eq_ignore_ascii_case(needle))
                };
                self.search = match r.capcom_id.is_empty() && r.name.is_empty() {
                    true => Vec::new(),
                    false => world
                        .everyone()
                        .into_iter()
                        .filter(|(id, p)| {
                            *id != me
                                && (r.capcom_id.is_empty() || contains(id.as_bytes(), &r.capcom_id))
                                && (r.name.is_empty() || contains(&p.name, &r.name))
                        })
                        .skip(r.first.saturating_sub(1) as usize)
                        .take(r.count as usize)
                        .map(|(id, _)| id.to_owned())
                        .collect(),
                };
                reply(
                    seq,
                    &AnsUserSearchHead {
                        first: 0,
                        count: self.search.len() as u32,
                    },
                )
            }
            ReqUserSearchData::ID => {
                // The rows asked for (the console asks 10 at a time).
                let r = ReqUserSearchData::decode(p)?;
                let page: Vec<String> = self
                    .search
                    .iter()
                    .skip(r.first.saturating_sub(1) as usize)
                    .take(r.count as usize)
                    .cloned()
                    .collect();
                let users = found_users(shared, &page, true);
                reply(seq, &AnsUserSearchData { first: 0, users })
            }
            ReqUserSearchFoot::ID => {
                ReqUserSearchFoot::decode(p)?;
                reply(seq, &AnsUserSearchFoot {})
            }
            ReqUserSearchInfo::ID => {
                // A player's card, if they're in the world.
                let r = ReqUserSearchInfo::decode(p)?;
                let id = String::from_utf8_lossy(&r.capcom_id).into_owned();
                let mut world = shared.world();
                let Some(place) = world.place(&id).cloned() else {
                    // MH3SP's way of showing an offline player: an alert carrying a card named OFFLINE.
                    let card = PatData::new()
                        .with(0x01, Item::String(r.capcom_id))
                        .with(0x02, Item::String(Bytes::from_static(b"OFFLINE")));
                    let mut payload = Vec::new();
                    payload.put_pat_data(&card)?;
                    payload.put_optional_fields(&OptionalFields::default())?;
                    let mut alert = Vec::new();
                    alert.put_u32(1);
                    alert.put_lp2_string(&payload)?;
                    return Ok(vec![Frame {
                        seq,
                        id: wire::PacketId(AnsUserSearchInfo::ID.0 | 0x01),
                        payload: alert.into(),
                    }]);
                };
                let language = Language::default();
                let names = [
                    world::SERVERS[place.server as usize - 1].0.to_owned(),
                    place
                        .gate
                        .map(|g| text(Text::GateName(g), language).into_owned())
                        .unwrap_or_default(),
                    place
                        .city
                        .map(|c| text(Text::CityName(c), language).into_owned())
                        .unwrap_or_default(),
                ];
                let (a, b) = search_ids(&id);
                let info = PatData::new()
                    .with(0x01, Item::String(r.capcom_id))
                    .with(0x02, Item::String(place.name.clone()))
                    .with(0x03, Item::Binary(place.profile.bytes()))
                    // Where "Warp to" goes (M5 D11): a player on a quest is found in their city.
                    .with(
                        0x04,
                        Item::Binary(Bytes::copy_from_slice(&world::jump_path(
                            self.version,
                            place.server,
                            place.gate,
                            place.city,
                        ))),
                    )
                    .with(0x07, Item::Byte(1))
                    .with(0x08, Item::String(names.join("\t").into()))
                    .with(0x0b, Item::Byte(1))
                    // Unknown; MH3SP's values.
                    .with(0x0c, Item::String(Bytes::from_static(b"StrC")))
                    .with(0x0d, Item::Long(4))
                    .with(0x0e, Item::Long(3))
                    .with(0x0f, Item::Long(a))
                    .with(0x10, Item::Long(b));
                reply(
                    seq,
                    &AnsUserSearchInfo {
                        info,
                        extra: OptionalFields::default(),
                    },
                )
            }
            NtcCollectionLog::ID => {
                // The console reporting an error it ran into.
                let r = NtcCollectionLog::decode(p)?;
                let code = match r.log.get(0x01) {
                    Some(Item::Long(code)) => *code,
                    _ => 0,
                };
                warn!(
                    code = format_args!("{code:#010x}"),
                    "console reported an error"
                );
                Ok(vec![])
            }
            ReqLayerStart::ID => {
                ReqLayerStart::decode(p)?;
                reply(
                    seq,
                    &AnsLayerStart {
                        layer: placeholder_layer(),
                    },
                )
            }
            ReqLayerEnd::ID => {
                // Back to the title, or to the server selection.
                ReqLayerEnd::decode(p)?;
                let mut world = shared.world();
                depart(
                    shared,
                    &mut world,
                    capcom_id(session),
                    self.mailbox.conn,
                    Depart::World,
                );
                reply(seq, &AnsLayerEnd {})
            }
            ReqLayerUserList::ID => {
                ReqLayerUserList::decode(p)?;
                let world = shared.world();
                let users = world
                    .neighbours(capcom_id(session))
                    .into_iter()
                    .map(|(id, place)| {
                        PatData::new()
                            .with(0x01, Item::String(Bytes::copy_from_slice(id.as_bytes())))
                            .with(0x02, Item::String(place.name.clone()))
                            .with(0x07, Item::Binary(place.profile.bytes()))
                    })
                    .collect();
                reply(seq, &AnsLayerUserList { users })
            }
            ReqUserStatusSet::ID => {
                // What the player is doing (searchable, M3).
                ReqUserStatusSet::decode(p)?;
                reply(seq, &AnsUserStatusSet {})
            }
            ReqUserBinaryNotice::ID => {
                // The profile changed: the others at the player's place get the part named, from the validated
                // block; with `target` 1, only the player the request names (if there too: the decomp's sender).
                // Never a window past the block: the receiving console copies it at the offset unchecked.
                let r = ReqUserBinaryNotice::decode(p)?;
                let me = capcom_id(session);
                let block = session.profile.bytes();
                let start = r.offset as usize;
                match start
                    .checked_add(r.size as usize)
                    .filter(|&end| end <= block.len())
                {
                    Some(end) => {
                        let ntc = NtcUserBinaryNotice {
                            target: r.target,
                            capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                            offset: r.offset,
                            data: block.slice(start..end),
                        };
                        match r.target {
                            ONE_PLAYER => {
                                let to = String::from_utf8_lossy(&r.capcom_id).into_owned();
                                let here = shared.world().others(me).contains(&to);
                                if here {
                                    shared.players.send(&to, frame(seq, &ntc)?);
                                }
                            }
                            _ => {
                                relay(shared, me, &ntc, seq)?;
                            }
                        }
                    }
                    None => warn!(
                        offset = r.offset,
                        size = r.size,
                        "profile notice past the block refused"
                    ),
                }
                reply(seq, &AnsUserBinaryNotice {})
            }
            ReqLayerChildListHead::ID => {
                ReqLayerChildListHead::decode(p)?;
                let (_, gate, _) = location(shared, session)?;
                let count = match gate {
                    None => world::GATES,
                    Some(_) => world::CITIES,
                };
                reply(
                    seq,
                    &AnsLayerChildListHead {
                        first: 0,
                        count: u32::from(count),
                    },
                )
            }
            ReqLayerChildListData::ID => {
                let r = ReqLayerChildListData::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                let world = shared.world();
                let children = match gate {
                    None => world::gates(&world, server, r.first, r.count),
                    Some(gate) => world::cities(&world, server, gate, r.first, r.count),
                };
                reply(
                    seq,
                    &AnsLayerChildListData {
                        first: r.first,
                        children,
                    },
                )
            }
            ReqLayerChildListFoot::ID => {
                ReqLayerChildListFoot::decode(p)?;
                reply(seq, &AnsLayerChildListFoot {})
            }
            ReqLayerSiblingListHead::ID => {
                // Change Gates (a gate's entrance, Relocate): the layers beside the player's, as MH3SP: its world
                // server's gates, or its gate's cities. Picking one goes through `ReqLayerJump*` (not built).
                ReqLayerSiblingListHead::decode(p)?;
                let count = match location(shared, session)? {
                    (_, Some(_), None) => world::GATES,
                    (_, Some(_), Some(_)) => world::CITIES,
                    _ => {
                        return Err(Fault::Client(anyhow!(
                            "no siblings at the world server level"
                        )));
                    }
                };
                reply(
                    seq,
                    &AnsLayerSiblingListHead {
                        first: 0,
                        count: u32::from(count),
                    },
                )
            }
            ReqLayerSiblingListData::ID => {
                let r = ReqLayerSiblingListData::decode(p)?;
                let place = location(shared, session)?;
                let world = shared.world();
                // Each row with the path a pick jumps to (item 2, M5 D11).
                let version = self.version;
                let to = |path: [u8; 14], (row, settings): (PatData, OptionalFields)| {
                    let row = PatData(row.0.into_iter().filter(|(f, _)| *f != 0x02).collect());
                    let row = row.with(0x02, Item::Binary(Bytes::copy_from_slice(&path)));
                    (row, settings)
                };
                let first = r.first.max(1);
                let siblings = match place {
                    (server, Some(_), None) => (first..)
                        .zip(world::gates(&world, server, first, r.count))
                        .map(|(g, row)| {
                            to(world::jump_path(version, server, Some(g as u16), None), row)
                        })
                        .collect(),
                    (server, Some(gate), Some(_)) => (first..)
                        .zip(world::cities(&world, server, gate, first, r.count))
                        .map(|(c, row)| {
                            to(
                                world::jump_path(version, server, Some(gate), Some(c as u16)),
                                row,
                            )
                        })
                        .collect(),
                    _ => {
                        return Err(Fault::Client(anyhow!(
                            "no siblings at the world server level"
                        )));
                    }
                };
                reply(
                    seq,
                    &AnsLayerSiblingListData {
                        first: r.first,
                        siblings,
                    },
                )
            }
            ReqLayerDetailSearchHead::ID => {
                // City search, not built (M5 D10): no city found, rather than a dropped connection (M5 D15).
                ReqLayerDetailSearchHead::decode(p)?;
                reply(
                    seq,
                    &AnsLayerDetailSearchHead {
                        unk: 0,
                        count: 0,
                        unk2: 0,
                    },
                )
            }
            ReqLayerDetailSearchData::ID => {
                ReqLayerDetailSearchData::decode(p)?;
                reply(seq, &AnsLayerDetailSearchData { unk: 0, count: 0 })
            }
            ReqLayerDetailSearchFoot::ID => {
                ReqLayerDetailSearchFoot::decode(p)?;
                reply(seq, &AnsLayerDetailSearchFoot {})
            }
            // No known sender (review, 2026-10-11): what the client expects, nothing more.
            ReqLayerInfoSet::ID => {
                ReqLayerInfoSet::decode(p)?;
                reply(seq, &AnsLayerInfoSet {})
            }
            ReqLayerUserInfoSet::ID => {
                ReqLayerUserInfoSet::decode(p)?;
                reply(seq, &AnsLayerUserInfoSet {})
            }
            ReqCircleSearchHead::ID => {
                ReqCircleSearchHead::decode(p)?;
                reply(seq, &AnsCircleSearchHead { unk: 0, total: 0 })
            }
            ReqCircleSearchFoot::ID => {
                ReqCircleSearchFoot::decode(p)?;
                reply(seq, &AnsCircleSearchFoot {})
            }
            ReqLayerSiblingListFoot::ID => {
                ReqLayerSiblingListFoot::decode(p)?;
                reply(seq, &AnsLayerSiblingListFoot {})
            }
            ReqLayerChildInfo::ID => {
                // A layer's record: `unk` 0 the player's own layer, else that child of it (a gate from a world
                // server, a city from a gate). Answered with the same `unk`: only for 0 does the console take the
                // record as its own layer's (Local Info: population, capacity, then the city's settings: HR limit,
                // goal, seeking). MH3SP answered 1 and a placeholder (0 of 4 players) every time; the world server
                // level keeps the placeholder.
                let r = ReqLayerChildInfo::decode(p)?;
                let (server, gate, city) = location(shared, session)?;
                let world = shared.world();
                let record = match (r.layer, gate, city) {
                    (0, Some(g), Some(c)) => {
                        world::cities(&world, server, g, u32::from(c), 1).pop()
                    }
                    (0, Some(g), None) => world::gates(&world, server, u32::from(g), 1).pop(),
                    (n, None, _) if n > 0 => world::gates(&world, server, u32::from(n), 1).pop(),
                    (n, Some(g), None) if n > 0 => {
                        world::cities(&world, server, g, u32::from(n), 1).pop()
                    }
                    _ => None,
                };
                let (layer, settings) =
                    record.unwrap_or_else(|| (placeholder_layer(), OptionalFields::default()));
                // A trailing 0 after the settings, as MH3SP sends (the console doesn't read it).
                reply(
                    seq,
                    &AnsLayerChildInfo {
                        layer: r.layer,
                        record: layer,
                        settings,
                        rest: Rest(Bytes::from_static(&[0])),
                    },
                )
            }
            ReqLayerDown::ID => {
                let r = ReqLayerDown::decode(p)?;
                let me = capcom_id(session);
                let (server, gate, city) = location(shared, session)?;
                let mut world = shared.world();
                match (gate, city) {
                    (None, _) if (1..=world::GATES).contains(&r.layer) => {
                        enter_gate(shared, &mut world, me, r.layer, seq);
                    }
                    (Some(gate), None) => {
                        if let Err(refusal) =
                            enter_city(shared, &mut world, me, (server, gate, r.layer))
                        {
                            return alert(AnsLayerDown::ID, seq, refusal.text());
                        }
                    }
                    _ => return Err(Fault::Client(anyhow!("no layer {} below", r.layer))),
                }
                reply(seq, &AnsLayerDown { layer: r.layer })
            }
            ReqLayerUp::ID => {
                ReqLayerUp::decode(p)?;
                let mut world = shared.world();
                depart(
                    shared,
                    &mut world,
                    capcom_id(session),
                    self.mailbox.conn,
                    Depart::Up,
                );
                reply(seq, &AnsLayerUp {})
            }
            ReqLayerJump::ID => {
                // A jump within the world server (M5 D10, D12): Change Gates, a found player's card, a friend's.
                let r = ReqLayerJump::decode(p)?;
                let (server, gate, city) = world::jump_target(&r.path)
                    .ok_or_else(|| Fault::Client(anyhow!("no such layer to jump to")))?;
                let me = capcom_id(session);
                let mut world = shared.world();
                if world.place(me).map(|p| p.server) != Some(server) {
                    return Err(Fault::Client(anyhow!("a jump to another world server")));
                }
                if let Err(refusal) =
                    jump(shared, &mut world, me, self.mailbox.conn, (gate, city), seq)
                {
                    return alert(AnsLayerJump::ID, seq, refusal.text());
                }
                reply(seq, &AnsLayerJump {})
            }
            ReqLayerJumpReady::ID => {
                // A jump to another world server (M5 D13): the target checked, a one-time ticket for it. The console
                // then reconnects there (`FmpInfo2`, `LayerEnd`, `Shut`) and finishes with `LayerJumpGo`.
                let r = ReqLayerJumpReady::decode(p)?;
                let target = world::jump_target(&r.path)
                    .ok_or_else(|| Fault::Client(anyhow!("no such layer to jump to")))?;
                if let (server, gate, Some(city)) = target
                    && let Err(refusal) = shared.world().can_join((server, gate, city))
                {
                    return alert(NtcLayerJumpReady::ID, seq, refusal.text());
                }
                let mut ticket = [0; 4];
                openssl::rand::rand_bytes(&mut ticket).map_err(|e| Fault::Server(e.into()))?;
                let ticket = u32::from_be_bytes(ticket);
                shared
                    .jumps
                    .insert(ticket, (capcom_id(session).to_owned(), target));
                reply(seq, &NtcLayerJumpReady { ticket })
            }
            ReqLayerJumpGo::ID => {
                // Back on the target's world server: its ticket places the player, once.
                let r = ReqLayerJumpGo::decode(p)?;
                let target = world::jump_target(&r.path);
                let me = capcom_id(session);
                let mut world = shared.world();
                let valid = shared
                    .jumps
                    .remove(&r.ticket)
                    .is_some_and(|(who, to)| who == me && Some(to) == target)
                    && world.place(me).map(|p| p.server) == target.map(|t| t.0);
                let Some((_, gate, city)) = target.filter(|_| valid) else {
                    return alert(NtcLayerJumpGo::ID, seq, Text::JumpFailed);
                };
                if let Err(refusal) =
                    jump(shared, &mut world, me, self.mailbox.conn, (gate, city), seq)
                {
                    return alert(NtcLayerJumpGo::ID, seq, refusal.text());
                }
                reply(seq, &NtcLayerJumpGo {})
            }
            ReqLayerHost::ID => {
                // A console entering a city asks who hosts its P2P session; the others learn of the arrival.
                let r = ReqLayerHost::decode(p)?;
                let me = capcom_id(session);
                let world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                let host = world
                    .city_host(city)
                    .ok_or_else(|| Fault::Client(anyhow!("a city without a host")))?;
                let host_name = world
                    .players_at(city.0, Some(city.1), Some(city.2))
                    .into_iter()
                    .find(|(id, _)| *id == host)
                    .map(|(_, p)| p.name.clone())
                    .unwrap_or_default();
                let answer = frame(
                    seq,
                    &AnsLayerHost {
                        layer: r.layer,
                        capcom_id: Bytes::copy_from_slice(host.as_bytes()),
                        name: host_name,
                    },
                )?;
                let ntc = layer_in(&world, me, seq)?;
                for other in world.others(me) {
                    shared.players.send(&other, ntc.clone());
                }
                Ok(vec![answer])
            }
            ReqLayerUserListHead::ID => {
                // The players at a layer (the console looks for a city's host this way): kept for the data request.
                let r = ReqLayerUserListHead::decode(p)?;
                let (server, gate, city) = world::parse_path(&r.layer)
                    .ok_or_else(|| Fault::Client(anyhow!("malformed layer path")))?;
                let world = shared.world();
                self.search = world
                    .players_at(server, gate, city)
                    .into_iter()
                    .skip(r.first.saturating_sub(1) as usize)
                    .take(r.count as usize)
                    .map(|(id, _)| id.to_owned())
                    .collect();
                reply(
                    seq,
                    &AnsLayerUserListHead {
                        first: r.first,
                        count: self.search.len() as u32,
                    },
                )
            }
            ReqLayerUserListData::ID => {
                ReqLayerUserListData::decode(p)?;
                let users = found_users(shared, &self.search, false);
                reply(seq, &AnsLayerUserListData { first: 1, users })
            }
            ReqLayerUserListFoot::ID => {
                ReqLayerUserListFoot::decode(p)?;
                reply(seq, &AnsLayerUserListFoot {})
            }
            NtcLayerUserPosition::ID => {
                // A position in town: to the others there, with the sender's Capcom ID in front.
                let r = NtcLayerUserPosition::decode(p)?;
                let me = capcom_id(session);
                let mut data = Vec::new();
                data.put_lp2_string(me.as_bytes())?;
                data.extend_from_slice(&r.data.0);
                relay(
                    shared,
                    me,
                    &NtcLayerUserPosition {
                        data: Rest(data.into()),
                    },
                    seq,
                )
            }
            NtcLayerBinary::ID => {
                // Game data for the layer: relayed with the sender's Capcom ID and name in front.
                let r = NtcLayerBinary::decode(p)?;
                let me = capcom_id(session);
                let body = without_sender(&r.data.0)?;
                #[cfg(feature = "bot")]
                bot::peer_message_from(shared, me, body);
                let mut data = Vec::new();
                data.put_lp2_string(me.as_bytes())?;
                data.put_pat_data(&sender(me, &session.name, true))?;
                data.extend_from_slice(body);
                relay(
                    shared,
                    me,
                    &NtcLayerBinary {
                        data: Rest(data.into()),
                    },
                    seq,
                )
            }
            NtcLayerBinary2::ID => {
                // Game data for one player of the layer (the P2P connection offers): to them only.
                let r = NtcLayerBinary2::decode(p)?;
                let me = capcom_id(session);
                let partner = String::from_utf8_lossy(&r.capcom_id).into_owned();
                if !shared.world().others(me).contains(&partner) {
                    return Ok(vec![]);
                }
                #[cfg(feature = "bot")]
                if bot::to_bot(shared, me, &partner, without_sender(&r.data.0)?) {
                    return Ok(vec![]);
                }
                let mut data = Vec::new();
                data.put_pat_data(&sender(me, &session.name, false))?;
                data.extend_from_slice(without_sender(&r.data.0)?);
                let ntc = frame(
                    seq,
                    &NtcLayerBinary2 {
                        capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                        data: Rest(data.into()),
                    },
                )?;
                shared.players.send(&partner, ntc);
                Ok(vec![])
            }
            ReqLayerCreateHead::ID => {
                let r = ReqLayerCreateHead::decode(p)?;
                let (server, gate, city) = location(shared, session)?;
                let (Some(gate), None) = (gate, city) else {
                    return Err(Fault::Client(anyhow!("city created outside a gate")));
                };
                if !(1..=world::CITIES).contains(&r.number) {
                    return Err(Fault::Client(anyhow!("no city {}", r.number)));
                }
                let me = capcom_id(session);
                let id = (server, gate, r.number);
                let mut world = shared.world();
                if let Err(refusal) = world.reserve_city(id, me) {
                    return alert(AnsLayerCreateHead::ID, seq, refusal.text());
                }
                gate_record(shared, &world, me, id)?;
                reply(seq, &AnsLayerCreateHead { number: r.number })
            }
            ReqLayerCreateSet::ID => {
                let r = ReqLayerCreateSet::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                let gate =
                    gate.ok_or_else(|| Fault::Client(anyhow!("city created outside a gate")))?;
                let me = capcom_id(session);
                let id = (server, gate, r.number);
                let mut world = shared.world();
                if let Err(refusal) = world.create_city(id, me, r.extra) {
                    return alert(AnsLayerCreateSet::ID, seq, refusal.text());
                }
                info!(gate, city = r.number, "created city");
                gate_counts(shared, &world, me, id)?;
                reply(seq, &AnsLayerCreateSet { number: r.number })
            }
            ReqLayerCreateFoot::ID => {
                let r = ReqLayerCreateFoot::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                if let Some(gate) = gate {
                    let me = capcom_id(session);
                    let id = (server, gate, r.number);
                    let mut world = shared.world();
                    world.release_city(id, me);
                    gate_record(shared, &world, me, id)?;
                }
                reply(seq, &AnsLayerCreateFoot { number: r.number })
            }
            ReqLayerMediationList::ID => {
                // The city's taken seats (1–12) and arm wrestling places (13–14): holder, slot, taken. The
                // console clears its 32 slots first. (MH3SP: 14 free placeholder entries named "Test".)
                ReqLayerMediationList::decode(p)?;
                let world = shared.world();
                let seats = world
                    .city_of(capcom_id(session))
                    .and_then(|id| world.city(id))
                    .map(|city| city.seats.clone())
                    .unwrap_or_default();
                let items = seats
                    .into_iter()
                    .map(|(slot, holder)| {
                        PatData::new()
                            .with(0x01, Item::String(holder.into()))
                            .with(0x02, Item::Byte(slot))
                            .with(0x03, Item::Byte(1))
                    })
                    .collect();
                reply(
                    seq,
                    &AnsLayerMediationList {
                        mode: 0,
                        items: ShortList(items),
                    },
                )
            }
            ReqLayerMediationLock::ID => {
                // Sitting down (or taking an arm wrestling place): one player per seat; the city sees it. A taken
                // seat is refused (an alert: the console then doesn't sit). (MH3SP: always yes, nobody told.)
                let r = ReqLayerMediationLock::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                if !world.take_seat(city, r.slot, me) {
                    return alert(AnsLayerMediationLock::ID, seq, Text::SeatTaken);
                }
                let ntc = frame(seq, &seat_notice(me, r.slot))?;
                for other in world.others(me) {
                    shared.players.send(&other, ntc.clone());
                }
                reply(seq, &AnsLayerMediationLock { result: 1 })
            }
            ReqLayerMediationUnlock::ID => {
                // Standing up: the seat is free again, the city sees it.
                let r = ReqLayerMediationUnlock::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                if world.free_seat(city, r.slot, me) {
                    let ntc = frame(seq, &seat_free_notice(me, r.slot))?;
                    for other in world.others(me) {
                        shared.players.send(&other, ntc.clone());
                    }
                }
                reply(seq, &AnsLayerMediationUnlock { result: 1 })
            }
            ReqCircleListLayer::ID => {
                ReqCircleListLayer::decode(p)?;
                let world = shared.world();
                let city = world.city_of(capcom_id(session)).ok_or_else(not_in_city)?;
                reply(
                    seq,
                    &AnsCircleListLayer {
                        first: 0,
                        circles: world.circles_in(city),
                    },
                )
            }
            ReqCircleMatchOptionSet::ID => {
                let r = ReqCircleMatchOptionSet::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world();
                // A race, not a fault: the quest just broke, or departed without the player.
                let Some(id) = world.circle_of(me) else {
                    return reply(seq, &AnsCircleMatchOptionSet {});
                };
                world.set_standby(me, matches!(r.options.get(0x03), Some(Item::Byte(1))));
                let circle = world.circle(id).expect("circle of the player");
                let options = set(
                    set(
                        set(
                            r.options,
                            0x05,
                            Item::String(Bytes::copy_from_slice(me.as_bytes())),
                        ),
                        0x06,
                        Item::String(session.name.clone()),
                    ),
                    0x04,
                    Item::Byte(circle.index_of(me).unwrap_or(0)),
                );
                let others: Vec<String> = circle
                    .members()
                    .filter(|(_, m)| *m != me)
                    .map(|(_, m)| m.to_owned())
                    .collect();
                let ntc = frame(seq, &NtcCircleMatchOptionSet { options })?;
                let mut out = Vec::new();
                notify(shared, me, &others, &ntc, &mut out);
                out.push(frame(seq, &AnsCircleMatchOptionSet {})?);
                Ok(out)
            }
            ReqCircleInfo::ID => {
                let r = ReqCircleInfo::decode(p)?;
                let world = shared.world();
                let city = world.city_of(capcom_id(session)).ok_or_else(not_in_city)?;
                let (info, extra) = world.circle_info((city, r.circle));
                reply(
                    seq,
                    &AnsCircleInfo {
                        circle: r.circle,
                        info,
                        extra,
                    },
                )
            }
            ReqCircleInfoSet::ID => {
                // Relayed to the circle; MH3SP doesn't apply it to the circle (the list keeps its creation values).
                let r = ReqCircleInfoSet::decode(p)?;
                let me = capcom_id(session);
                let world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                let id = (city, r.circle);
                // Only its leader sets a quest's details: a modified client.
                if world.circle(id).is_some_and(|c| c.leader != me) {
                    warn!(
                        circle = id.1,
                        "quest details set by a member, not its leader: refused"
                    );
                    return reply(seq, &AnsCircleInfoSet { circle: r.circle });
                }
                let others: Vec<String> = world
                    .circle(id)
                    .map(|c| {
                        c.members()
                            .filter(|(_, m)| *m != me)
                            .map(|(_, m)| m.to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let ntc = frame(
                    seq,
                    &NtcCircleInfoSet {
                        circle: r.circle,
                        info: r.info,
                        extra: r.extra,
                    },
                )?;
                let mut out = Vec::new();
                notify(shared, me, &others, &ntc, &mut out);
                out.push(frame(seq, &AnsCircleInfoSet { circle: r.circle })?);
                list_change(shared, &world, me, id, seq, &mut out)?;
                Ok(out)
            }
            ReqCircleMatchStart::ID => {
                ReqCircleMatchStart::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world();
                let id = world.circle_of(me).ok_or_else(not_in_circle)?;
                // Only its leader departs a quest (the game offers no one else the choice): a modified client.
                if world.circle(id).is_some_and(|c| c.leader != me) {
                    warn!(
                        circle = id.1,
                        "quest departure asked by a member, not its leader: refused"
                    );
                    return reply(seq, &AnsCircleMatchStart {});
                }
                // Members not ready stay behind: removed from the party (MH3SP's rule, M3 D7).
                let mut out = Vec::new();
                let behind: Vec<String> = world
                    .circle_members(id)
                    .into_iter()
                    .filter(|(_, _, _, ready)| !ready)
                    .map(|(_, m, _, _)| m)
                    .collect();
                // Departed first: the list changes sent with the removals already show the quest closed (MH3SP's order).
                let ready = world.depart(id);
                for member in &behind {
                    shared.players.send(
                        member,
                        frame(
                            seq,
                            &NtcCircleKick {
                                kind: 0,
                                text: Bytes::new(),
                            },
                        )?,
                    );
                    quit_quest(shared, &mut world, member, me, seq, &mut out)?;
                }
                info!(circle = id.1, players = ready.len(), "quest departs");
                // The players departing: count, then per player its index, Capcom ID, a flag and an unknown 21.
                let mut data = Vec::new();
                data.put_u32(ready.len() as u32);
                for (index, member) in &ready {
                    data.put_u8(*index);
                    data.put_lp2_string(member.as_bytes())?;
                    data.put_lp2_string(b"\x01")?;
                    data.put_u16(21);
                }
                let members: Vec<String> = world
                    .circle(id)
                    .map(|c| c.members().map(|(_, m)| m.to_owned()).collect())
                    .unwrap_or_default();
                let ntc = frame(
                    seq,
                    &NtcCircleMatchStart {
                        data: data.into(),
                        rest: Rest(Bytes::from_static(&[0, 0, 0, 1])),
                    },
                )?;
                notify(shared, me, &members, &ntc, &mut out);
                if !behind.is_empty() {
                    list_change(shared, &world, me, id, seq, &mut out)?;
                }
                out.push(frame(seq, &AnsCircleMatchStart {})?);
                Ok(out)
            }
            NtcCircleBinary::ID => {
                // Game data for the circle: relayed with the sender's Capcom ID and name in front.
                let r = NtcCircleBinary::decode(p)?;
                let mut reader = Reader::new(&r.data.0);
                reader.pat_data()?;
                let me = capcom_id(session);
                let body = reader.rest();
                let world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                let others: Vec<String> = world
                    .circle((city, r.circle))
                    .map(|c| {
                        c.members()
                            .filter(|(_, m)| *m != me)
                            .map(|(_, m)| m.to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let ntc = circle_binary(seq, me, &session.name, r.circle, body)?;
                let mut out = Vec::new();
                notify(shared, me, &others, &ntc, &mut out);
                #[cfg(feature = "bot")]
                bot::party_sync(shared, &world, me, &others, (seq, r.circle), body, &mut out)?;
                Ok(out)
            }
            ReqCircleMatchEnd::ID => {
                ReqCircleMatchEnd::decode(p)?;
                reply(seq, &AnsCircleMatchEnd {})
            }
            ReqCircleLeave::ID => {
                let r = ReqCircleLeave::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world();
                let mut out = Vec::new();
                quit_quest(shared, &mut world, me, me, seq, &mut out)?;
                out.push(frame(seq, &AnsCircleLeave { circle: r.circle })?);
                Ok(out)
            }
            ReqCircleUserList::ID => {
                // The party: per member, ready or not, index, Capcom ID, name.
                ReqCircleUserList::decode(p)?;
                let world = shared.world();
                let me = capcom_id(session);
                // Out of a quest that just ended: nobody.
                let users = world
                    .circle_of(me)
                    .map(|id| world.circle_members(id))
                    .unwrap_or_default();
                drop(world);
                let users = users
                    .into_iter()
                    .map(|(index, id, name, ready)| {
                        PatData::new()
                            .with(0x03, Item::Byte(u8::from(ready)))
                            .with(0x04, Item::Byte(index))
                            .with(0x05, Item::String(id.into()))
                            .with(0x06, Item::String(name))
                    })
                    .collect();
                reply(seq, &AnsCircleUserList { users })
            }
            ReqCircleHost::ID => {
                let r = ReqCircleHost::decode(p)?;
                let world = shared.world();
                let city = world.city_of(capcom_id(session)).ok_or_else(not_in_city)?;
                let (index, id, name) = world
                    .circle_members((city, r.circle))
                    .into_iter()
                    .find(|(_, id, _, _)| {
                        world
                            .circle((city, r.circle))
                            .is_some_and(|c| &c.leader == id)
                    })
                    .map(|(i, id, name, _)| (i, id, name))
                    .ok_or_else(|| Fault::Client(anyhow!("no quest {}", r.circle)))?;
                reply(
                    seq,
                    &AnsCircleHost {
                        circle: r.circle,
                        leader: index,
                        capcom_id: id.into(),
                        name,
                    },
                )
            }
            NtcCircleBinary2::ID => {
                // Party data for one member: to them only, the sender filled in.
                let r = NtcCircleBinary2::decode(p)?;
                let me = capcom_id(session);
                let partner = String::from_utf8_lossy(&r.capcom_id).into_owned();
                let world = shared.world();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                let in_party = world
                    .circle((city, r.circle))
                    .is_some_and(|c| c.index_of(&partner).is_some());
                drop(world);
                if in_party {
                    let mut data = Vec::new();
                    data.put_pat_data(&sender(me, &session.name, false))?;
                    data.extend_from_slice(without_sender(&r.data.0)?);
                    let ntc = NtcCircleBinary2 {
                        circle: r.circle,
                        capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                        data: Rest(data.into()),
                    };
                    shared.players.send(&partner, frame(seq, &ntc)?);
                }
                Ok(vec![])
            }
            NtcLayerChat::ID => {
                // Chat: to the others at the sender's place, with the sender filled in and the place's colour
                // (MH3SP's: pink on a world server, white in gates and cities).
                let r = NtcLayerChat::decode(p)?;
                let me = capcom_id(session);
                if let Some(answer) = commands::run(shared, me, &session.name, &r.text) {
                    return Ok(commands::answer(shared, me, &answer));
                }
                let (_, gate, _) = location(shared, session)?;
                let colour = if gate.is_some() {
                    0xffff_ffff
                } else {
                    0xbb33_85ff
                };
                let info = set(
                    set(
                        set(r.info, 0x01, Item::Long(colour)),
                        0x03,
                        Item::String(Bytes::copy_from_slice(me.as_bytes())),
                    ),
                    0x04,
                    Item::String(session.name.clone()),
                );
                relay(
                    shared,
                    me,
                    &NtcLayerChat {
                        channel: r.channel,
                        info,
                        text: r.text,
                    },
                    seq,
                )
            }
            NtcCircleChat::ID => {
                // Party chat through the server (the game's fallback without a P2P session): to the other members,
                // the sender filled in (MH3SP's).
                let r = NtcCircleChat::decode(p)?;
                let me = capcom_id(session);
                if let Some(answer) = commands::run(shared, me, &session.name, &r.text) {
                    return Ok(commands::answer(shared, me, &answer));
                }
                let info = set(
                    set(
                        r.info,
                        0x03,
                        Item::String(Bytes::copy_from_slice(me.as_bytes())),
                    ),
                    0x04,
                    Item::String(session.name.clone()),
                );
                let world = shared.world();
                let ntc = frame(seq, &NtcCircleChat { info, text: r.text })?;
                for (_, member, _, _) in world
                    .circle_of(me)
                    .map(|id| world.circle_members(id))
                    .unwrap_or_default()
                {
                    if member != me {
                        shared.players.send(&member, ntc.clone());
                    }
                }
                Ok(vec![])
            }
            ReqCircleInfoNoticeSet::ID => {
                ReqCircleInfoNoticeSet::decode(p)?;
                reply(seq, &AnsCircleInfoNoticeSet {})
            }
            NtcCheatCheck::ID => {
                // The game's integrity report: read with anti-cheat (M9).
                trace!(len = p.len(), "cheat check");
                Ok(vec![])
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
                        first: 0,
                        count: world::SERVERS.len() as u32,
                    },
                )
            }
            ReqFmpListData2::ID => {
                let r = ReqFmpListData2::decode(p)?;
                let servers = world::list(shared, r.first, r.count)?;
                reply(seq, &AnsFmpListData2 { first: 0, servers })
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
            let mut world = shared.world();
            depart(
                shared,
                &mut world,
                capcom_id,
                self.mailbox.conn,
                Depart::World,
            );
            drop(world);
            shared.players.unregister(capcom_id, self.mailbox.conn);
        }
    }
}

/// MH3SP's placeholder layer (`AnsLayerStart`, `AnsLayerChildInfo`): the console only needs the fields to exist.
fn placeholder_layer() -> PatData {
    PatData::new()
        .with(0x05, Item::Word(1))
        .with(0x03, Item::String(Bytes::from_static(b"LayerStart")))
        .with(0x06, Item::Long(0))
        .with(0x09, Item::Long(4))
        .with(0x10, Item::Byte(1))
        .with(0x11, Item::Long(world::POSITION_INTERVAL))
        .with(0x12, Item::Byte(1))
}

/// Sends `ntc` to `to`: the requester's own copy goes in its answer (`out`), in order; the others' to their
/// mailboxes.
fn notify(shared: &Shared, me: &str, to: &[String], ntc: &Frame, out: &mut Vec<Frame>) {
    for player in to {
        match player == me {
            true => out.push(ntc.clone()),
            false => {
                shared.players.send(player, ntc.clone());
            }
        }
    }
}

#[cfg(feature = "bot")]
mod bot;
#[cfg(feature = "bot")]
pub use bot::{add_bot, bot_says, remove_bot};

/// A party message as the members get it: the sender's Capcom ID and name in front.
fn circle_binary(
    seq: u16,
    sender: &str,
    name: &Bytes,
    circle: u32,
    body: &[u8],
) -> Result<Frame, Fault> {
    let mut data = Vec::new();
    data.put_lp2_string(sender.as_bytes())?;
    data.put_pat_data(
        &PatData::new()
            .with(0x01, Item::Long(0x1234_5678))
            .with(
                0x02,
                Item::String(Bytes::copy_from_slice(sender.as_bytes())),
            )
            .with(0x03, Item::String(name.clone())),
    )?;
    data.extend_from_slice(body);
    frame(
        seq,
        &NtcCircleBinary {
            circle,
            data: Rest(data.into()),
        },
    )
}

/// The player enters gate `gate` of its world server, from the server's root: those there see it arrive.
fn enter_gate(shared: &Shared, world: &mut World, me: &str, gate: u16, seq: u16) {
    world.place(me).expect("located").gate = Some(gate);
    world.arrive(me);
    info!(gate, "entered gate");
    if let Ok(ntc) = layer_in(world, me, seq) {
        for other in world.others(me) {
            shared.players.send(&other, ntc.clone());
        }
    }
}

/// The player enters city `id` from its gate, if the city can take it: those in it get its new counts (the console
/// announces itself with `LayerHost`).
fn enter_city(
    shared: &Shared,
    world: &mut World,
    me: &str,
    id: (u32, u16, u16),
) -> Result<(), world::CityRefusal> {
    #[cfg(feature = "bot")]
    bot::make_room(shared, world, me, id);
    world.join_city(id, me)?;
    info!(gate = id.1, city = id.2, "entered city");
    // Best effort: the player is in.
    let _ = user_num(shared, world, me, id);
    Ok(())
}

/// A jump to `gate` (and `city`) of the player's world server (M5 D12): leaving where it is and walking in, with the
/// same notices; a city that can't take the player refuses before it moves.
fn jump(
    shared: &Shared,
    world: &mut World,
    me: &str,
    conn: u64,
    (gate, city): (u16, Option<u16>),
    seq: u16,
) -> Result<(), world::CityRefusal> {
    let place = world.place(me).expect("located");
    let (server, here) = (place.server, (place.gate, place.city));
    if let Some(city) = city {
        world.can_join((server, gate, city))?;
    }
    if here.1.is_some() && here != (Some(gate), city) {
        depart(shared, world, me, conn, Depart::Up);
    }
    if here.0.is_some_and(|g| g != gate) {
        depart(shared, world, me, conn, Depart::Up);
    }
    if world.place(me).expect("located").gate.is_none() {
        enter_gate(shared, world, me, gate, seq);
    }
    match city {
        Some(city) if world.place(me).expect("located").city != Some(city) => {
            enter_city(shared, world, me, (server, gate, city))
        }
        _ => Ok(()),
    }
}

enum Depart {
    /// One level up (a city to its gate, a gate to its world server).
    Up,
    /// Out of the world.
    World,
}

/// A player leaves a layer: those left there learn of it (`NtcLayerOut`); a city host leaving hands the city's P2P
/// session to the earliest player left (`NtcLayerHost`). Nothing if another connection holds the player's place.
fn depart(shared: &Shared, world: &mut World, me: &str, conn: u64, how: Depart) {
    if !world.placed_by(me, conn) {
        return;
    }
    #[cfg(feature = "bot")]
    bot::departing(shared, me, conn);
    // Leaving a city (or the game) means leaving its quest first; the requester's own notices are of no use now.
    let mut ignored = Vec::new();
    let in_city = world.city_of(me).is_some();
    let leaves_city =
        matches!(how, Depart::World) || world.place(me).is_some_and(|p| p.city.is_some());
    if in_city && leaves_city {
        let _ = quit_quest(shared, world, me, me, 0, &mut ignored);
    }
    let others = world.others(me);
    let city = world.city_of(me);
    let was_host = city.is_some_and(|id| world.city_host(id) == Some(me));
    // Its seats are free again for those still in the city.
    let freed = match in_city && leaves_city {
        true => world.release_seats(me),
        false => Vec::new(),
    };
    for slot in freed {
        if let Ok(ntc) = frame(0, &seat_free_notice(me, slot)) {
            for other in &others {
                shared.players.send(other, ntc.clone());
            }
        }
    }
    match how {
        Depart::Up => {
            let place = world.place(me).expect("placed");
            if place.city.is_some() {
                world.leave_city(me);
            } else {
                place.gate = None;
                world.arrive(me);
            }
        }
        Depart::World => world.leave(me, conn),
    }
    if let Ok(out) = frame(
        0,
        &NtcLayerOut {
            capcom_id: Bytes::copy_from_slice(me.as_bytes()),
        },
    ) {
        for other in &others {
            shared.players.send(other, out.clone());
        }
    }
    if let Some(id) = city.filter(|_| leaves_city) {
        // Best effort: the departure itself is done.
        let _ = user_num(shared, world, me, id);
        let _ = gate_counts(shared, world, me, id);
        // An emptied city: its whole record.
        if world.city_host(id).is_none() {
            let _ = gate_record(shared, world, me, id);
        }
    }
    if let Some(id) = city.filter(|_| was_host)
        && let Some(host) = world.city_host(id)
    {
        let name = world
            .players_at(id.0, Some(id.1), Some(id.2))
            .into_iter()
            .find(|(p, _)| *p == host)
            .map(|(_, p)| p.name.clone())
            .unwrap_or_default();
        // An unknown 8 bytes first (MH3SP's zeros).
        let ntc = NtcLayerHost {
            layer: Bytes::from_static(&[0; 8]),
            capcom_id: Bytes::copy_from_slice(host.as_bytes()),
            name,
        };
        if let Ok(ntc) = frame(0, &ntc) {
            for other in &others {
                shared.players.send(other, ntc.clone());
            }
        }
    }
    #[cfg(feature = "bot")]
    if let Some(id) = city.filter(|_| leaves_city) {
        bot::left_behind(shared, world, conn, id);
    }
}

/// A city's counts changed: the players standing in its gate (not in its cities) get them (`NtcLayerUserNum`, kind 4:
/// a child of the console's layer), as MH3SP, so the gate's city list stays live.
fn gate_counts(
    shared: &Shared,
    world: &World,
    me: &str,
    (server, gate, city): (u32, u16, u16),
) -> Result<(), Fault> {
    let Some((record, _)) = world::cities(world, server, gate, u32::from(city), 1).pop() else {
        return Ok(());
    };
    let count = |field| record.get(field).cloned().unwrap_or(Item::Long(0));
    let path = world::path(server, Some(gate), Some(city));
    let ntc = frame(
        0,
        &NtcLayerUserNum {
            kind: 4,
            layer: PatData::new()
                .with(0x01, Item::Binary(Bytes::copy_from_slice(&path)))
                .with(0x02, count(0x06))
                .with(0x03, Item::Long(u32::from(city)))
                // Unknown; MH3SP's values.
                .with(0x04, Item::Long(0xaf00_fa00))
                .with(0x05, count(0x09))
                .with(0x06, Item::Long(0xbf00_fb00))
                .with(0x07, count(0x0b)),
        },
    )?;
    to_gate(shared, world, me, (server, gate), ntc);
    Ok(())
}

/// A city was reserved, created or emptied: the players standing in its gate get its whole record
/// (`NtcLayerInfoSet`), as MH3SP.
fn gate_record(
    shared: &Shared,
    world: &World,
    me: &str,
    (server, gate, city): (u32, u16, u16),
) -> Result<(), Fault> {
    let Some((record, settings)) = world::cities(world, server, gate, u32::from(city), 1).pop()
    else {
        return Ok(());
    };
    let path = Bytes::copy_from_slice(&world::path(server, Some(gate), Some(city)));
    // The city's own path even when nobody holds it (the list names it only for an occupied city).
    let record = match record.get(0x02) {
        Some(_) => record,
        None => record.with(0x02, Item::Binary(path.clone())),
    };
    let ntc = frame(
        0,
        &NtcLayerInfoSet {
            path,
            record,
            settings,
        },
    )?;
    to_gate(shared, world, me, (server, gate), ntc);
    Ok(())
}

fn to_gate(shared: &Shared, world: &World, me: &str, (server, gate): (u32, u16), ntc: Frame) {
    for (other, _) in world.players_at(server, Some(gate), None) {
        if other != me {
            shared.players.send(other, ntc.clone());
        }
    }
}

/// The city's population changed: the others in it get its counts (`NtcLayerUserNum`, kind 2: the console's own
/// layer), which Local Info shows.
fn user_num(
    shared: &Shared,
    world: &World,
    me: &str,
    (server, gate, city): (u32, u16, u16),
) -> Result<(), Fault> {
    let Some((record, _)) = world::cities(world, server, gate, u32::from(city), 1).pop() else {
        return Ok(());
    };
    let count = |field| record.get(field).cloned().unwrap_or(Item::Long(0));
    let ntc = frame(
        0,
        &NtcLayerUserNum {
            kind: 2,
            layer: PatData::new()
                .with(0x02, count(0x06))
                .with(0x03, count(0x07))
                .with(0x05, count(0x09))
                .with(0x07, count(0x0b)),
        },
    )?;
    for other in world.players_at(server, Some(gate), Some(city)) {
        if other.0 != me {
            shared.players.send(other.0, ntc.clone());
        }
    }
    Ok(())
}

/// A seat taken (`NtcLayerMediationLock`): its holder, the slot, taken (field 3: 1).
fn seat_notice(holder: &str, slot: u8) -> NtcLayerMediationLock {
    NtcLayerMediationLock {
        capcom_id: Bytes::copy_from_slice(holder.as_bytes()),
        slot,
        items: PatData::new().with(0x03, Item::Byte(1)),
    }
}

/// A seat freed (`NtcLayerMediationUnlock`).
fn seat_free_notice(holder: &str, slot: u8) -> NtcLayerMediationUnlock {
    NtcLayerMediationUnlock {
        capcom_id: Bytes::copy_from_slice(holder.as_bytes()),
        slot,
        items: PatData::new().with(0x03, Item::Byte(0)),
    }
}

/// A player arriving (`NtcLayerIn`): Capcom ID, then name and profile block.
fn layer_in(world: &World, me: &str, seq: u16) -> Result<Frame, Fault> {
    let place = world
        .neighbours(me)
        .into_iter()
        .find(|(id, _)| *id == me)
        .map(|(_, p)| p);
    let place = place.ok_or_else(|| Fault::Server(anyhow!("arriving player not placed")))?;
    let user = PatData::new()
        .with(0x01, Item::String(Bytes::copy_from_slice(me.as_bytes())))
        .with(0x02, Item::String(place.name.clone()))
        .with(0x07, Item::Binary(place.profile.bytes()));
    frame(
        seq,
        &NtcLayerIn {
            capcom_id: Bytes::copy_from_slice(me.as_bytes()),
            user,
        },
    )
}

/// Two numbers the game uses to tell players apart in relayed town data: stable per Capcom ID.
fn search_ids(capcom_id: &str) -> (u32, u32) {
    let hash = openssl::sha::sha256(capcom_id.as_bytes());
    let mut hash = &hash[..];
    (hash.get_u32(), hash.get_u32())
}

/// Players found by a search, as the lists show them: Capcom ID, name, where they are (with `stats`, Player Search's
/// rows: the hunter's stats block first, as MH3SP's `UserSearchInfo`); then weapon type and whether on a quest (0 in
/// town, 1 on a quest), and HR.
fn found_users(shared: &Shared, found: &[String], stats: bool) -> Vec<(PatData, OptionalFields)> {
    let mut world = shared.world();
    found
        .iter()
        .filter_map(|id| {
            let on_quest = world.on_quest(id);
            let place = world.place(id)?;
            let path = Item::Binary(Bytes::copy_from_slice(&world::path(
                place.server,
                place.gate,
                place.city,
            )));
            let record = PatData::new()
                .with(0x01, Item::String(Bytes::copy_from_slice(id.as_bytes())))
                .with(0x02, Item::String(place.name.clone()));
            let record = match stats {
                true => record
                    .with(0x03, Item::Binary(place.profile.bytes()))
                    .with(0x04, path),
                false => record.with(0x03, path),
            };
            let in_quest = u32::from(on_quest);
            let weapon = u32::from(place.profile.weapon);
            let hr = u32::from(place.profile.hr);
            Some((
                record,
                OptionalFields(vec![
                    (1, Some(weapon << 24 | in_quest)),
                    (2, Some(hr << 16)),
                ]),
            ))
        })
        .collect()
}

/// Who sent relayed game data: an unknown long (MH3SP's 0x12345678, only to the whole layer), Capcom ID, name.
fn sender(me: &str, name: &Bytes, with_long: bool) -> PatData {
    let data = match with_long {
        true => PatData::new().with(0x01, Item::Long(0x1234_5678)),
        false => PatData::new(),
    };
    data.with(0x02, Item::String(Bytes::copy_from_slice(me.as_bytes())))
        .with(0x03, Item::String(name.clone()))
}

/// Relayed game data as a console sends it starts with an empty sender record, which the server fills in: the rest.
fn without_sender(data: &[u8]) -> Result<&[u8], Fault> {
    let mut reader = Reader::new(data);
    reader.pat_data()?;
    Ok(reader.rest())
}

/// Sends a relayed packet to the others at the sender's place.
/// `ReqUserBinaryNotice`'s `target` naming one player (the request carries their Capcom ID; other values, 0 and
/// 2–4, are for everyone at the place: 2 is the only one recorded).
const ONE_PLAYER: u8 = 1;

fn relay<P: Packet>(shared: &Shared, me: &str, packet: &P, seq: u16) -> Handled {
    let ntc = frame(seq, packet)?;
    for other in shared.world().others(me) {
        shared.players.send(&other, ntc.clone());
    }
    Ok(vec![])
}

/// Tells everyone in the city (the requester included) how circle `id` looks now.
fn list_change(
    shared: &Shared,
    world: &World,
    me: &str,
    id: CircleId,
    seq: u16,
    out: &mut Vec<Frame>,
) -> Result<(), Fault> {
    let (info, extra) = world.circle_info(id);
    let ntc = frame(
        seq,
        &NtcCircleListLayerChange {
            circle: id.1,
            info,
            extra,
        },
    )?;
    notify(shared, me, &world.city_players(id.0), &ntc, out);
    Ok(())
}

fn not_in_city() -> Fault {
    Fault::Client(anyhow!("not in a city"))
}

fn not_in_circle() -> Fault {
    Fault::Client(anyhow!("not in a quest"))
}

/// Sets `field` in place, or appends it.
fn set(mut data: PatData, field: u8, item: Item) -> PatData {
    match data.0.iter_mut().find(|(f, _)| *f == field) {
        Some(entry) => entry.1 = item,
        None => data.0.push((field, item)),
    }
    data
}

/// Logged in on FMP means a Capcom ID (checked at login).
fn capcom_id(session: &Session) -> &str {
    session.capcom_id.as_deref().unwrap_or_default()
}

/// Where the player is: world server, gate, city. Once found, the place stays for the rest of the request (the `expect`s
/// on it under the world lock): only this connection's own close removes it, and a newer login replaces it.
fn location(shared: &Shared, session: &Session) -> Result<(u32, Option<u16>, Option<u16>), Fault> {
    let mut world = shared.world();
    let place = world
        .place(capcom_id(session))
        .ok_or_else(|| Fault::Client(anyhow!("not in a world server")))?;
    Ok((place.server, place.gate, place.city))
}

/// Content binary `kind`, built now.
fn binary(shared: &Shared, kind: u8) -> Result<Binary, Fault> {
    let ctx = content::Context {
        dir: &shared.config.content_dir,
        started: shared.started,
        now: shared.now(),
        language: Language::default(),
    };
    content::binary(kind, &ctx)
        .map_err(Fault::Server)?
        .ok_or_else(|| Fault::Client(anyhow!("content type {kind:#x} unknown")))
}

/// Creating and joining quests (async: a quest password is hashed and checked with argon2, off the async threads).
async fn quests(shared: &Shared, session: &Session, req: &Frame) -> Handled {
    let (seq, p) = (req.seq, &req.payload);
    let me = capcom_id(session);
    match req.id {
        ReqCircleCreate::ID => {
            let r = ReqCircleCreate::decode(p)?;
            let string = |field| match r.info.get(field) {
                Some(Item::String(s)) => Some(s.clone()),
                _ => None,
            };
            let password_hash = match string(0x04).filter(|p| !p.is_empty()) {
                Some(password) => Some(hash_password(password).await?),
                None => None,
            };
            let new = NewCircle {
                password_hash,
                remarks: string(0x06),
                unk_0e: match r.info.get(0x0e) {
                    Some(Item::Byte(b)) => *b,
                    _ => 0,
                },
                capacity: match r.info.get(0x09) {
                    Some(Item::Long(c)) => *c,
                    _ => world::CIRCLE_CAPACITY,
                },
                quest_id: r
                    .extra
                    .0
                    .iter()
                    .find(|(f, _)| *f == 0x02)
                    .and_then(|(_, v)| *v)
                    .unwrap_or(0),
            };
            let mut world = shared.world();
            if world.circle_of(me).is_some() {
                return Err(Fault::Client(anyhow!("a second quest while in one")));
            }
            let Some(id) = world.create_circle(me, new) else {
                return alert(AnsCircleCreate::ID, seq, Text::NoQuestSlot);
            };
            info!(circle = id.1, "created quest");
            let mut out = Vec::new();
            list_change(shared, &world, me, id, seq, &mut out)?;
            out.push(frame(seq, &AnsCircleCreate { circle: id.1 })?);
            Ok(out)
        }
        ReqCircleJoin::ID => {
            let r = ReqCircleJoin::decode(p)?;
            let city = shared.world().city_of(me).ok_or_else(not_in_city)?;
            let id = (city, r.circle);
            let hash = shared.world().circle_password(id);
            if let Some(hash) = hash {
                let typed = match r.info.get(0x04) {
                    Some(Item::String(s)) => s.clone(),
                    _ => Bytes::new(),
                };
                if !check_password(typed, hash).await? {
                    return alert(AnsCircleJoin::ID, seq, Text::WrongPassword);
                }
            }
            let mut world = shared.world();
            let index = match world.join_circle(me, id) {
                Ok(index) => index,
                Err(refusal) => return alert(AnsCircleJoin::ID, seq, refusal.text()),
            };
            info!(circle = id.1, "joined quest");
            // The others in the party learn of it (state 0: MH3SP's; 2 would count something on the console).
            let ntc = NtcCircleJoin {
                circle: id.1,
                capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                name: session.name.clone(),
                player: index,
                // 2 would count the player in a tally the console keeps; leaving must then send 2 too (unk.md).
                state: 0,
            };
            let ntc = frame(seq, &ntc)?;
            for (_, other) in world
                .circle(id)
                .map(|c| {
                    c.members()
                        .filter(|(_, m)| *m != me)
                        .map(|(i, m)| (i, m.to_owned()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            {
                shared.players.send(&other, ntc.clone());
            }
            reply(
                seq,
                &AnsCircleJoin {
                    circle: id.1,
                    player: index,
                },
            )
        }
        id => Err(unexpected(id)),
    }
}

async fn hash_password(password: Bytes) -> Result<String, Fault> {
    tokio::task::spawn_blocking(move || {
        use argon2::password_hash::PasswordHasher;
        argon2::Argon2::default()
            .hash_password(&password)
            .map(|h| h.to_string())
    })
    .await
    .map_err(|e| Fault::Server(e.into()))?
    .map_err(|e| Fault::Server(anyhow!("argon2: {e}")))
}

async fn check_password(typed: Bytes, hash: String) -> Result<bool, Fault> {
    tokio::task::spawn_blocking(move || {
        use argon2::password_hash::PasswordVerifier;
        argon2::Argon2::default()
            .verify_password(&typed, hash.as_str())
            .is_ok()
    })
    .await
    .map_err(|e| Fault::Server(e.into()))
}

/// `leaver` leaves its quest, if in one (on `requester`'s request: its own copies go in `out`, in order; others'
/// to their mailboxes): the city sees the quest change; the other members learn who left (`NtcCircleLeave`), and the
/// new leader of a departed quest (`NtcCircleHost`) or that the party broke (`NtcCircleBreak`).
fn quit_quest(
    shared: &Shared,
    world: &mut World,
    leaver: &str,
    requester: &str,
    seq: u16,
    out: &mut Vec<Frame>,
) -> Result<(), Fault> {
    let Some(leaving) = world.leave_circle(leaver) else {
        return Ok(());
    };
    info!(circle = leaving.id.1, player = leaver, "left quest");
    let left = frame(
        seq,
        &NtcCircleLeave {
            circle: leaving.id.1,
            capcom_id: Bytes::copy_from_slice(leaver.as_bytes()),
            player: leaving.index,
            // The same as its `NtcCircleJoin.state`: 2 counts the player in a tally the console keeps (unk.md).
            state: 0,
        },
    )?;
    notify(shared, requester, &leaving.others, &left, out);
    if let Some((index, leader)) = &leaving.new_leader {
        let name = world
            .players_under(leaving.id.0.0, Some(leaving.id.0.1), Some(leaving.id.0.2))
            .into_iter()
            .find(|(id, _)| id == leader)
            .map(|(_, p)| p.name.clone())
            .unwrap_or_default();
        let host = frame(
            seq,
            &NtcCircleHost {
                circle: 0,
                leader: *index,
                capcom_id: leader.clone().into(),
                name,
            },
        )?;
        notify(shared, requester, &leaving.others, &host, out);
    }
    if leaving.broken {
        let broken = frame(seq, &NtcCircleBreak { circle: 0 })?;
        notify(shared, requester, &leaving.others, &broken, out);
    }
    let changed = list_change(shared, world, requester, leaving.id, seq, out);
    #[cfg(feature = "bot")]
    bot::partyless(shared, world, requester, leaving.id.0);
    changed
}

/// A message's sender info: the sender's Capcom ID and name, and an unknown 20; in the order consoles send (and
/// MH3SP relays) each kind: a plain message `01` (0), `02`, `03`, `04`; a friend request `03`, `04`, `02`.
fn message_info(friend_request: bool, from: &str, name: Bytes) -> PatData {
    let from = Item::String(Bytes::copy_from_slice(from.as_bytes()));
    match friend_request {
        true => PatData::new()
            .with(0x03, from)
            .with(0x04, Item::String(name))
            .with(0x02, Item::Long(20)),
        false => PatData::new()
            .with(0x01, Item::Long(0))
            .with(0x02, Item::Long(20))
            .with(0x03, from)
            .with(0x04, Item::String(name)),
    }
}

/// A list of hunters as the friend and black lists show them: index, Capcom ID, name; `count` from `first`.
fn hunter_records(list: Vec<(String, Vec<u8>)>, first: u32, count: u32) -> Vec<PatData> {
    list.into_iter()
        .zip(1u32..)
        .skip(first.saturating_sub(1) as usize)
        .take(count as usize)
        .map(|((capcom_id, name), index)| {
            PatData::new()
                .with(0x01, Item::Long(index))
                .with(0x02, Item::String(capcom_id.into()))
                .with(0x03, Item::String(name.into()))
        })
        .collect()
}

/// The console writes a window of its profile block: validated (an invalid write is refused, the previous profile
/// kept), shown to others from now on, and stored.
async fn profile_set(shared: &Shared, session: &mut Session, req: &Frame) -> Handled {
    let r = ReqUserBinarySet::decode(&req.payload)?;
    match session.profile.write(r.offset as usize, &r.data) {
        Ok(profile) => {
            let me = capcom_id(session);
            if let Some(place) = shared.world().place(me) {
                place.profile = profile.clone();
            }
            let db = shared
                .db
                .pool()
                .ok_or_else(|| Fault::Database(anyhow!("database not opened")))?;
            let database = |e: anyhow::Error| Fault::Database(e);
            let (id, _) = accounts::hunter(db, me)
                .await
                .map_err(database)?
                .ok_or_else(|| Fault::Server(anyhow!("logged-in hunter not in the database")))?;
            accounts::save_profile(db, id, &profile)
                .await
                .map_err(database)?;
            session.profile = profile;
        }
        Err(e) => warn!("profile write refused: {e:#}"),
    }
    reply(req.seq, &AnsUserBinarySet {})
}

/// After a request handled: the messages waiting for the player, at their position in a city. The game's inbox
/// drops a message arriving while a scene loads (seen in the lab: sent at the world server or on entering a gate,
/// they never showed; to a player standing in a city, they do): a position means the town is loaded and the hunter
/// on screen.
async fn deliver_mail(
    shared: &Shared,
    mailbox: &Mailbox,
    session: &Session,
    req: &Frame,
    handled: Handled,
) -> Handled {
    let mut out = handled?;
    if req.id != NtcLayerUserPosition::ID {
        return Ok(out);
    }
    let me = capcom_id(session);
    let in_city = match shared.world().place(me) {
        Some(place) if place.city.is_some() => {
            place.standing = true;
            true
        }
        _ => false,
    };
    if !in_city || !mailbox.take_mail() {
        return Ok(out);
    }
    match waiting_mail(shared, me).await {
        Ok(mail) => out.extend(mail),
        // The position still goes to the others (architecture decision 9); the mail waits for the next one.
        Err(Fault::Database(e)) => {
            error!(error = %format!("{e:#}"), "database failed, mail not delivered");
            shared.players.mail(me);
        }
        Err(fault) => return Err(fault),
    }
    Ok(out)
}

/// The messages waiting for `me`, now delivered.
async fn waiting_mail(shared: &Shared, me: &str) -> Result<Vec<Frame>, Fault> {
    let db = shared
        .db
        .pool()
        .ok_or_else(|| Fault::Database(anyhow!("database not opened")))?;
    let (my_id, _) = accounts::hunter(db, me)
        .await
        .map_err(Fault::Database)?
        .ok_or_else(|| Fault::Server(anyhow!("logged-in hunter not in the database")))?;
    inbox(db, me, my_id, 0).await
}

/// The messages waiting for `me` (`id` in the database), as notices, now delivered.
async fn inbox(db: &SqlitePool, me: &str, id: i64, seq: u16) -> Result<Vec<Frame>, Fault> {
    let messages = accounts::inbox(db, id).await.map_err(Fault::Database)?;
    let mut out = Vec::new();
    for (from, name, text, friend_request) in messages {
        let capcom_id = Bytes::copy_from_slice(me.as_bytes());
        let info = message_info(friend_request, &from, name.into());
        let text = text.into();
        out.push(match friend_request {
            true => frame(
                seq,
                &NtcFriendAccept {
                    capcom_id,
                    info,
                    text,
                },
            )?,
            false => frame(
                seq,
                &NtcTell {
                    capcom_id,
                    info,
                    text,
                },
            )?,
        });
    }
    Ok(out)
}

/// Stores a message (or a friend request) for `to`, unless they block the sender: delivered at once if they stand
/// in a city, else when they next do.
async fn post(
    shared: &Shared,
    db: &SqlitePool,
    from: i64,
    (to_id, to): (i64, &str),
    text: &[u8],
    friend_request: bool,
    seq: u16,
) -> Result<(), Fault> {
    let database = |e: anyhow::Error| Fault::Database(e);
    if to_id == from || accounts::blocks(db, to_id, from).await.map_err(database)? {
        return Ok(());
    }
    if !accounts::store_message(db, from, to_id, text, friend_request)
        .await
        .map_err(database)?
    {
        warn!("recipient's inbox full, message dropped");
        return Ok(());
    }
    let standing = shared.world().place(to).is_some_and(|p| p.standing);
    if !standing {
        shared.players.mail(to);
        return Ok(());
    }
    match inbox(db, to, to_id, seq).await {
        Ok(notices) => {
            for notice in notices {
                shared.players.send(to, notice);
            }
        }
        // Stored all the same: the sender isn't told otherwise, the recipient gets it at a later position.
        Err(Fault::Database(e)) => {
            error!(error = %format!("{e:#}"), "database failed, mail not delivered");
            shared.players.mail(to);
        }
        Err(fault) => return Err(fault),
    }
    Ok(())
}

/// Two hunters became friends: both are told (`me` in `out`, the other if online).
fn befriend(
    shared: &Shared,
    (me, my_name): (&str, &Bytes),
    (other, other_name): (&str, Bytes),
    seq: u16,
    out: &mut Vec<Frame>,
) -> Result<(), Fault> {
    info!("new friends");
    let card = |id: &str, name: Bytes| {
        PatData::new()
            .with(0x01, Item::Long(1))
            .with(0x02, Item::String(Bytes::copy_from_slice(id.as_bytes())))
            .with(0x03, Item::String(name))
    };
    let to_them = NtcFriendAdd {
        capcom_id: Bytes::copy_from_slice(other.as_bytes()),
        friend: card(me, my_name.clone()),
        accepted: 1,
    };
    shared.players.send(other, frame(seq, &to_them)?);
    let to_me = NtcFriendAdd {
        capcom_id: Bytes::copy_from_slice(me.as_bytes()),
        friend: card(other, other_name),
        accepted: 1,
    };
    out.push(frame(seq, &to_me)?);
    Ok(())
}

/// Messages, friends and the black list. A blocked hunter's tells and friend requests are answered as sent but
/// never reach the blocker.
async fn social(shared: &Shared, session: &Session, req: &Frame) -> Handled {
    let (seq, p) = (req.seq, &req.payload);
    let db = shared
        .db
        .pool()
        .ok_or_else(|| Fault::Database(anyhow!("database not opened")))?;
    let database = |e: anyhow::Error| Fault::Database(e);
    let me = capcom_id(session);
    let (my_id, _) = accounts::hunter(db, me)
        .await
        .map_err(database)?
        .ok_or_else(|| Fault::Server(anyhow!("logged-in hunter not in the database")))?;
    // Who a request names: `None` for an unknown Capcom ID.
    let named = |capcom_id: &Bytes| String::from_utf8_lossy(capcom_id).into_owned();
    match req.id {
        ReqTell::ID => {
            // A message for the recipient's inbox: stored, delivered at their next position in a city.
            let r = ReqTell::decode(p)?;
            let to = named(&r.capcom_id);
            if let Some((to_id, _)) = accounts::hunter(db, &to).await.map_err(database)? {
                post(shared, db, my_id, (to_id, &to), &r.text, false, seq).await?;
            }
            reply(seq, &AnsTell {})
        }
        ReqFriendList::ID => {
            // Asked when entering a world server.
            let r = ReqFriendList::decode(p)?;
            let list = accounts::friends(db, my_id).await.map_err(database)?;
            reply(
                seq,
                &AnsFriendList {
                    first: 0,
                    friends: hunter_records(list, r.first, r.count),
                },
            )
        }
        ReqFriendAdd::ID => {
            // A request (with its message), stored and delivered like a message; when the other asked too, the two
            // become friends at once.
            let r = ReqFriendAdd::decode(p)?;
            let to = named(&r.capcom_id);
            let Some((to_id, to_name)) = accounts::hunter(db, &to).await.map_err(database)? else {
                return alert(AnsFriendAdd::ID, seq, Text::UnknownCapcomId);
            };
            let mut out = vec![frame(seq, &AnsFriendAdd {})?];
            if accounts::answer_friend_request(db, to_id, my_id, true)
                .await
                .map_err(database)?
            {
                befriend(
                    shared,
                    (me, &session.name),
                    (&to, to_name.into()),
                    seq,
                    &mut out,
                )?;
            } else {
                post(shared, db, my_id, (to_id, &to), &r.text, true, seq).await?;
            }
            Ok(out)
        }
        ReqFriendAccept::ID => {
            // The answer to a request: only one actually sent counts.
            let r = ReqFriendAccept::decode(p)?;
            let from = named(&r.capcom_id);
            let mut out = vec![frame(seq, &AnsFriendAccept {})?];
            let Some((from_id, from_name)) = accounts::hunter(db, &from).await.map_err(database)?
            else {
                return Ok(out);
            };
            let accept = r.accepted == 1;
            if accounts::answer_friend_request(db, from_id, my_id, accept)
                .await
                .map_err(database)?
                && accept
            {
                befriend(
                    shared,
                    (me, &session.name),
                    (&from, from_name.into()),
                    seq,
                    &mut out,
                )?;
            }
            Ok(out)
        }
        ReqFriendDelete::ID => {
            let r = ReqFriendDelete::decode(p)?;
            if let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(database)?
            {
                accounts::unfriend(db, my_id, id).await.map_err(database)?;
            }
            reply(seq, &AnsFriendDelete {})
        }
        ReqBlackList::ID => {
            let r = ReqBlackList::decode(p)?;
            let list = accounts::blacklist(db, my_id).await.map_err(database)?;
            reply(
                seq,
                &AnsBlackList {
                    first: 0,
                    users: hunter_records(list, r.first, r.count),
                },
            )
        }
        ReqBlackAdd::ID => {
            let r = ReqBlackAdd::decode(p)?;
            let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(database)?
            else {
                return alert(AnsBlackAdd::ID, seq, Text::UnknownCapcomId);
            };
            if id != my_id {
                accounts::block(db, my_id, id).await.map_err(database)?;
            }
            reply(seq, &AnsBlackAdd {})
        }
        ReqBlackDelete::ID => {
            let r = ReqBlackDelete::decode(p)?;
            if let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(database)?
            {
                accounts::unblock(db, my_id, id).await.map_err(database)?;
            }
            reply(seq, &AnsBlackDelete {})
        }
        id => Err(unexpected(id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_allows_its_burst_then_its_rate() {
        let mut bucket = Bucket::new((3.0, 1.0));
        assert!((0..3).all(|_| bucket.take()));
        assert!(!bucket.take());
        // A second later, one more.
        bucket.last -= std::time::Duration::from_secs(1);
        assert!(bucket.take());
        assert!(!bucket.take());
        // Never more than the burst, however long it waited.
        bucket.last -= std::time::Duration::from_secs(60);
        assert!((0..3).all(|_| bucket.take()));
        assert!(!bucket.take());
    }
}
