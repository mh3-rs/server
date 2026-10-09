//! FMP (port 8202): the world servers. The console comes twice with the ticket of its last LMP visit: once to pick a
//! world server, then to play on it (gates, cities, quests).

use std::collections::HashMap;

use anyhow::anyhow;
use bytes::{BufMut, Bytes};
use tracing::{Span, info, trace};
use wire::packets::*;
use wire::{Frame, Item, PatData, PatWrite, Reader};

use super::players::Mailbox;
use super::world::{CircleId, NewCircle, World};
use super::{
    Fault, Handled, PROFILE_SIZE, Session, alert, frame, reply, server_time, unexpected, world,
};
use crate::Shared;
use crate::content::{self, Binary};
use crate::text::{Language, Text};

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
                // Back from the server selection with a pick: the player enters that world server.
                if let Some(server) = session.world_server {
                    let mut world = shared.world.lock().unwrap();
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
                shared
                    .world
                    .lock()
                    .unwrap()
                    .leave(capcom_id(session), self.mailbox.conn);
                reply(seq, &AnsLayerEnd {})
            }
            ReqLayerUserList::ID => {
                ReqLayerUserList::decode(p)?;
                let world = shared.world.lock().unwrap();
                let users = world
                    .neighbours(capcom_id(session))
                    .into_iter()
                    .map(|(id, place)| {
                        PatData::new()
                            .with(0x01, Item::String(Bytes::copy_from_slice(id.as_bytes())))
                            .with(0x02, Item::String(place.name.clone()))
                            .with(
                                0x07,
                                Item::Binary(Bytes::copy_from_slice(&place.profile[..])),
                            )
                    })
                    .collect();
                reply(seq, &AnsLayerUserList { users })
            }
            ReqFriendList::ID => {
                // Friends come with M3.
                ReqFriendList::decode(p)?;
                reply(
                    seq,
                    &AnsFriendList {
                        unk: 0,
                        friends: vec![],
                    },
                )
            }
            ReqBlackList::ID => {
                // The black list comes with M3. MH3SP answers one blank entry rather than none: kept, the console
                // is known to accept it.
                ReqBlackList::decode(p)?;
                let blank = PatData::new()
                    .with(0x01, Item::Long(0))
                    .with(0x02, Item::String(Bytes::new()))
                    .with(0x03, Item::String(Bytes::new()));
                reply(
                    seq,
                    &AnsBlackList {
                        unk: 0,
                        users: vec![blank],
                    },
                )
            }
            ReqUserStatusSet::ID => {
                // What the player is doing (searchable, M3).
                ReqUserStatusSet::decode(p)?;
                reply(seq, &AnsUserStatusSet {})
            }
            ReqUserBinaryNotice::ID => {
                // Tells the layer the profile changed (to the others: M3).
                ReqUserBinaryNotice::decode(p)?;
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
                        unk: 0,
                        count: u32::from(count),
                    },
                )
            }
            ReqLayerChildListData::ID => {
                let r = ReqLayerChildListData::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                let world = shared.world.lock().unwrap();
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
            ReqLayerChildInfo::ID => {
                // MH3SP answers its placeholder layer whatever is asked; the console accepts it.
                ReqLayerChildInfo::decode(p)?;
                reply(
                    seq,
                    &AnsLayerChildInfo {
                        unk: 1,
                        layer: placeholder_layer(),
                        unk2: 0,
                    },
                )
            }
            ReqLayerDown::ID => {
                let r = ReqLayerDown::decode(p)?;
                let me = capcom_id(session);
                let (server, gate, city) = location(shared, session)?;
                let mut world = shared.world.lock().unwrap();
                match (gate, city) {
                    (None, _) if (1..=world::GATES).contains(&r.layer) => {
                        world.place(me).expect("located").gate = Some(r.layer);
                        info!(gate = r.layer, "entered gate");
                    }
                    (Some(gate), None) => {
                        if let Err(refusal) = world.join_city((server, gate, r.layer), me) {
                            return alert(AnsLayerDown::ID, seq, refusal.text());
                        }
                        info!(gate, city = r.layer, "entered city");
                    }
                    _ => return Err(Fault::Client(anyhow!("no layer {} below", r.layer))),
                }
                reply(seq, &AnsLayerDown { layer: r.layer })
            }
            ReqLayerUp::ID => {
                ReqLayerUp::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world.lock().unwrap();
                if let Some(place) = world.place(me) {
                    if place.city.is_some() {
                        world.leave_city(me);
                    } else {
                        place.gate = None;
                    }
                }
                reply(seq, &AnsLayerUp {})
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
                if let Err(refusal) = shared
                    .world
                    .lock()
                    .unwrap()
                    .reserve_city((server, gate, r.number), capcom_id(session))
                {
                    return alert(AnsLayerCreateHead::ID, seq, refusal.text());
                }
                reply(seq, &AnsLayerCreateHead { number: r.number })
            }
            ReqLayerCreateSet::ID => {
                let r = ReqLayerCreateSet::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                let gate =
                    gate.ok_or_else(|| Fault::Client(anyhow!("city created outside a gate")))?;
                if let Err(refusal) = shared.world.lock().unwrap().create_city(
                    (server, gate, r.number),
                    capcom_id(session),
                    r.extra,
                ) {
                    return alert(AnsLayerCreateSet::ID, seq, refusal.text());
                }
                info!(gate, city = r.number, "created city");
                reply(seq, &AnsLayerCreateSet { number: r.number })
            }
            ReqLayerCreateFoot::ID => {
                let r = ReqLayerCreateFoot::decode(p)?;
                let (server, gate, _) = location(shared, session)?;
                if let Some(gate) = gate {
                    shared
                        .world
                        .lock()
                        .unwrap()
                        .release_city((server, gate, r.number), capcom_id(session));
                }
                reply(seq, &AnsLayerCreateFoot { number: r.number })
            }
            ReqLayerMediationList::ID => {
                // MH3SP's placeholder list (its meaning is unknown): 14 unlocked entries named "Test".
                ReqLayerMediationList::decode(p)?;
                let items = (1..=14)
                    .map(|i| {
                        PatData::new()
                            .with(0x01, Item::String(Bytes::from_static(b"Test")))
                            .with(0x02, Item::Byte(i))
                            .with(0x03, Item::Byte(0))
                    })
                    .collect();
                reply(
                    seq,
                    &AnsLayerMediationList {
                        unk: 0,
                        items: ShortList(items),
                    },
                )
            }
            ReqCircleListLayer::ID => {
                ReqCircleListLayer::decode(p)?;
                let world = shared.world.lock().unwrap();
                let city = world.city_of(capcom_id(session)).ok_or_else(not_in_city)?;
                reply(
                    seq,
                    &AnsCircleListLayer {
                        unk: 0,
                        circles: world.circles_in(city),
                    },
                )
            }
            ReqCircleCreate::ID => {
                let r = ReqCircleCreate::decode(p)?;
                let string = |field| match r.info.get(field) {
                    Some(Item::String(s)) => Some(s.clone()),
                    _ => None,
                };
                let new = NewCircle {
                    password: string(0x04),
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
                let me = capcom_id(session);
                let mut world = shared.world.lock().unwrap();
                let Some(id) = world.create_circle(me, new) else {
                    return alert(AnsCircleCreate::ID, seq, Text::NoQuestSlot);
                };
                info!(circle = id.1, "created quest");
                let mut out = Vec::new();
                list_change(shared, &world, me, id, seq, &mut out)?;
                out.push(frame(seq, &AnsCircleCreate { circle: id.1 })?);
                Ok(out)
            }
            ReqCircleMatchOptionSet::ID => {
                let r = ReqCircleMatchOptionSet::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world.lock().unwrap();
                let id = world.circle_of(me).ok_or_else(not_in_circle)?;
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
                let world = shared.world.lock().unwrap();
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
                let world = shared.world.lock().unwrap();
                let city = world.city_of(me).ok_or_else(not_in_city)?;
                let id = (city, r.circle);
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
                let mut world = shared.world.lock().unwrap();
                let id = world.circle_of(me).ok_or_else(not_in_circle)?;
                let ready = world.depart(id);
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
                let mut out = Vec::new();
                notify(shared, me, &members, &ntc, &mut out);
                out.push(frame(seq, &AnsCircleMatchStart {})?);
                Ok(out)
            }
            NtcCircleBinary::ID => {
                // Game data for the circle: relayed with the sender's Capcom ID and name in front.
                let r = NtcCircleBinary::decode(p)?;
                let mut reader = Reader::new(&r.data.0);
                reader.pat_data()?;
                let me = capcom_id(session);
                let mut data = Vec::new();
                data.put_lp2_string(me.as_bytes())?;
                data.put_pat_data(
                    &PatData::new()
                        .with(0x01, Item::Long(0x1234_5678))
                        .with(0x02, Item::String(Bytes::copy_from_slice(me.as_bytes())))
                        .with(0x03, Item::String(session.name.clone())),
                )?;
                data.extend_from_slice(reader.rest());
                let world = shared.world.lock().unwrap();
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
                let ntc = frame(
                    seq,
                    &NtcCircleBinary {
                        circle: r.circle,
                        data: Rest(data.into()),
                    },
                )?;
                let mut out = Vec::new();
                notify(shared, me, &others, &ntc, &mut out);
                Ok(out)
            }
            ReqCircleMatchEnd::ID => {
                ReqCircleMatchEnd::decode(p)?;
                reply(seq, &AnsCircleMatchEnd {})
            }
            ReqCircleLeave::ID => {
                let r = ReqCircleLeave::decode(p)?;
                let me = capcom_id(session);
                let mut world = shared.world.lock().unwrap();
                let mut out = Vec::new();
                if let Some(id) = world.leave_circle(me) {
                    info!(circle = id.1, "left quest");
                    list_change(shared, &world, me, id, seq, &mut out)?;
                }
                out.push(frame(seq, &AnsCircleLeave { circle: r.circle })?);
                Ok(out)
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
                if let Some(place) = shared.world.lock().unwrap().place(capcom_id(session)) {
                    place.profile[start..end].copy_from_slice(&r.data);
                }
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
            shared
                .world
                .lock()
                .unwrap()
                .leave(capcom_id, self.mailbox.conn);
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
        .with(0x11, Item::Long(500))
        .with(0x12, Item::Byte(1))
}

/// Sends `ntc` to `to`: the requester's own copy goes in its answer (`out`), in order; the others' to their
/// mailboxes.
fn notify(shared: &Shared, me: &str, to: &[String], ntc: &Frame, out: &mut Vec<Frame>) {
    for player in to {
        match player == me {
            true => out.push(ntc.clone()),
            false => shared.players.send(player, ntc.clone()),
        }
    }
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

/// Where the player is: world server, gate, city.
fn location(shared: &Shared, session: &Session) -> Result<(u32, Option<u16>, Option<u16>), Fault> {
    let mut world = shared.world.lock().unwrap();
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
