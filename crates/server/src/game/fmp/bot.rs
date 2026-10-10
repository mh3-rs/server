//! The quest bots' side of FMP (`roadmap/experiment-bots.md`, the `bot` feature): a bot joining and leaving a
//! party as a player would, the peer messages it says and answers for its P2P session (`crate::peer`).

use super::*;
use crate::peer;
use world::BOT_CONN;

/// A peer message (`NtcLayerBinary`/`Binary2`'s body after the sender: a length, then a record), if it is one.
pub(super) fn peer_message(body: &[u8]) -> Option<PatData> {
    let mut r = Reader::new(body);
    r.u16().ok()?;
    r.pat_data().ok()
}

/// A peer message as the consoles send it: a length, then the record.
fn peer_body(message: &PatData) -> Result<Vec<u8>, Fault> {
    let mut record = Vec::new();
    record.put_pat_data(message)?;
    let mut body = Vec::new();
    body.put_u16(record.len() as u16);
    body.extend_from_slice(&record);
    Ok(body)
}

/// What a console's peer message to its city means for the quest bots there (`peer.rs`): its record, which bots
/// answer with theirs (kind 7) as consoles do; the session host's pair states (kind 8).
pub(super) fn peer_message_from(shared: &Shared, me: &str, body: &[u8]) {
    let Some(message) = peer_message(body) else {
        return;
    };
    let bots: Vec<String> = {
        let world = shared.world.lock().unwrap();
        world
            .others(me)
            .into_iter()
            .filter(|o| shared.peers.is_bot(o))
            .collect()
    };
    match peer::kind(&message) {
        Some(peer::KIND_RECORD | peer::KIND_RECORD_ANSWER) => {
            let Some(record) = peer::Record::from_pat(&message) else {
                return;
            };
            shared.peers.learn(me, record.peer_id);
            if peer::kind(&message) != Some(peer::KIND_RECORD) {
                return;
            }
            for bot in bots {
                let Some(own) = shared.peers.bot_record(&bot) else {
                    continue;
                };
                shared.peers.deliver(
                    &bot,
                    peer::Event::Record {
                        from: me.to_owned(),
                        record,
                    },
                );
                bot_says(shared, &bot, Some(me), &own.pat(peer::KIND_RECORD_ANSWER));
            }
        }
        Some(peer::KIND_PAIR) => {
            let long = |f| match message.get(f) {
                Some(Item::Long(v)) => Some(*v),
                _ => None,
            };
            let state = match message.get(0x09) {
                Some(Item::Byte(s)) => *s,
                _ => return,
            };
            let (Some(from), Some(to)) = (long(0x07), long(0x08)) else {
                return;
            };
            for bot in bots {
                shared
                    .peers
                    .deliver(&bot, peer::Event::Pair { from, to, state });
            }
        }
        _ => {}
    }
}

/// A peer message from a bot, to its city (`NtcLayerBinary`) or to one console (`NtcLayerBinary2`), as consoles
/// send theirs.
pub fn bot_says(shared: &Shared, bot: &str, to: Option<&str>, message: &PatData) {
    let Ok(body) = peer_body(message) else { return };
    let Some(to) = to else {
        let world = shared.world.lock().unwrap();
        return bot_to_city(shared, &world, bot, &body);
    };
    let ntc = (|| {
        let mut data = Vec::new();
        data.put_pat_data(&sender(bot, &Bytes::from_static(BOT_NAME), false))?;
        data.extend_from_slice(&body);
        frame(
            0,
            &NtcLayerBinary2 {
                capcom_id: Bytes::copy_from_slice(bot.as_bytes()),
                data: Rest(data.into()),
            },
        )
    })();
    if let Ok(ntc) = ntc {
        shared.players.send(to, ntc);
    }
}

/// A message from a bot to its city (`NtcLayerBinary`), as consoles send theirs.
fn bot_to_city(shared: &Shared, world: &World, bot: &str, body: &[u8]) {
    let ntc = (|| {
        let mut data = Vec::new();
        data.put_lp2_string(bot.as_bytes())?;
        data.put_pat_data(&sender(bot, &Bytes::from_static(BOT_NAME), true))?;
        data.extend_from_slice(body);
        frame(
            0,
            &NtcLayerBinary {
                data: Rest(data.into()),
            },
        )
    })();
    if let Ok(ntc) = ntc {
        for other in world.others(bot) {
            shared.players.send(&other, ntc.clone());
        }
    }
}

