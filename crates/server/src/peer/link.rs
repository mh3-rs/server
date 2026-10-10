//! A quest bot's connection with one console (`roadmap/experiment-bots.md`, step 2b): GT2's connect, then the
//! game's connection handshake, then the session as consoles keep it in town. Pure: datagrams and a clock in,
//! datagrams and PAT messages out, so a test can play the console. Every number here is from the recordings (s04).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut, Bytes};
use wire::p2p::{self, Frame, Gt2, Message, flags, message};

use tracing::debug;

use super::body::Body;

/// How long a console may stay silent, and a pair take to come up (the game's 20 s).
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Resends (GT2's challenge, the hello) and keepalives (pings, session op 6): every second, as consoles do.
const EVERY: Duration = Duration::from_secs(1);
/// GT2 challenges sent before giving up.
const CHALLENGES: u32 = 15;
/// The MTU consoles answer a probe with, and the probe's size.
const MTU: u32 = 1024;
const MTU_PROBE: usize = 1028;

/// The connection's frames before the session: their source and destination.
const CONNECTION_NONCE: u32 = 1;

/// Connection messages (`NetworkConnectionStable.cpp`).
const HELLO: u8 = 0x81;
const HELLO_ANSWER: u8 = 0x82;
const ESTABLISHED: u8 = 0x83;
const PING: u8 = 0x91;
const PONG: u8 = 0x92;
const MTU_ANSWER: u8 = 0x95;
/// The quest departure's roll call (a game message's opcode).
const ROLL_CALL: [u8; 2] = [0x0d, 0x0a];
/// The host's spawn state at the quest's start (group 1, kind 5).
const HOST_SPAWN: [u8; 2] = [0x01, 0x05];
/// A player message's kind 2 (its position, about twice a second: the group byte is the player's slot from 1,
/// below 5), its map byte and its area (recorded; `body.rs`).
const PLAYER_POSITION: u8 = 2;
const MAP_AT: usize = 0x1c;
/// A player message's kind 1 (its full state) and its map byte (the area follows).
const PLAYER_STATE: u8 = 1;
const STATE_MAP_AT: usize = 0x20;
/// A monster message's kind 2: its ownership handed over, the new owner's slot at `OWNER_AT`, the monster's area after.
const MONSTER_HANDOVER: u8 = 2;
pub const OWNER_AT: usize = 0x1e;
/// The session's keepalive in town.
const SESSION_KEEPALIVE: u8 = 6;

/// Which side of GT2's connect the bot is (the city's host decides: the pair's "to" side connects).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Client,
    Listener,
}

/// What the bot must say through PAT for this link.
#[derive(Debug, PartialEq, Eq)]
pub enum Say {
    /// The pair is linked (kind 8, state 1): the "to" side reports it to the city.
    Report { from: u32, to: u32 },
    /// Kind 5, to the console alone: the link is up (the console answers kind 4).
    Linked,
}

/// What a link produced: datagrams for the console, PAT messages.
#[derive(Debug, Default)]
pub struct Out {
    pub datagrams: Vec<Bytes>,
    pub say: Vec<Say>,
    /// The console's hunter's position and area, from its position message.
    pub hunter: Option<([f32; 3], u8)>,
    /// The console's slot (its game messages' sender index), with `hunter`.
    pub slot: Option<u8>,
    /// Large monsters handed to the bot (their handover messages).
    pub given: Vec<Vec<u8>>,
}

/// Who the bot is, for every link.
#[derive(Clone, Debug)]
pub struct Me {
    pub capcom_id: String,
    pub peer_id: u32,
    /// GT2's 20-byte profile for a connect: the "to" peer's ID, the FMP server's IP and port, the world server, then
    /// gate<<16 | city.
    pub profile: [u8; 20],
    /// Its index in the session's game messages (its party slot, from 0: the host is 0).
    pub index: u16,
}

#[derive(Debug, PartialEq, Eq)]
enum Gt2State {
    /// Client: our challenge sent, `tries` times.
    Challenged {
        tries: u32,
    },
    /// Listener: our challenge sent with our response, waiting for theirs.
    Answered,
    Connected,
}

/// The console's side of the connection, from its hello.
#[derive(Debug, Default)]
struct Them {
    /// Its channel 0's next sequence number (reliable: acknowledged), its channel 1's first (always acknowledged).
    channel_0: u16,
    channel_1: u16,
    nonce: u32,
    session_0: u16,
}

