//! The quest bots' P2P side (`roadmap/experiment-bots.md`, A1): to the consoles, a bot is a peer of its city's
//! session. Its UDP socket comes from the relay's range (open on the operator's side already); its peer record goes to
//! the city through PAT (`NtcLayerBinary` kind 6, answered with kind 7); NAT negotiation pairs it with each console
//! (`natneg.rs` knows a bot pair by its cookie: the XOR of two peer IDs, one a bot's) and the bot answers the
//! consoles' pings; then each pair connects (GT2, the connection, the session: `link.rs`), the bot on the side the
//! city's host gave it. A pair not up 20 s after the host announced it, or none announced 20 s after joining, and the
//! bot leaves: it would hang its party's departure.

mod body;
mod link;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use wire::natneg::{Body, MAGIC, Peer, Record as NatNeg};
use wire::{Item, PatData};

use crate::Shared;
use link::{Link, Me, Role, Say};

/// A peer record (`NtcLayerBinary` kinds 6 and 7, `NetworkLayerPat::sendPeerRecord`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub peer_id: u32,
    /// Two peers with the same mode skip NAT negotiation (a LAN shortcut): a bot's differs from every console's.
    pub mode: u32,
    pub session: u32,
    /// The peer's game (GT2) socket's port.
    pub port: u16,
}

/// The kinds of peer messages (field 1 of the record).
pub const KIND_RECORD: u8 = 6;
pub const KIND_RECORD_ANSWER: u8 = 7;
pub const KIND_PAIR: u8 = 8;
/// After a link: kind 5 from the pair's "to" side, answered kind 4.
pub const KIND_LINKED: u8 = 5;
pub const KIND_LINKED_ANSWER: u8 = 4;
/// A pair's states (kind 8).
const LINKED: u8 = 1;
const NEGOTIATING: u8 = 2;

impl Record {
    pub fn pat(&self, kind: u8) -> PatData {
        PatData::new()
            .with(0x01, Item::Byte(kind))
            .with(0x03, Item::Long(self.peer_id))
            .with(0x04, Item::Long(self.mode))
            .with(0x05, Item::Long(self.session))
            .with(0x06, Item::Word(self.port))
    }

    /// A kind 6 or 7 record.
    pub fn from_pat(data: &PatData) -> Option<Record> {
        let long = |f| match data.get(f) {
            Some(Item::Long(v)) => Some(*v),
            _ => None,
        };
        let port = match data.get(0x06) {
            Some(Item::Word(p)) => *p,
            _ => return None,
        };
        Some(Record {
            peer_id: long(0x03)?,
            mode: long(0x04)?,
            session: long(0x05)?,
            port,
        })
    }
}

/// A peer message's kind (field 1).
pub fn kind(data: &PatData) -> Option<u8> {
    match data.get(0x01) {
        Some(Item::Byte(k)) => Some(*k),
        _ => None,
    }
}

/// What a bot's task learns from the city (through PAT).
#[derive(Debug)]
pub enum Event {
    /// A console's peer record (its kind 6, or the kind 7 answering the bot's).
    Record {
        from: String,
        record: Record,
    },
    /// The session host's pair states (kind 8): from, to, state (2 negotiating).
    Pair {
        from: u32,
        to: u32,
        state: u8,
    },
    /// From the brain: where the bot is to be, and its area.
    Move {
        position: [f32; 3],
        run: bool,
    },
    Area {
        area: u8,
    },
}

/// A bot as a peer.
pub struct Bot {
    pub record: Record,
    /// Where consoles reach it (the server's external address, its port).
    pub address: SocketAddrV4,
    events: mpsc::UnboundedSender<Event>,
    stop: CancellationToken,
}

/// The peer IDs the server knows: consoles' (from the records it relays) and bots'.
#[derive(Default)]
pub struct Peers {
    consoles: DashMap<String, u32>,
    bots: DashMap<String, Bot>,
}

impl Peers {
    /// A console's peer ID, from a record it sent.
    pub fn learn(&self, capcom_id: &str, peer_id: u32) {
        self.consoles.insert(capcom_id.to_owned(), peer_id);
    }

    pub fn is_bot(&self, capcom_id: &str) -> bool {
        self.bots.contains_key(capcom_id)
    }