/// The party flags of a town status (recorded: 1 leader, 2 member, 6 member and ready, 0 out of a party).
const PARTY_MEMBER: u32 = 2;
const PARTY_READY: u32 = 4;

/// The town status a console sends its city when its party changes (recorded, s05, s10, s11), as a body: two items,
/// 0x40 (1), then one of a type (9) no PAT decoder here knows, its header copied as recorded: kind 6, its Capcom ID,
/// its party flags (the city marks a party member from them), then 1 (0 until a hunter's first party).
fn party_status(capcom_id: &str, flags: u32) -> Vec<u8> {
    let mut status = Vec::with_capacity(0x34);
    status.put_slice(&[2, 0x40, 1, 1, 0, 9]);
    status.put_u16(0x2c);
    status.put_slice(&[6, 1, 0, 0]);
    status.put_slice(capcom_id.as_bytes());
    status.put_bytes(0, 20 - capcom_id.len() + 12);
    status.put_u32(flags);
    status.put_u32(1);
    let mut body = Vec::with_capacity(2 + status.len());
    body.put_u16(status.len() as u16);
    body.extend_from_slice(&status);
    body
}

/// The party's messages (`NtcCircleBinary`, kind in field 1, recorded): a joiner's on joining, every member's after a
/// departure.
const CIRCLE_JOINED: u8 = 2;
pub(super) const CIRCLE_DEPARTED: u8 = 3;

/// The quest bots' name.
pub(super) const BOT_NAME: &[u8] = b"[BOT]";

/// Why a bot can't join a player's party.
#[derive(Debug, PartialEq, Eq)]
pub enum BotRefusal {
    /// No brain connected (`BOT_PORT`).
    NoBrain,
    /// The player isn't in a quest party.
    NoParty,
    /// The city or the party has no room.
    Full,
    /// No port free for its socket (the relay's range), or no IPv4 `EXTERNAL_ADDRESS`.
    NoPeer,
}