pub struct Link {
    pub role: Role,
    /// The console's peer ID.
    pub console: u32,
    me: Me,
    gt2: Gt2State,
    challenge: [u8; 32],
    /// Our sequence numbers (connection channels 0 and 1, session channel 0) and session nonce.
    channel_0: u16,
    channel_1: u16,
    session_0: u16,
    session_1: u16,
    nonce: u32,
    them: Option<Them>,
    /// Its hello answered ours (0x82); its 0x83 came (the session is up).
    answered: bool,
    up: bool,
    closed: bool,
    /// In a quest: the bot's body; the map byte of each area, as hunters there send it.
    body: Option<Body>,
    maps: HashMap<u8, u8>,
    ping: u8,
    epoch: Instant,
    last_sent: Instant,
    pub last_heard: Instant,
}

impl Link {
    /// A link the bot opens (client) or that a console opened (listener, with its challenge).
    pub fn new(me: Me, role: Role, console: u32, now: Instant) -> Link {
        let random = random_bytes::<12>();
        let mut r = &random[..];
        Link {
            role,
            console,
            me,
            gt2: match role {
                Role::Client => Gt2State::Challenged { tries: 0 },
                Role::Listener => Gt2State::Answered,
            },
            challenge: p2p::gt2_challenge(random_bytes()),
            channel_0: r.get_u16(),
            channel_1: r.get_u16(),
            session_0: r.get_u16(),
            session_1: r.get_u16(),
            // Never 0 or -1 (the game's rule).
            nonce: r.get_u32().clamp(2, u32::MAX - 1),
            them: None,
            answered: false,
            up: false,
            closed: false,
            body: None,
            maps: HashMap::new(),
            ping: 0,
            epoch: now,
            last_sent: now,
            last_heard: now,
        }
    }

    /// From the brain: where the bot goes, walking or running (`body.rs`). Nothing before the quest's spawn.
    pub fn go(&mut self, target: [f32; 3], run: bool, now: Instant) -> Vec<Bytes> {
        let messages = self
            .body
            .as_mut()
            .map(|b| b.go(target, run, now))
            .unwrap_or_default();
        self.game(messages)
    }

    /// From the brain: the bot's next move starts in `area`.
    pub fn enter(&mut self, area: u8) {
        let map = self.maps.get(&area).copied().unwrap_or(1);
        if let Some(body) = &mut self.body {
            body.enter(area, map);
        }
    }

    /// A game message from the bot (on the session). Nothing before the quest's spawn: no session to say it on.
    pub fn say(&mut self, message: Vec<u8>) -> Vec<Bytes> {
        self.game(vec![message])
    }

    /// Game messages on the session, one frame each.
    fn game(&mut self, messages: Vec<Vec<u8>>) -> Vec<Bytes> {
        messages
            .into_iter()
            .map(|m| self.session(&[Message::new(message::GAME, m)]))
            .collect()
    }

    /// Whether the session is up.
    pub fn up(&self) -> bool {
        self.up
    }

    /// The client's first challenge.
    pub fn start(&mut self, now: Instant) -> Out {
        let mut out = Out::default();
        if self.role == Role::Client {
            self.challenge_again(now, &mut out);
        }
        out
    }

    fn challenge_again(&mut self, now: Instant, out: &mut Out) {
        if let Gt2State::Challenged { tries } = &mut self.gt2 {
            *tries += 1;
            self.last_sent = now;
            out.datagrams
                .push(gt2(p2p::GT2_CLIENT_CHALLENGE, 0, 0, &self.challenge));
        }
    }

    /// A datagram from the console.
    pub fn receive(&mut self, datagram: &[u8], now: Instant) -> Out {
        self.last_heard = now;
        let mut out = Out::default();
        if Gt2::is_gt2(datagram) {
            if let Ok(packet) = Gt2::decode(datagram) {
                self.gt2_packet(&packet, now, &mut out);
            }
            return out;
        }
        let Ok(frames) = Frame::decode_datagram(datagram) else {
            return out;
        };
        for frame in frames {
            self.frame(&frame, now, &mut out);
        }
        out
    }