    /// The console with this peer ID.
    pub fn console_of(&self, peer_id: u32) -> Option<String> {
        self.consoles
            .iter()
            .find(|c| *c.value() == peer_id)
            .map(|c| c.key().clone())
    }

    pub fn bot_record(&self, bot: &str) -> Option<Record> {
        self.bots.get(bot).map(|b| b.record)
    }

    /// The bot's address when `cookie` pairs a bot with a known console.
    pub fn bot_pair(&self, cookie: u32) -> Option<SocketAddrV4> {
        self.bots.iter().find_map(|b| {
            let other = cookie ^ b.record.peer_id;
            self.consoles
                .iter()
                .any(|c| *c.value() == other)
                .then_some(b.address)
        })
    }

    /// Tells a bot's task.
    pub fn deliver(&self, bot: &str, event: Event) {
        if let Some(b) = self.bots.get(bot) {
            let _ = b.events.send(event);
        }
    }

    /// A bot becomes a peer: a socket from the relay's range, a record, its task. `profile` is GT2's profile after
    /// the peer ID, `index` its index in the session's game messages (`link::Me`). `None` without a free port.
    pub fn spawn(
        &self,
        shared: &Arc<Shared>,
        bot: &str,
        external: Ipv4Addr,
        profile: [u8; 16],
        index: u16,
        party: tunnel::Party,
    ) -> Option<Record> {
        let relays = shared.relays.as_ref()?;
        let (socket, port) = relays.take_port()?;
        let socket = UdpSocket::from_std(socket).ok()?;
        let record = Record {
            peer_id: nonzero_random(),
            mode: nonzero_random(),
            session: u32::from(external),
            port,
        };
        let (events, rx) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        info!(bot, port, "bot peer up");
        let mut full = [0u8; 20];
        full[..4].copy_from_slice(&record.peer_id.to_be_bytes());
        full[4..].copy_from_slice(&profile);
        let me = Me {
            capcom_id: bot.to_owned(),
            peer_id: record.peer_id,
            profile: full,
            index,
        };
        tokio::spawn(run(
            me,
            party,
            socket,
            rx,
            stop.clone(),
            shared.clone(),
            port,
        ));
        self.bots.insert(
            bot.to_owned(),
            Bot {
                record,
                address: SocketAddrV4::new(external, port),
                events,
                stop,
            },
        );
        Some(record)
    }

    /// The bot stops being a peer (its task ends, its port goes back).
    pub fn remove(&self, bot: &str) {
        if let Some((_, b)) = self.bots.remove(bot) {
            b.stop.cancel();
        }
    }
}

fn nonzero_random() -> u32 {
    loop {
        let mut b = [0u8; 4];
        openssl::rand::rand_bytes(&mut b).expect("OpenSSL random source");
        let v = bytes::Buf::get_u32(&mut &b[..]);
        if v != 0 && v != u32::MAX {
            return v;
        }
    }
}

/// A pair the city's host announced with the bot: its side, and when it must be up by.
struct Pair {
    role: Role,
    deadline: Instant,
    /// The bot connected (the "to" side), once.
    connected: bool,
}