/// A bot joins `player`'s quest party: a place in its city (the party's P2P session is the city's) and in the
/// party, with the notices a player joining would cause; the brain is told. Its Capcom ID.
pub fn add_bot(shared: &std::sync::Arc<Shared>, player: &str) -> Result<String, BotRefusal> {
    if !shared.brain.connected() {
        return Err(BotRefusal::NoBrain);
    }
    let bot = crate::accounts::bot_capcom_id();
    let mut world = shared.world.lock().unwrap();
    let circle = world.circle_of(player).ok_or(BotRefusal::NoParty)?;
    let city = circle.0;
    if world.can_join(city).is_err()
        || world.city_players(city).len() as u32 >= world::CITY_CAPACITY
        || world
            .circle(circle)
            .is_none_or(|c| c.members().count() >= c.capacity())
    {
        return Err(BotRefusal::Full);
    }
    world.enter(
        &bot,
        BOT_CONN,
        city.0,
        Bytes::from_static(BOT_NAME),
        Default::default(),
    );
    world.place(&bot).expect("just placed").gate = Some(city.1);
    if enter_city(shared, &mut world, &bot, city).is_err() {
        world.leave(&bot, BOT_CONN);
        return Err(BotRefusal::Full);
    }
    if let Ok(ntc) = layer_in(&world, &bot, 0) {
        for other in world.others(&bot) {
            shared.players.send(&other, ntc.clone());
        }
    }
    // Its party slot first: its index in the session's game messages (the slot from 0) comes from it; the party
    // learns of it below, after the record.
    let Ok(index) = world.join_circle(&bot, circle) else {
        depart(shared, &mut world, &bot, BOT_CONN, Depart::World);
        return Err(BotRefusal::Full);
    };
    let party = tunnel::Party {
        server: city.0,
        gate: city.1,
        city: city.2,
        circle: circle.1,
    };
    // A peer of the city's session: its socket, then its record to the city (after its arrival, which consoles check).
    let external = shared
        .config
        .external_address
        .as_deref()
        .and_then(|a| a.parse::<std::net::Ipv4Addr>().ok());
    // GT2's profile after the peer ID: the FMP server as consoles got it (`AnsFmpInfo`), the world server, then
    // gate<<16 | city (that order a guess: every recording is gate 1, city 1).
    let record = external.and_then(|external| {
        let mut profile = [0u8; 16];
        let mut p = &mut profile[..];
        p.put_u32(u32::from(external));
        p.put_u32(u32::from(shared.fmp_port));
        p.put_u32(city.0);
        p.put_u32(u32::from(city.1) << 16 | u32::from(city.2));
        shared
            .peers
            .spawn(shared, &bot, external, profile, u16::from(index) - 1, party)
    });
    let Some(record) = record else {
        // Out of the party unannounced, then out of the city.
        world.leave_circle(&bot);
        depart(shared, &mut world, &bot, BOT_CONN, Depart::World);
        return Err(BotRefusal::NoPeer);
    };
    if let Ok(body) = peer_body(&record.pat(peer::KIND_RECORD)) {
        bot_to_city(shared, &world, &bot, &body);
    }
    info!(bot, circle = circle.1, "bot joined quest");
    let ntc = NtcCircleJoin {
        circle: circle.1,
        capcom_id: Bytes::copy_from_slice(bot.as_bytes()),
        name: Bytes::from_static(BOT_NAME),
        player: index,
        state: 0,
    };
    if let Ok(ntc) = frame(0, &ntc) {
        for (_, member) in world
            .circle(circle)
            .map(|c| c.members())
            .into_iter()
            .flatten()
        {
            if member != bot {
                shared.players.send(member, ntc.clone());
            }
        }
    }
    // Ready at once: it follows its leader (the departure takes it along), as a joiner says it (`ReqCircleMatchOptionSet`).
    world.set_standby(&bot, true);
    let ready = PatData::new()
        .with(0x03, Item::Byte(1))
        .with(0x05, Item::String(Bytes::copy_from_slice(bot.as_bytes())))
        .with(0x06, Item::String(Bytes::from_static(BOT_NAME)))
        .with(0x04, Item::Byte(index));
    if let Ok(ntc) = frame(0, &NtcCircleMatchOptionSet { options: ready }) {
        for (_, member) in world
            .circle(circle)
            .map(|c| c.members())
            .into_iter()
            .flatten()
        {
            if member != bot {
                shared.players.send(member, ntc.clone());
            }
        }
    }
    bot_to_city(
        shared,
        &world,
        &bot,
        &party_status(&bot, PARTY_MEMBER | PARTY_READY),
    );
    // A joiner's party message (kind 2, value 0, recorded).
    let joined = PatData::new()
        .with(0x01, Item::Byte(CIRCLE_JOINED))
        .with(0x02, Item::Long(0));
    if let Ok(ntc) = peer_body(&joined)
        .and_then(|body| circle_binary(0, &bot, &Bytes::from_static(BOT_NAME), circle.1, &body))
    {
        for (_, member) in world
            .circle(circle)
            .map(|c| c.members())
            .into_iter()
            .flatten()
        {
            if member != bot {
                shared.players.send(member, ntc.clone());
            }
        }
    }
    shared.brain.send(tunnel::ToBrain::Create {
        bot: bot.clone(),
        party,
    });
    Ok(bot)
}

/// A bot leaves its party and city, as a player leaving the game would: `false` if there's no such bot.
pub fn remove_bot(shared: &Shared, bot: &str) -> bool {
    let mut world = shared.world.lock().unwrap();
    if !world.placed_by(bot, BOT_CONN) {
        return false;
    }
    depart(shared, &mut world, bot, BOT_CONN, Depart::World);
    true
}