    fn gt2_packet(&mut self, packet: &Gt2, now: Instant, out: &mut Out) {
        let body = &packet.body[..];
        match (self.role, packet.kind) {
            // Listener: the console's challenge, answered with our response and our challenge (also when resent).
            (Role::Listener, p2p::GT2_CLIENT_CHALLENGE) if body.len() >= 32 => {
                let mut answer = response(body).to_vec();
                answer.extend_from_slice(&self.challenge);
                out.datagrams
                    .push(gt2(p2p::GT2_SERVER_CHALLENGE, 0, 1, &answer));
            }
            // Listener: its response (and its profile, which we take as is): accepted.
            (Role::Listener, p2p::GT2_CLIENT_RESPONSE) if body.len() >= 32 => {
                if !answers(&self.challenge, &body[..32]) {
                    return;
                }
                if self.gt2 != Gt2State::Connected {
                    self.gt2 = Gt2State::Connected;
                    self.hello(HELLO, now, out);
                }
                out.datagrams.push(gt2(p2p::GT2_ACCEPT, 1, 2, &[]));
            }
            // Client: its response to our challenge and its own challenge: our response and the profile.
            (Role::Client, p2p::GT2_SERVER_CHALLENGE) if body.len() >= 64 => {
                if !answers(&self.challenge, &body[..32]) {
                    return;
                }
                let mut answer = response(&body[32..64]).to_vec();
                answer.extend_from_slice(&self.me.profile);
                out.datagrams
                    .push(gt2(p2p::GT2_CLIENT_RESPONSE, 1, 1, &answer));
            }
            // Client: accepted. Acknowledged; the pair reported to the city; then the hello.
            (Role::Client, p2p::GT2_ACCEPT) => {
                out.datagrams.push(gt2(p2p::GT2_ACK, 0, 2, &[]));
                if self.gt2 != Gt2State::Connected {
                    self.gt2 = Gt2State::Connected;
                    out.say.push(Say::Report {
                        from: self.console,
                        to: self.me.peer_id,
                    });
                    self.hello(HELLO, now, out);
                }
            }
            // The console closed the connection (leaving the city).
            (_, p2p::GT2_CLOSED) => self.closed = true,
            _ => {}
        }
    }

    fn frame(&mut self, frame: &Frame, now: Instant, out: &mut Out) {
        let messages = Message::decode_all(&frame.payload).unwrap_or_default();
        if frame.flags & flags::SESSION != 0 {
            // The session's channel 0: acknowledged when it carries something (op 6 in town).
            if frame.flags & flags::CHANNEL_0 != 0 && !frame.payload.is_empty() {
                if let Some(them) = &mut self.them {
                    them.session_0 = frame.seq.wrapping_add(frame.payload.len() as u16);
                }
                out.datagrams.push(self.session(&[]));
            }
            // The game's messages (index, opcode, then data): the departure's roll call answered (recorded, s05 and
            // s11: the host sends 0d0a until each member answers with its own index; then 0d0b, and the quest).
            // The quest's start: the host's spawn state (0105) answered with ours (0205: the same layout, the bot on the
            // host's spot), then our position (0202) twice a second (recorded, s05 and s11, from the joiner's side).
            for message in messages.iter().filter(|m| m.flags & message::GAME != 0) {
                // A large monster handed to the bot (monster groups 5–8, kind 2, its new owner's slot at `OWNER_AT`):
                // for the task, which keeps it until a hunter is in its area (A4, `experiment-bots.md`).
                if let Some(&[group, MONSTER_HANDOVER]) = message.data.get(2..4)
                    && (5..=8).contains(&group)
                    && message.data.get(OWNER_AT) == Some(&(self.me.index as u8))
                {
                    debug!(bot = %self.me.capcom_id, group, "bot peer: a monster given to the bot");
                    out.given.push(message.data.to_vec());
                }
                let mut answer = match message.data.get(2..4) {
                    Some(op) if op == ROLL_CALL => message.data.to_vec(),
                    Some(op) if op == HOST_SPAWN => {
                        let Some((spawn, body)) = Body::spawn(self.me.index, &message.data, now)
                        else {
                            continue;
                        };
                        self.body = Some(body);
                        spawn
                    }
                    // A hunter's position (kind 2, every 0.5 s) or full state (kind 1, the moment its action changes: its
                    // starts and stops, the 0.5 s timer restarting after it): both to the brain, with the area's map byte.
                    Some(op) if op[0] < 5 => {
                        let at = match op[1] {
                            PLAYER_POSITION => (4, MAP_AT),
                            PLAYER_STATE => (8, STATE_MAP_AT),
                            _ => continue,
                        };
                        let Some(fields) = message.data.get(at.0..at.1 + 2) else {
                            continue;
                        };
                        let mut r = fields;
                        let position = [r.get_f32(), r.get_f32(), r.get_f32()];
                        let (map, area) = (fields[at.1 - at.0], fields[at.1 - at.0 + 1]);
                        self.maps.insert(area, map);
                        out.hunter = Some((position, area));
                        out.slot = Some(message.data[1]);
                        continue;
                    }
                    _ => continue,
                };
                answer[..2].copy_from_slice(&self.me.index.to_be_bytes());
                out.datagrams
                    .push(self.session(&[Message::new(message::GAME, answer)]));
            }
            return;
        }
        if frame.flags & flags::MTU_PROBE != 0 {
            let mut answer = vec![MTU_ANSWER];
            answer.put_u32(MTU);
            out.datagrams
                .push(self.connection_1(&[Message::new(0, answer)]));
            return;
        }
        if frame.flags & flags::CHANNEL_0 != 0 && !frame.payload.is_empty() {
            if let Some(them) = &mut self.them {
                them.channel_0 = frame.seq.wrapping_add(frame.payload.len() as u16);
            }
            out.datagrams.push(self.connection_0(&[]));
        }
        for message in messages {
            self.message(&message.data, now, out);
        }
    }