/// A bot's socket: NAT negotiation's pings answered, then a link per console (`link.rs`).
async fn run(
    me: Me,
    party: tunnel::Party,
    socket: UdpSocket,
    mut events: mpsc::UnboundedReceiver<Event>,
    stop: CancellationToken,
    shared: Arc<Shared>,
    port: u16,
) {
    let bot = me.capcom_id.clone();
    let born = Instant::now();
    let mut links: HashMap<SocketAddrV4, Link> = HashMap::new();
    // The consoles' game sockets, from their pings: the console's peer ID each, and when first heard. A ping may come
    // before the host's pair announcement reaches the bot (recorded: 9 ms before), and the pings end with NAT
    // negotiation: a pair and an address make a connect, whichever comes first.
    let mut addresses: HashMap<SocketAddrV4, (u32, Instant)> = HashMap::new();
    let mut pairs: HashMap<u32, Pair> = HashMap::new();
    // Large monsters the bot owns (by monster group: their handover to it), each console's slot and area, and the
    // bot's own counter in the handovers it sends.
    let mut owned: HashMap<u8, Vec<u8>> = HashMap::new();
    let mut hunters: HashMap<u32, (u8, u8)> = HashMap::new();
    let mut handovers: u32 = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut buf = [0u8; 2048];
    loop {
        let now = Instant::now();
        let mut outs = Vec::new();
        tokio::select! {
            _ = stop.cancelled() => break,
            event = events.recv() => match event {
                Some(Event::Record { from, record }) => debug!(bot, from, peer_id = record.peer_id, port = record.port, "bot peer: a record"),
                Some(Event::Pair { from, to, state }) => {
                    debug!(bot, from, to, state, "bot peer: a pair state");
                    let side = match (from == me.peer_id, to == me.peer_id) {
                        (true, _) => Some((to, Role::Listener)),
                        (_, true) => Some((from, Role::Client)),
                        _ => None,
                    };
                    if let (Some((console, role)), NEGOTIATING) = (side, state) {
                        pairs.entry(console).or_insert(Pair { role, deadline: now + link::TIMEOUT, connected: false });
                    }
                }
                Some(Event::Move { position, run }) => {
                    for (address, link) in links.iter_mut() {
                        let datagrams = link.go(position, run, now);
                        outs.push((*address, link.console, link::Out { datagrams, ..Default::default() }));
                    }
                }
                Some(Event::Area { area }) => links.values_mut().for_each(|l| l.enter(area)),
                None => break,
            },
            received = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                let std::net::SocketAddr::V4(from) = from else { continue };
                let datagram = &buf[..n];
                if datagram.starts_with(&MAGIC) {
                    // GameSpy's rule: a finished ping gets no answer (or the two sides ping each other forever); ours is
                    // finished once the console got our data.
                    let Ok(NatNeg { cookie, body: Body::Ping(ping) }) = NatNeg::decode(datagram) else { continue };
                    addresses.entry(from).or_insert((cookie ^ me.peer_id, now));
                    if ping.finished != 0 {
                        continue;
                    }
                    let answer = NatNeg {
                        cookie,
                        body: Body::Ping(Peer { addr: from, got_your_data: 1, finished: ping.got_your_data }),
                    };
                    let _ = socket.send_to(&answer.encode(), from).await;
                    continue;
                }
                // A console connecting to the bot (its listening side): only one NAT negotiation told about.
                let link = match links.entry(from) {
                    std::collections::hash_map::Entry::Occupied(link) => link.into_mut(),
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        let Some(&(console, _)) = addresses.get(&from) else { continue };
                        if !wire::p2p::Gt2::is_gt2(datagram) {
                            continue;
                        }
                        debug!(bot, console, "bot peer: a console connects");
                        slot.insert(Link::new(me.clone(), Role::Listener, console, now))
                    }
                };
                if wire::p2p::Gt2::is_gt2(datagram) {
                    debug!(bot, kind = datagram.get(2), "bot peer: GT2 from a console");
                }
                let was_up = link.up();
                outs.push((from, link.console, link.receive(datagram, now)));
                if link.up() && !was_up {
                    info!(bot, role = ?link.role, "bot peer: a link is up");
                }
            }
            _ = tick.tick() => {
                // The bot the "to" side: it connects once NAT negotiation had time to finish on the console's side.
                for (address, (console, heard)) in &addresses {
                    let Some(pair) = pairs.get_mut(console) else { continue };
                    if pair.role != Role::Client || pair.connected || now < *heard + CONNECT_AFTER {
                        continue;
                    }
                    pair.connected = true;
                    debug!(bot, console, "bot peer: connecting");
                    let (address, console) = (*address, *console);
                    let mut link = Link::new(me.clone(), Role::Client, console, now);
                    outs.push((address, console, link.start(now)));
                    links.insert(address, link);
                }
                links.retain(|address, link| match link.tick(now) {
                    Some(out) => {
                        outs.push((*address, link.console, out));
                        true
                    }
                    None => {
                        info!(bot, up = link.up(), "bot peer: a link ended");
                        pairs.remove(&link.console);
                        false
                    }
                });
                let late = pairs.iter().any(|(console, pair)| {
                    pair.deadline <= now && !links.values().any(|l| l.console == *console && l.up())
                });
                if late || (pairs.is_empty() && links.is_empty() && now.duration_since(born) > link::TIMEOUT) {
                    info!(bot, "bot peer: its session didn't come up in time, the bot leaves");
                    crate::game::fmp::remove_bot(&shared, &bot);
                    break;
                }
            }
        }
        // Ownership: a monster the bot was handed goes to the first hunter in its area (until then it's frozen, unseen).
        for (_, console, out) in &mut outs {
            if let (Some((_, area)), Some(slot)) = (out.hunter, out.slot) {
                hunters.insert(*console, (slot, area));
            }
            for given in out.given.drain(..) {
                owned.insert(given[2], given);
            }
        }
        let due: Vec<(u8, u8)> = owned
            .iter()
            .filter_map(|(group, given)| {
                let area = given[link::OWNER_AT + 1];
                hunters
                    .values()
                    .find(|(_, a)| *a == area)
                    .map(|(slot, _)| (*group, *slot))
            })
            .collect();
        for (group, slot) in due {
            let given = owned.remove(&group).expect("just found");
            handovers = handovers.wrapping_add(1);
            let handover = handover(&given, me.index, handovers, slot);
            info!(
                bot,
                group, slot, "bot peer: a monster handed to a hunter in its area"
            );
            for (address, link) in links.iter_mut() {
                let datagrams = link.say(handover.clone());
                outs.push((
                    *address,
                    link.console,
                    link::Out {
                        datagrams,
                        ..Default::default()
                    },
                ));
            }
        }
        for (address, console, out) in outs {
            for datagram in out.datagrams {
                let _ = socket.send_to(&datagram, address).await;
            }
            // A hunter's position, for the brain.
            if let (Some((position, area)), Some(hunter)) =
                (out.hunter, shared.peers.console_of(console))
            {
                shared.brain.send(tunnel::ToBrain::Hunter {
                    party,
                    hunter,
                    position,
                    area,
                });
            }
            for say in out.say {
                // The report to the city; kind 5 to the console alone.
                let (message, to) = match say {
                    Say::Report { from, to } => (
                        PatData::new()
                            .with(0x01, Item::Byte(KIND_PAIR))
                            .with(0x07, Item::Long(from))
                            .with(0x08, Item::Long(to))
                            .with(0x09, Item::Byte(LINKED)),
                        None,
                    ),
                    Say::Linked => (linked(KIND_LINKED), shared.peers.console_of(console)),
                };
                crate::game::fmp::bot_says(&shared, &bot, to.as_deref(), &message);
            }
        }
    }
    if let Some(relays) = &shared.relays {
        relays.give_port(port);
    }
    info!(bot, "bot peer down");
}