/// A console's peer message to a quest bot (`NtcLayerBinary2`): its record answering the bot's, for the bot's task
/// (`peer.rs`); a link's kind 5, answered kind 4 as the city's host does. `false`: not to a bot.
pub(super) fn to_bot(shared: &Shared, me: &str, partner: &str, body: &[u8]) -> bool {
    if !shared.peers.is_bot(partner) {
        return false;
    }
    let message = peer_message(body);
    if message.as_ref().and_then(peer::kind) == Some(peer::KIND_LINKED) {
        bot_says(
            shared,
            partner,
            Some(me),
            &peer::linked(peer::KIND_LINKED_ANSWER),
        );
    }
    if let Some(record) = message.as_ref().and_then(peer::Record::from_pat) {
        shared.peers.learn(me, record.peer_id);
        let from = me.to_owned();
        shared
            .peers
            .deliver(partner, peer::Event::Record { from, record });
    }
    true
}

/// The party's sync after a departure (`NtcCircleBinary` kind 3): each member sends it and waits for everyone's,
/// quest bots too.
pub(super) fn party_sync(
    shared: &Shared,
    world: &World,
    me: &str,
    others: &[String],
    (seq, circle): (u16, u32),
    body: &[u8],
    out: &mut Vec<Frame>,
) -> Result<(), Fault> {
    if peer_message(body).as_ref().and_then(peer::kind) != Some(CIRCLE_DEPARTED) {
        return Ok(());
    }
    for bot in others.iter().filter(|m| world.placed_by(m, BOT_CONN)) {
        let to: Vec<String> = others.iter().filter(|m| *m != bot).cloned().collect();
        let from_bot = circle_binary(seq, bot, &Bytes::from_static(BOT_NAME), circle, body)?;
        notify(
            shared,
            me,
            &[to, vec![me.to_owned()]].concat(),
            &from_bot,
            out,
        );
    }
    Ok(())
}

/// Quest bots make room for a hunter: they leave the city (and their party) as it walks in, before it can pair with
/// them (`experiment-bots.md`).
pub(super) fn make_room(shared: &Shared, world: &mut World, me: &str, id: (u32, u16, u16)) {
    if world.placed_by(me, BOT_CONN) || world.can_join(id).is_err() {
        return;
    }
    for bot in world.city_players(id) {
        if world.placed_by(&bot, BOT_CONN) {
            depart(shared, world, &bot, BOT_CONN, Depart::World);
        }
    }
}

/// A bot departing: its peer stops, the brain is told.
pub(super) fn departing(shared: &Shared, me: &str, conn: u64) {
    if conn == BOT_CONN {
        info!(bot = me, "bot left");
        shared.peers.remove(me);
        shared
            .brain
            .send(tunnel::ToBrain::Leave { bot: me.to_owned() });
    }
}

/// Someone left city `id`: bots don't stay in a city without a hunter.
pub(super) fn left_behind(shared: &Shared, world: &mut World, conn: u64, id: (u32, u16, u16)) {
    let players = world.city_players(id);
    if conn != BOT_CONN && players.iter().all(|p| world.placed_by(p, BOT_CONN)) {
        for bot in players {
            depart(shared, world, &bot, BOT_CONN, Depart::World);
        }
    }
}

/// Bots exist only in parties with a hunter: one whose party ended (left behind at departure, or the party broke) or
/// has no hunter left (a departed quest goes to its next member) leaves. Not the requester: a bot leaving on its own
/// is already departing (`depart`).
pub(super) fn partyless(
    shared: &Shared,
    world: &mut World,
    requester: &str,
    city: (u32, u16, u16),
) {
    let hunterless = |id| {
        world
            .circle(id)
            .is_none_or(|c| c.members().all(|(_, m)| world.placed_by(m, BOT_CONN)))
    };
    let partyless: Vec<String> = world
        .city_players(city)
        .into_iter()
        .filter(|p| p != requester && world.placed_by(p, BOT_CONN))
        .filter(|p| world.circle_of(p).is_none_or(hunterless))
        .collect();
    for bot in partyless {
        depart(shared, world, &bot, BOT_CONN, Depart::World);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn party_status_is_what_consoles_send() {
        // dev2's, a party member (s10, dev2-05-fmp, t=171041580), after its empty sender.
        let recorded = "0034024001010009002c06010000304643474b3200000000000000000000000000000000000000000000000000000000000200000001";
        assert_eq!(hex::encode(party_status("0FCGK2", PARTY_MEMBER)), recorded);
    }
}