    fn message(&mut self, data: &[u8], now: Instant, out: &mut Out) {
        match data.first() {
            Some(&kind @ (HELLO | HELLO_ANSWER)) => {
                let Some(them) = parse_hello(data) else {
                    return;
                };
                // Its channel 0 may have moved on since (a resent hello): keep what we acknowledged.
                let channel_0 = self.them.as_ref().map_or(them.channel_0, |t| t.channel_0);
                let session_0 = self.them.as_ref().map_or(them.session_0, |t| t.session_0);
                self.them = Some(Them {
                    channel_0,
                    session_0,
                    ..them
                });
                if kind == HELLO {
                    self.hello(HELLO_ANSWER, now, out);
                } else if !self.answered {
                    self.answered = true;
                    if self.role == Role::Client {
                        out.say.push(Say::Linked);
                    }
                    let established = self.connection_0(&[Message::new(0, vec![ESTABLISHED])]);
                    out.datagrams.push(established);
                    out.datagrams.push(self.mtu_probe());
                    self.keepalive(now, out);
                }
            }
            Some(&ESTABLISHED) => self.up = true,
            Some(&PING) if data.len() >= 6 => {
                let mut pong = data[..6].to_vec();
                pong[0] = PONG;
                out.datagrams
                    .push(self.connection_1(&[Message::new(0, pong)]));
            }
            _ => {}
        }
    }

    /// Resends and keepalives; `None` once the link is dead (never answered, closed, or the console went silent).
    pub fn tick(&mut self, now: Instant) -> Option<Out> {
        let mut out = Out::default();
        if self.closed || now.duration_since(self.last_heard) > TIMEOUT {
            return None;
        }
        let moves = self.body.as_mut().map(|b| b.step(now)).unwrap_or_default();
        out.datagrams.extend(self.game(moves));
        if now.duration_since(self.last_sent) < EVERY {
            return Some(out);
        }
        match self.gt2 {
            Gt2State::Challenged { tries } if tries >= CHALLENGES => return None,
            Gt2State::Challenged { .. } => self.challenge_again(now, &mut out),
            Gt2State::Answered => {}
            Gt2State::Connected if !self.answered => self.hello(HELLO, now, &mut out),
            Gt2State::Connected => self.keepalive(now, &mut out),
        }
        Some(out)
    }

    fn keepalive(&mut self, now: Instant, out: &mut Out) {
        self.last_sent = now;
        let ms = now.duration_since(self.epoch).as_millis() as u32;
        let mut ping = vec![PING, self.ping];
        ping.put_u32(ms);
        self.ping = self.ping.wrapping_add(1);
        out.datagrams
            .push(self.connection_1(&[Message::new(0, ping)]));
        out.datagrams
            .push(self.session(&[Message::new(0, vec![SESSION_KEEPALIVE])]));
    }