/// A monster handover from the bot to `slot`: the one it was given, from the bot, with its own counter (a console's
/// own handover back, recorded: the long hunt, dev2 to dev1).
fn handover(given: &[u8], index: u16, counter: u32, slot: u8) -> Vec<u8> {
    let mut handover = given.to_vec();
    handover[..2].copy_from_slice(&index.to_be_bytes());
    handover[4..8].copy_from_slice(&counter.to_be_bytes());
    handover[link::OWNER_AT] = slot;
    handover
}

/// Kind 5 or 4 (a link's two sides; the second field 0, as recorded).
pub fn linked(kind: u8) -> PatData {
    PatData::new()
        .with(0x01, Item::Byte(kind))
        .with(0x02, Item::Long(0))
}

/// How long the bot leaves a console to finish NAT negotiation before connecting (the console waits 5 s, then
/// connects; the bot's challenges are resent for 15 s).
const CONNECT_AFTER: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_monster_goes_from_the_bot_to_a_hunter() {
        // The lab's: dev1 (slot 0) handing group 5 to the bot (slot 1) in area 1; the bot hands it to slot 0.
        let given = ::hex::decode("000005020000001cc4379226c2aa157a44dcae38000000000000fc820000010100000000000000000000000000000000").unwrap();
        let back = handover(&given, 1, 7, 0);
        assert_eq!(&back[..8], [0, 1, 5, 2, 0, 0, 0, 7]);
        assert_eq!((back[link::OWNER_AT], back[link::OWNER_AT + 1]), (0, 1));
        assert_eq!(back[8..link::OWNER_AT], given[8..link::OWNER_AT]);
    }
}
