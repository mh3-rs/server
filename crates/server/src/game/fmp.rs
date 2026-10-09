//! FMP (port 8202): the world servers. The console comes twice with the ticket of its last LMP visit: once to pick a
//! world server, then to play on it (gates, cities, quests).

use std::collections::HashMap;

use anyhow::anyhow;
use bytes::{BufMut, Bytes};
use tracing::{Span, info, trace, warn};
use wire::packets::*;
use wire::{Frame, Item, OptionalFields, PatData, PatWrite, Reader};

use super::players::Mailbox;
use super::world::{CircleId, NewCircle, World};
use super::{
    Fault, Handled, PROFILE_SIZE, Session, alert, frame, reply, server_time, unexpected, world,
};
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
}

impl Fmp {
    pub fn new(mailbox: Mailbox) -> Self {
        Fmp {
            mailbox,
            login: None,
            downloads: HashMap::new(),
            search: Vec::new(),
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
            (Some(_), _) => match self.social(shared, req).await {
                Some(handled) => handled,
                None => self.request(shared, req),
            },
        }
    }

    /// Messages, friends and the black list (they need the database); `None` for anything else.
    async fn social(&mut self, shared: &Shared, req: &Frame) -> Option<Handled> {
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
                let world = shared.world.lock().unwrap();
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
                        unk: 0,
                        count: self.search.len() as u32,
                    },
                )
            }
            ReqLayerUserSearchData::ID => {
                ReqLayerUserSearchData::decode(p)?;
                let users = found_users(shared, &self.search);
                reply(seq, &AnsLayerUserSearchData { unk: 0, users })
            }
            ReqLayerUserSearchFoot::ID => {
                ReqLayerUserSearchFoot::decode(p)?;
                reply(seq, &AnsLayerUserSearchFoot {})
            }
            ReqUserSearchInfo::ID => {
                // A player's card, if they're in the world.
                let r = ReqUserSearchInfo::decode(p)?;
                let id = String::from_utf8_lossy(&r.capcom_id).into_owned();
                let mut world = shared.world.lock().unwrap();
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
                    .with(
                        0x03,
                        Item::Binary(Bytes::copy_from_slice(&place.profile[..])),
                    )
                    .with(
                        0x04,
                        Item::Binary(Bytes::copy_from_slice(&world::path(
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
                let mut world = shared.world.lock().unwrap();
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
            ReqUserStatusSet::ID => {
                // What the player is doing (searchable, M3).
                ReqUserStatusSet::decode(p)?;
                reply(seq, &AnsUserStatusSet {})
            }
            ReqUserBinaryNotice::ID => {
                // The profile changed: the others at the player's place get the part named.
                let r = ReqUserBinaryNotice::decode(p)?;
                let me = capcom_id(session);
                let start = (r.offset as usize).min(PROFILE_SIZE);
                let end = start.saturating_add(r.size as usize).min(PROFILE_SIZE);
                let ntc = NtcUserBinaryNotice {
                    unk: r.unk,
                    capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                    offset: r.offset,
                    data: Bytes::copy_from_slice(&session.profile[start..end]),
                };
                relay(shared, me, &ntc, seq)?;
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
                        let ntc = layer_in(&world, me, seq)?;
                        for other in world.others(me) {
                            shared.players.send(&other, ntc.clone());
                        }
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
                let mut world = shared.world.lock().unwrap();
                depart(
                    shared,
                    &mut world,
                    capcom_id(session),
                    self.mailbox.conn,
                    Depart::Up,
                );
                reply(seq, &AnsLayerUp {})
            }
            ReqLayerHost::ID => {
                // A console entering a city asks who hosts its P2P session; the others learn of the arrival.
                let r = ReqLayerHost::decode(p)?;
                let me = capcom_id(session);
                let world = shared.world.lock().unwrap();
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
                let world = shared.world.lock().unwrap();
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
                let users = found_users(shared, &self.search);
                reply(seq, &AnsLayerUserListData { unk: 1, users })
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
                let mut data = Vec::new();
                data.put_lp2_string(me.as_bytes())?;
                data.put_pat_data(&sender(me, &session.name, true))?;
                data.extend_from_slice(without_sender(&r.data.0)?);
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
                if !shared.world.lock().unwrap().others(me).contains(&partner) {
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
                quit_quest(shared, &mut world, me, &session.name, seq, &mut out)?;
                out.push(frame(seq, &AnsCircleLeave { circle: r.circle })?);
                Ok(out)
            }
            ReqCircleUserList::ID => {
                // The party: per member, ready or not, index, Capcom ID, name.
                ReqCircleUserList::decode(p)?;
                let world = shared.world.lock().unwrap();
                let me = capcom_id(session);
                let id = world.circle_of(me).ok_or_else(not_in_circle)?;
                let users = world.circle_members(id);
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
                let world = shared.world.lock().unwrap();
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
                let world = shared.world.lock().unwrap();
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
                        unk: r.unk,
                        info,
                        text: r.text,
                    },
                    seq,
                )
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
            let mut world = shared.world.lock().unwrap();
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
        .with(0x11, Item::Long(500))
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
    // Leaving a city (or the game) means leaving its quest first; the requester's own notices are of no use now.
    let mut ignored = Vec::new();
    let in_city = world.city_of(me).is_some();
    let leaves_city =
        matches!(how, Depart::World) || world.place(me).is_some_and(|p| p.city.is_some());
    if in_city && leaves_city {
        let _ = quit_quest(shared, world, me, &Bytes::new(), 0, &mut ignored);
    }
    let others = world.others(me);
    let city = world.city_of(me);
    let was_host = city.is_some_and(|id| world.city_host(id) == Some(me));
    match how {
        Depart::Up => {
            let place = world.place(me).expect("placed");
            if place.city.is_some() {
                world.leave_city(me);
            } else {
                place.gate = None;
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
        .with(
            0x07,
            Item::Binary(Bytes::copy_from_slice(&place.profile[..])),
        );
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
    let number = |i: usize| u32::from_be_bytes([hash[i], hash[i + 1], hash[i + 2], hash[i + 3]]);
    (number(0), number(4))
}

/// Players found by a search, as the lists show them: Capcom ID, name, where they are; then weapon type and whether
/// on a quest (0 in town, 1 on a quest), and HR.
fn found_users(shared: &Shared, found: &[String]) -> Vec<(PatData, OptionalFields)> {
    let mut world = shared.world.lock().unwrap();
    found
        .iter()
        .filter_map(|id| {
            let on_quest = world.on_quest(id);
            let place = world.place(id)?;
            let record = PatData::new()
                .with(0x01, Item::String(Bytes::copy_from_slice(id.as_bytes())))
                .with(0x02, Item::String(place.name.clone()))
                .with(
                    0x03,
                    Item::Binary(Bytes::copy_from_slice(&world::path(
                        place.server,
                        place.gate,
                        place.city,
                    ))),
                );
            let in_quest = u32::from(on_quest);
            let weapon = u32::from(place.profile[5]);
            let hr = u32::from(u16::from_be_bytes([place.profile[0], place.profile[1]]));
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
fn relay<P: Packet>(shared: &Shared, me: &str, packet: &P, seq: u16) -> Handled {
    let ntc = frame(seq, packet)?;
    for other in shared.world.lock().unwrap().others(me) {
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
            let mut world = shared.world.lock().unwrap();
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
            let city = shared
                .world
                .lock()
                .unwrap()
                .city_of(me)
                .ok_or_else(not_in_city)?;
            let id = (city, r.circle);
            let hash = shared.world.lock().unwrap().circle_password(id);
            if let Some(hash) = hash {
                let typed = match r.info.get(0x04) {
                    Some(Item::String(s)) => s.clone(),
                    _ => Bytes::new(),
                };
                if !check_password(typed, hash).await? {
                    return alert(AnsCircleJoin::ID, seq, Text::WrongPassword);
                }
            }
            let mut world = shared.world.lock().unwrap();
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

/// The player leaves its quest, if in one: the city sees the quest change; the other members learn who left
/// (`NtcCircleLeave`), and the new leader of a departed quest (`NtcCircleHost`) or that the party broke
/// (`NtcCircleBreak`).
fn quit_quest(
    shared: &Shared,
    world: &mut World,
    me: &str,
    _name: &Bytes,
    seq: u16,
    out: &mut Vec<Frame>,
) -> Result<(), Fault> {
    let Some(leaving) = world.leave_circle(me) else {
        return Ok(());
    };
    info!(circle = leaving.id.1, "left quest");
    let left = frame(
        seq,
        &NtcCircleLeave {
            circle: leaving.id.1,
            capcom_id: Bytes::copy_from_slice(me.as_bytes()),
            player: leaving.index,
            unk: 0,
        },
    )?;
    for other in &leaving.others {
        shared.players.send(other, left.clone());
    }
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
                unk: 0,
                leader: *index,
                capcom_id: leader.clone().into(),
                name,
            },
        )?;
        for other in &leaving.others {
            shared.players.send(other, host.clone());
        }
    }
    if leaving.broken {
        let broken = frame(seq, &NtcCircleBreak { unk: 0 })?;
        for other in &leaving.others {
            shared.players.send(other, broken.clone());
        }
    }
    list_change(shared, world, me, leaving.id, seq, out)
}

/// A text message's sender info: the console's fields with the sender filled in, and an unknown 20.
fn message_info(info: PatData, me: &str, name: &Bytes) -> PatData {
    let info = set(
        info,
        0x03,
        Item::String(Bytes::copy_from_slice(me.as_bytes())),
    );
    set(
        set(info, 0x04, Item::String(name.clone())),
        0x02,
        Item::Long(20),
    )
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

/// Messages, friends and the black list. A blocked hunter's tells and friend requests are answered as sent but
/// never reach the blocker.
async fn social(shared: &Shared, session: &Session, req: &Frame) -> Handled {
    let (seq, p) = (req.seq, &req.payload);
    let db = shared
        .db
        .pool()
        .ok_or_else(|| Fault::Server(anyhow!("database not opened")))?;
    let server = |e: anyhow::Error| Fault::Server(e);
    let me = capcom_id(session);
    let (my_id, _) = accounts::hunter(db, me)
        .await
        .map_err(server)?
        .ok_or_else(|| Fault::Server(anyhow!("logged-in hunter not in the database")))?;
    // Who a request names: `None` for an unknown Capcom ID.
    let named = |capcom_id: &Bytes| String::from_utf8_lossy(capcom_id).into_owned();
    match req.id {
        ReqTell::ID => {
            let r = ReqTell::decode(p)?;
            let to = named(&r.capcom_id);
            let blocked = match accounts::hunter(db, &to).await.map_err(server)? {
                Some((to_id, _)) => accounts::blocks(db, to_id, my_id).await.map_err(server)?,
                None => false,
            };
            if !blocked {
                let info = message_info(r.info, me, &session.name);
                shared.players.send(
                    &to,
                    frame(
                        seq,
                        &NtcTell {
                            capcom_id: r.capcom_id,
                            info,
                            text: r.text,
                        },
                    )?,
                );
            }
            reply(seq, &AnsTell {})
        }
        ReqFriendList::ID => {
            // Asked when entering a world server: then the friend requests that came while away are delivered.
            let r = ReqFriendList::decode(p)?;
            let list = accounts::friends(db, my_id).await.map_err(server)?;
            let mut out = vec![frame(
                seq,
                &AnsFriendList {
                    unk: 0,
                    friends: hunter_records(list, r.first, r.count),
                },
            )?];
            for (from, name, message) in accounts::undelivered_requests(db, my_id)
                .await
                .map_err(server)?
            {
                let info = message_info(PatData::new(), &from, &Bytes::from(name));
                let ntc = NtcFriendAccept {
                    capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                    info,
                    text: message.into(),
                };
                out.push(frame(seq, &ntc)?);
            }
            Ok(out)
        }
        ReqFriendAdd::ID => {
            let r = ReqFriendAdd::decode(p)?;
            let to = named(&r.capcom_id);
            let Some((to_id, _)) = accounts::hunter(db, &to).await.map_err(server)? else {
                return alert(AnsFriendAdd::ID, seq, Text::UnknownCapcomId);
            };
            if to_id != my_id && !accounts::blocks(db, to_id, my_id).await.map_err(server)? {
                accounts::request_friend(db, my_id, to_id, &r.text)
                    .await
                    .map_err(server)?;
                let info = message_info(r.info, me, &session.name);
                let ntc = frame(
                    seq,
                    &NtcFriendAccept {
                        capcom_id: r.capcom_id,
                        info,
                        text: r.text,
                    },
                )?;
                if shared.players.send(&to, ntc) {
                    accounts::delivered(db, my_id, to_id)
                        .await
                        .map_err(server)?;
                }
            }
            reply(seq, &AnsFriendAdd {})
        }
        ReqFriendAccept::ID => {
            // The answer to a request: only one actually sent counts.
            let r = ReqFriendAccept::decode(p)?;
            let from = named(&r.capcom_id);
            let mut out = vec![frame(seq, &AnsFriendAccept {})?];
            let Some((from_id, from_name)) = accounts::hunter(db, &from).await.map_err(server)?
            else {
                return Ok(out);
            };
            let accept = r.accepted == 1;
            if accounts::answer_friend_request(db, from_id, my_id, accept)
                .await
                .map_err(server)?
                && accept
            {
                info!("new friends");
                let card = |id: &str, name: Bytes| {
                    PatData::new()
                        .with(0x01, Item::Long(1))
                        .with(0x02, Item::String(Bytes::copy_from_slice(id.as_bytes())))
                        .with(0x03, Item::String(name))
                };
                let to_them = NtcFriendAdd {
                    capcom_id: r.capcom_id,
                    friend: card(me, session.name.clone()),
                    accepted: 1,
                };
                shared.players.send(&from, frame(seq, &to_them)?);
                let to_me = NtcFriendAdd {
                    capcom_id: Bytes::copy_from_slice(me.as_bytes()),
                    friend: card(&from, from_name.into()),
                    accepted: 1,
                };
                out.push(frame(seq, &to_me)?);
            }
            Ok(out)
        }
        ReqFriendDelete::ID => {
            let r = ReqFriendDelete::decode(p)?;
            if let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(server)?
            {
                accounts::unfriend(db, my_id, id).await.map_err(server)?;
            }
            reply(seq, &AnsFriendDelete {})
        }
        ReqBlackList::ID => {
            let r = ReqBlackList::decode(p)?;
            let list = accounts::blacklist(db, my_id).await.map_err(server)?;
            reply(
                seq,
                &AnsBlackList {
                    unk: 0,
                    users: hunter_records(list, r.first, r.count),
                },
            )
        }
        ReqBlackAdd::ID => {
            let r = ReqBlackAdd::decode(p)?;
            let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(server)?
            else {
                return alert(AnsBlackAdd::ID, seq, Text::UnknownCapcomId);
            };
            if id != my_id {
                accounts::block(db, my_id, id).await.map_err(server)?;
            }
            reply(seq, &AnsBlackAdd {})
        }
        ReqBlackDelete::ID => {
            let r = ReqBlackDelete::decode(p)?;
            if let Some((id, _)) = accounts::hunter(db, &named(&r.capcom_id))
                .await
                .map_err(server)?
            {
                accounts::unblock(db, my_id, id).await.map_err(server)?;
            }
            reply(seq, &AnsBlackDelete {})
        }
        id => Err(unexpected(id)),
    }
}