    /// The hello (0x81) or its answer (0x82): our channels' sequence numbers and our record (kind 3, the Capcom ID the
    /// console knows from PAT, our session nonce and its channels' sequence numbers).
    fn hello(&mut self, kind: u8, now: Instant, out: &mut Out) {
        self.last_sent = now;
        let mut data = vec![kind];
        data.put_u16(self.channel_0);
        data.put_u16(self.channel_1);
        data.put_u32(22);
        data.put_slice(&[3, 0, 0, 0]);
        let mut id = [0u8; 10];
        let len = self.me.capcom_id.len().min(10);
        id[..len].copy_from_slice(&self.me.capcom_id.as_bytes()[..len]);
        data.put_slice(&id);
        data.put_u32(self.nonce);
        data.put_u16(self.session_0);
        data.put_u16(self.session_1);
        out.datagrams
            .push(self.connection_1(&[Message::new(0, data)]));
    }

    fn mtu_probe(&self) -> Bytes {
        let frame = Frame {
            seq: self.channel_1,
            ack: self.them.as_ref().map_or(0, |t| t.channel_1),
            source: 0,
            destination: 0,
            flags: flags::CONNECTION | flags::CHANNEL_1 | flags::MTU_PROBE,
            payload: Bytes::from(vec![0; MTU_PROBE]),
        };
        Frame::encode_datagram(&[(frame, random_bytes())])
    }

    /// A frame on the connection's channel 1 (unreliable: the console's first sequence number acknowledged).
    fn connection_1(&mut self, messages: &[Message]) -> Bytes {
        let ack = self.them.as_ref().map_or(0, |t| t.channel_1);
        let flags = flags::CONNECTION | flags::CHANNEL_1;
        let seq = advance(&mut self.channel_1, messages);
        datagram(
            seq,
            ack,
            CONNECTION_NONCE,
            CONNECTION_NONCE,
            flags,
            messages,
        )
    }

    /// A frame on the connection's channel 0 (reliable: the console's next sequence number acknowledged).
    fn connection_0(&mut self, messages: &[Message]) -> Bytes {
        let ack = self.them.as_ref().map_or(0, |t| t.channel_0);
        let flags = flags::CONNECTION | flags::CHANNEL_0;
        let seq = advance(&mut self.channel_0, messages);
        datagram(
            seq,
            ack,
            CONNECTION_NONCE,
            CONNECTION_NONCE,
            flags,
            messages,
        )
    }

    /// A frame on the session's channel 0 (reliable), between the two session nonces.
    fn session(&mut self, messages: &[Message]) -> Bytes {
        let (ack, them) = self
            .them
            .as_ref()
            .map_or((0, 0), |t| (t.session_0, t.nonce));
        let flags = flags::SESSION | flags::CHANNEL_0;
        let seq = advance(&mut self.session_0, messages);
        datagram(seq, ack, self.nonce, them, flags, messages)
    }
}

/// The sequence number a frame carries; the channel's next one moves on by its payload's length.
fn advance(channel: &mut u16, messages: &[Message]) -> u16 {
    let seq = *channel;
    *channel = channel.wrapping_add(Message::encode_all(messages).len() as u16);
    seq
}

fn datagram(
    seq: u16,
    ack: u16,
    source: u32,
    destination: u32,
    flags: u8,
    messages: &[Message],
) -> Bytes {
    let frame = Frame {
        seq,
        ack,
        source,
        destination,
        flags,
        payload: Message::encode_all(messages),
    };
    Frame::encode_datagram(&[(frame, random_bytes())])
}

fn gt2(kind: u8, serial: u16, expected: u16, body: &[u8]) -> Bytes {
    Gt2 {
        kind,
        serial,
        expected,
        body: Bytes::copy_from_slice(body),
    }
    .encode()
}

/// Our response to a challenge: its two free bytes printable, as the game draws them.
fn response(challenge: &[u8]) -> [u8; 32] {
    let challenge: [u8; 32] = challenge[..32].try_into().expect("32 bytes");
    let free = random_bytes::<2>().map(|b| 33 + b % 93);
    p2p::gt2_response(&challenge, free)
}

/// Whether `response` answers our `challenge` (its free bytes are its own).
fn answers(challenge: &[u8; 32], response: &[u8]) -> bool {
    p2p::gt2_response(challenge, [response[0], response[13]])[..] == response[..32]
}

/// A hello's fields: the console's channels, then its record's session nonce and session channel 0.
fn parse_hello(data: &[u8]) -> Option<Them> {
    if data.len() < 31 {
        return None;
    }
    let mut r = &data[1..];
    let (channel_0, channel_1) = (r.get_u16(), r.get_u16());
    r.advance(4 + 4 + 10);
    Some(Them {
        channel_0,
        channel_1,
        nonce: r.get_u32(),
        session_0: r.get_u16(),
    })
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    openssl::rand::rand_bytes(&mut b).expect("OpenSSL random source");
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::body;

    const CONSOLE: u32 = 0xeca5_160b;
    /// The recorded console's hello (s04, dev1 to dev2): channels cbed and cb98, session nonce 0001b118, its session's
    /// channel 0 at c399.
    const HELLO_DATA: &str = "81cbedcb980000001603000000494d4a4a4f42000000000001b118c399a15e";

    fn me() -> Me {
        Me {
            capcom_id: "0BOT01".into(),
            peer_id: 0x7beb_835e,
            profile: *b"\x7b\xeb\x83\x5e\xc0\xa8\x6e\x96\0\0\x20\x0a\0\0\0\x01\0\x01\0\x01",
            index: 1,
        }
    }

    /// A datagram from the console: one frame of `messages`.
    fn console(seq: u16, ack: u16, nonces: (u32, u32), flags: u8, messages: &[&[u8]]) -> Bytes {
        let messages: Vec<Message> = messages
            .iter()
            .map(|m| Message::new(0, m.to_vec()))
            .collect();
        datagram(seq, ack, nonces.0, nonces.1, flags, &messages)
    }

    fn frames(out: &Out) -> Vec<Frame> {
        out.datagrams
            .iter()
            .filter(|d| !Gt2::is_gt2(d))
            .flat_map(|d| Frame::decode_datagram(d).unwrap())
            .collect()
    }

    /// The first byte of each message the bot sent.
    fn kinds(out: &Out) -> Vec<u8> {
        frames(out)
            .iter()
            .filter(|f| f.flags & flags::MTU_PROBE == 0)
            .flat_map(|f| Message::decode_all(&f.payload).unwrap())
            .map(|m| m.data[0])
            .collect()
    }

    fn hex(s: &str) -> Vec<u8> {
        ::hex::decode(s).unwrap()
    }

    #[test]
    fn the_bot_connects_as_the_to_side() {
        let t0 = Instant::now();
        let mut link = Link::new(me(), Role::Client, CONSOLE, t0);

        // GT2: our challenge, valid by the game's rule.
        let out = link.start(t0);
        let challenge = Gt2::decode(&out.datagrams[0]).unwrap();
        assert_eq!(
            (challenge.kind, challenge.serial, challenge.expected),
            (1, 0, 0)
        );
        let ours: [u8; 32] = challenge.body[..].try_into().unwrap();
        assert!(p2p::gt2_valid_challenge(&ours));
        // The console's answer (its challenge recorded): our response, then the profile.
        let theirs = hex("792e67765c3d3b30292f39725d39783e5d422d24367c2e6e306543317c69302631627e335c6d4c494e635c6776432c5a787d243e2c7a327976413e237d415e5f")[32..].to_vec();
        let mut answer = p2p::gt2_response(&ours, *b"y.").to_vec();
        answer.extend_from_slice(&theirs);
        let out = link.receive(&gt2(2, 0, 1, &answer), t0);
        let response = Gt2::decode(&out.datagrams[0]).unwrap();
        assert_eq!(
            (response.kind, response.serial, response.expected),
            (3, 1, 1)
        );
        assert!(answers(
            &theirs[..].try_into().unwrap(),
            &response.body[..32]
        ));
        assert_eq!(&response.body[32..], &me().profile);
        // A wrong response to our challenge: nothing.
        assert!(
            link.receive(&gt2(2, 0, 1, &[b'!'; 64]), t0)
                .datagrams
                .is_empty()
        );

        // Accepted: acknowledged, the pair reported, the hello sent.
        let out = link.receive(&hex("fefe0400010002"), t0);
        assert_eq!(&out.datagrams[0][..], hex("fefe640002"));
        assert_eq!(
            out.say,
            vec![Say::Report {
                from: CONSOLE,
                to: me().peer_id
            }]
        );
        assert_eq!(kinds(&out), vec![HELLO]);
        let hello = &frames(&out)[0];
        let data = &Message::decode_all(&hello.payload).unwrap()[0].data;
        assert_eq!(&data[9..19], b"\x03\0\0\x000BOT01");

        // Its hello: answered (0x82), its first channel 1 number acknowledged.
        let mut data = hex(HELLO_DATA);
        let out = link.receive(&console(0xcb98, 0, (1, 1), 0x12, &[&data]), t0);
        assert_eq!(kinds(&out), vec![HELLO_ANSWER]);
        assert_eq!(frames(&out)[0].ack, 0xcb98);
        // Its answer: 0x83 on channel 0, the MTU probe, the first ping and keepalive; kind 5 for the console.
        data[0] = HELLO_ANSWER;
        let out = link.receive(&console(0xcbba, 0, (1, 1), 0x12, &[&data]), t0);
        assert_eq!(out.say, vec![Say::Linked]);
        let sent = frames(&out);
        assert_eq!((sent[0].flags, sent[0].ack), (0x11, 0xcbed));
        assert_eq!(sent[1].payload.len(), MTU_PROBE);
        assert_eq!(kinds(&out), vec![ESTABLISHED, PING, SESSION_KEEPALIVE]);
        let session = &sent[3];
        assert_eq!(
            (session.flags, session.destination, session.ack),
            (0x21, 0x0001_b118, 0xc399)
        );
        assert!(!link.up());

        // Its 0x83: acknowledged on channel 0 (its next number); the session is up.
        let out = link.receive(
            &console(0xcbed, sent[0].seq, (1, 1), 0x11, &[&[ESTABLISHED]]),
            t0,
        );
        let ack = &frames(&out)[0];
        assert_eq!((ack.flags, ack.ack, ack.payload.len()), (0x11, 0xcbf1, 0));
        assert!(link.up());

        // Its ping, echoed; its MTU probe, answered; its session keepalive, acknowledged.
        let out = link.receive(
            &console(0xcbdc, 0, (1, 1), 0x12, &[&hex("910100027485")]),
            t0,
        );
        let pong = &Message::decode_all(&frames(&out)[0].payload).unwrap()[0].data;
        assert_eq!(&pong[..], hex("920100027485"));
        let probe = Frame {
            seq: 0xcbdc,
            ack: 0,
            source: 0,
            destination: 0,
            flags: 0x16,
            payload: Bytes::from(vec![0; MTU_PROBE]),
        };
        let out = link.receive(&Frame::encode_datagram(&[(probe, [1, 2])]), t0);
        let mtu = &Message::decode_all(&frames(&out)[0].payload).unwrap()[0].data;
        assert_eq!(&mtu[..], hex("9500000400"));
        let out = link.receive(&console(0xc399, 0, (0x0001_b118, 0), 0x21, &[&[6]]), t0);
        let ack = &frames(&out)[0];
        assert_eq!((ack.flags, ack.ack, ack.payload.len()), (0x21, 0xc39d, 0));

        // The departure's roll call: answered with our index (recorded: 00010d0a00000003 to 00000d0a00000003).
        let call = datagram(
            0xc39d,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(message::GAME, hex("00000d0a00000003"))],
        );
        let out = link.receive(&call, t0);
        let answer = frames(&out)
            .iter()
            .flat_map(|f| Message::decode_all(&f.payload).unwrap())
            .find(|m| m.flags == message::GAME)
            .unwrap();
        assert_eq!(&answer.data[..], hex("00010d0a00000003"));

        // Every second: a ping and a keepalive; 20 s of silence ends the link.
        assert!(
            link.tick(t0 + Duration::from_millis(500))
                .unwrap()
                .datagrams
                .is_empty()
        );
        let out = link.tick(t0 + Duration::from_millis(1100)).unwrap();
        assert_eq!(kinds(&out), vec![PING, SESSION_KEEPALIVE]);

        // The quest's start: the host's spawn state (recorded) answered with ours, then our position twice a second.
        let spawn =
            hex("00000105c48020004383800044200000000000000000ac1700000000010000000000000000010000");
        let call = datagram(
            0xc3a5,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(message::GAME, spawn)],
        );
        let game = |out: &Out| -> Vec<Bytes> {
            frames(out)
                .iter()
                .flat_map(|f| Message::decode_all(&f.payload).unwrap())
                .filter(|m| m.flags == message::GAME)
                .map(|m| m.data)
                .collect()
        };
        let t1 = t0 + Duration::from_millis(1200);
        assert_eq!(
            game(&link.receive(&call, t1)),
            vec![Bytes::from(hex(
                "00010205c48020004383800044200000000000000000ac1700000000010000000000000000010000"
            ))]
        );
        assert_eq!(
            game(&link.tick(t1 + Duration::from_millis(600)).unwrap()),
            vec![Bytes::from(hex(
                "00010202c48020004383800044200000000000000000ac17000000000100006400640258006400000000000000000000"
            ))]
        );
        // A hunter's position (recorded, the joiner's 0202 in area 0), for the brain; the bot moved by it.
        let theirs = datagram(
            0xc3d5,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(
                message::GAME,
                hex(
                    "00010202c48da615438380004428451a000000000000ac17000000000101006400640258006400000000000000000000",
                ),
            )],
        );
        let out = link.receive(&theirs, t1);
        let (position, area) = out.hunter.unwrap();
        assert_eq!((position[1], area), (263.0, 1));
        // A hunter's full state (recorded: dev2 starting a walk at the camp) reaches the brain too.
        let walk = datagram(
            0xc401,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(
                message::GAME,
                hex(
                    "0001020100000000c411be64c36dee75c38f381e000000000000b55600000000020000010001a379006400640258000002000000427000000100000000640000000000000000000000000000",
                ),
            )],
        );
        let (position, area) = link.receive(&walk, t1).hunter.unwrap();
        assert_eq!((position[0].round(), area), (-583.0, 0));

        // A large monster handed to the bot (recorded, the lab's A4 run: owner slot 1, area 1): reported, not
        // answered (the task keeps it); one handed to someone else: not reported.
        let handover = hex(
            "000005020000001cc4379226c2aa157a44dcae38000000000000fc820000010100000000000000000000000000000000",
        );
        let given = datagram(
            0xc4f0,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(message::GAME, handover.clone())],
        );
        assert_eq!(link.receive(&given, t1).given, vec![handover.clone()]);
        let mut elsewhere = handover.clone();
        elsewhere[OWNER_AT] = 2;
        let other = datagram(
            0xc540,
            0,
            0x0001_b118,
            0,
            0x21,
            &[Message::new(message::GAME, elsewhere)],
        );
        assert!(link.receive(&other, t1).given.is_empty());

        // Sent into that area by the brain: it enters there (`body.rs`), with the map byte that area's hunters send.
        link.enter(1);
        let datagrams = link.go([1.0, 2.0, 3.0], false, t1);
        let entry = &game(&Out {
            datagrams,
            ..Out::default()
        })[0];
        assert_eq!(&entry[2..4], [2, 3]);
        assert_eq!(&entry[4..16], hex("3f8000004000000040400000"));
        assert_eq!((entry[MAP_AT], entry[body::AREA_AT]), (1, 1));

        assert!(link.tick(t1 + Duration::from_secs(21)).is_none());
    }

    #[test]
    fn the_bot_answers_as_the_listening_side() {
        let t0 = Instant::now();
        let mut link = Link::new(me(), Role::Listener, CONSOLE, t0);
        assert!(link.start(t0).datagrams.is_empty());
        // The console's recorded challenge: our response and our challenge.
        let theirs = hex("5a43794f374a635d5e3d71414b5e7c59656b72555f293b5c7e753e743a3c5362");
        let out = link.receive(&gt2(1, 0, 0, &theirs), t0);
        let answer = Gt2::decode(&out.datagrams[0]).unwrap();
        assert_eq!((answer.kind, answer.serial, answer.expected), (2, 0, 1));
        assert!(answers(&theirs[..].try_into().unwrap(), &answer.body[..32]));
        let ours: [u8; 32] = answer.body[32..64].try_into().unwrap();
        assert!(p2p::gt2_valid_challenge(&ours));
        // A wrong response: nothing. The right one: accepted, and our hello.
        assert!(
            link.receive(&gt2(3, 1, 1, &[b'!'; 52]), t0)
                .datagrams
                .is_empty()
        );
        let mut response = p2p::gt2_response(&ours, *b"lN").to_vec();
        response.extend_from_slice(&me().profile);
        let out = link.receive(&gt2(3, 1, 1, &response), t0);
        assert_eq!(kinds(&out), vec![HELLO]);
        assert!(out.datagrams.iter().any(|d| d[..] == hex("fefe0400010002")));
        assert!(out.say.is_empty());
        // The console closing: the link ends.
        link.receive(&hex("fefe68"), t0);
        assert!(link.tick(t0).is_none());
    }

    #[test]
    fn a_console_that_never_answers_ends_the_link() {
        let t0 = Instant::now();
        let mut link = Link::new(me(), Role::Client, CONSOLE, t0);
        link.start(t0);
        let mut ended = None;
        for s in 1..=20 {
            if link.tick(t0 + Duration::from_secs(s)).is_none() {
                ended = Some(s);
                break;
            }
        }
        // 15 challenges, a second apart.
        assert_eq!(ended, Some(15));
    }
}
