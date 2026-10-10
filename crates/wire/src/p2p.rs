//! The consoles' peer-to-peer traffic (towns and quests), as the quest bots need to speak it
//! (`roadmap/experiment-bots.md`, "How a peer joins a city's session"). Two layers in one UDP flow:
//! - **GameSpy GT2**, only for connecting and closing: `fefe`, a type, then serial numbers (types 1–4) or an expected
//!   serial (the ack, 0x64), or nothing (closed, 0x68). The connect is a challenge-response (`gt2_response`).
//! - **The game's frames**, everything else: a datagram is `[u16 length][frame]` repeated, then `0000`; a frame is a
//!   22-byte header (version 0x0101, payload length, seq, ack, source and destination nonces, `04`, flags, a
//!   CRC-16/CCITT over the plaintext, two key bytes) and a payload under a rolling XOR. The payload holds messages:
//!   a u16 length, a flag byte (0x80 game data; 0x40 a timestamp follows; low 6 bits, a count of values), then the
//!   data.
//!
//! The decomp: `GameSpyInterfaceThread.cpp`, `DWCi/dwc_nasfunc.c`, `NetworkUnitPacket.cpp`, `NetworkStreamSink.cpp`,
//! `NetworkConnectionStable.cpp`.

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Why P2P bytes can't be read.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum P2pError {
    #[error("malformed at byte {0}")]
    Malformed(usize),
    #[error("CRC mismatch")]
    Crc,
}

// ---- GT2 -------------------------------------------------------------------------------------------------------

pub const GT2_CLIENT_CHALLENGE: u8 = 0x01;
pub const GT2_SERVER_CHALLENGE: u8 = 0x02;
pub const GT2_CLIENT_RESPONSE: u8 = 0x03;
pub const GT2_ACCEPT: u8 = 0x04;
pub const GT2_REJECT: u8 = 0x05;
pub const GT2_ACK: u8 = 0x64;
pub const GT2_CLOSED: u8 = 0x68;
const GT2_MAGIC: [u8; 2] = [0xfe, 0xfe];

/// A GT2 control packet: its type, its serial numbers (types 1–7) or expected serial (ack), and what follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gt2 {
    pub kind: u8,
    pub serial: u16,
    pub expected: u16,
    pub body: Bytes,
}

impl Gt2 {
    /// Whether a datagram is GT2's (the rest is the game's frames, or NAT negotiation's `fdfc`).
    pub fn is_gt2(datagram: &[u8]) -> bool {
        datagram.starts_with(&GT2_MAGIC)
    }

    pub fn decode(datagram: &[u8]) -> Result<Self, P2pError> {
        let mut r = datagram;
        if r.len() < 3 || r[..2] != GT2_MAGIC {
            return Err(P2pError::Malformed(0));
        }
        r.advance(2);
        let kind = r.get_u8();
        let (serial, expected) = match kind {
            GT2_CLOSED => (0, 0),
            GT2_ACK => (0, read_u16(&mut r, 3)?),
            _ => (read_u16(&mut r, 3)?, read_u16(&mut r, 5)?),
        };
        Ok(Gt2 {
            kind,
            serial,
            expected,
            body: Bytes::copy_from_slice(r),
        })
    }

    pub fn encode(&self) -> Bytes {
        let mut out = BytesMut::with_capacity(7 + self.body.len());
        out.put_slice(&GT2_MAGIC);
        out.put_u8(self.kind);
        match self.kind {
            GT2_CLOSED => {}
            GT2_ACK => out.put_u16(self.expected),
            _ => {
                out.put_u16(self.serial);
                out.put_u16(self.expected);
            }
        }
        out.put_slice(&self.body);
        out.freeze()
    }
}

fn read_u16(r: &mut &[u8], offset: usize) -> Result<u16, P2pError> {
    r.try_get_u16().map_err(|_| P2pError::Malformed(offset))
}

/// The secret both sides of a GT2 connect hash with (`DWCi/dwc_nasfunc.c`): part of the protocol, not content.
const GT2_SECRET: &[u8; 32] = b"3b8dd8995f7c40a9a5c5b7dd5b481341";

/// The answer to a GT2 challenge (`dwc_nasfunc.c:2500`): 32 printable bytes; 0 and 13 free (the game draws them).
pub fn gt2_response(challenge: &[u8; 32], free: [u8; 2]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        if i == 0 || i == 13 {
            out[i] = free[usize::from(i == 13)];
            continue;
        }
        let c = usize::from(if i == 1 || i == 14 {
            challenge[i]
        } else {
            challenge[i - 1]
        });
        let k = usize::from(GT2_SECRET[(i + usize::from(challenge[i])) % 32]);
        let v =
            challenge[(k + i * usize::from(challenge[i])) % 32] ^ GT2_SECRET[(c * i * 17991) % 32];
        out[i] = v % 93 + 33;
    }
    out
}

/// Whether a challenge follows the game's parity rule (`dwc_nasfunc.c:2481`), which a listener checks.
pub fn gt2_valid_challenge(challenge: &[u8; 32]) -> bool {
    (1..32).all(|i| challenge[i] & 1 == expected_parity(challenge, i))
}

/// The parity byte `i` of a challenge must have, given the bytes before it.
fn expected_parity(challenge: &[u8; 32], i: usize) -> u8 {
    let mut odd = 0u8;
    for j in 1..=i {
        let previous = challenge[j - 1];
        odd = (challenge[0] & 1)
            ^ odd
            ^ ((j as u8 ^ previous) & 1)
            ^ u8::from(challenge[0] < 79)
            ^ u8::from(previous < challenge[0]);
    }
    odd
}

/// A valid challenge from `random` (32 bytes): printable, each byte's parity set by the rule.
pub fn gt2_challenge(random: [u8; 32]) -> [u8; 32] {
    let mut challenge = [0u8; 32];
    for i in 0..32 {
        // 33..=126, then the parity fixed (126 is even, so +1 never leaves the range when odd is needed below 126).
        let mut b = 33 + random[i] % 93;
        if i > 0 && b & 1 != expected_parity(&challenge, i) {
            b = if b < 126 { b + 1 } else { b - 1 };
        }
        challenge[i] = b;
    }
    challenge
}

// ---- The game's frames ------------------------------------------------------------------------------------------

const FRAME_VERSION: u16 = 0x0101;
const FRAME_HEADER: usize = 22;

/// Frame flags (`NetworkUnitPacket.cpp`): the level, the channel, and kinds.
pub mod flags {
    /// The connection's own messages (hello, ping, MTU).
    pub const CONNECTION: u8 = 0x10;
    /// The session's messages (the game's).
    pub const SESSION: u8 = 0x20;
    pub const CHANNEL_0: u8 = 0x01;
    pub const CHANNEL_1: u8 = 0x02;
    pub const MTU_PROBE: u8 = 0x04;
    pub const OUT_OF_BAND: u8 = 0x08;
}

/// A game frame: its header's fields and its (plaintext) payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub seq: u16,
    pub ack: u16,
    pub source: u32,
    pub destination: u32,
    pub flags: u8,
    pub payload: Bytes,
}

impl Frame {
    /// The frames of a datagram.
    pub fn decode_datagram(datagram: &[u8]) -> Result<Vec<Frame>, P2pError> {
        let mut r = datagram;
        let mut frames = Vec::new();
        loop {
            let at = datagram.len() - r.len();
            let len = usize::from(read_u16(&mut r, at)?);
            if len == 0 {
                return Ok(frames);
            }
            if r.len() < len || len < FRAME_HEADER {
                return Err(P2pError::Malformed(at));
            }
            frames.push(Frame::decode(&r[..len])?);
            r.advance(len);
        }
    }

    fn decode(frame: &[u8]) -> Result<Frame, P2pError> {
        let mut h = frame;
        let version = h.get_u16();
        let len = usize::from(h.get_u16());
        let (seq, ack, source, destination) = (h.get_u16(), h.get_u16(), h.get_u32(), h.get_u32());
        let _ = h.get_u8(); // 04
        let flags = h.get_u8();
        let crc = h.get_u16();
        let keys = [h.get_u8(), h.get_u8()];
        if version != FRAME_VERSION || frame.len() != FRAME_HEADER + len {
            return Err(P2pError::Malformed(0));
        }
        let payload = xor(&frame[FRAME_HEADER..], keys, false);
        let mut zeroed = frame[..FRAME_HEADER].to_vec();
        zeroed[18..22].fill(0);
        if crc16(&[&zeroed[..], &payload[..]]) != crc {
            return Err(P2pError::Crc);
        }
        Ok(Frame {
            seq,
            ack,
            source,
            destination,
            flags,
            payload: payload.into(),
        })
    }

    /// The frame on the wire, its payload under the XOR with `keys` (the game draws them at random).
    pub fn encode(&self, keys: [u8; 2]) -> Bytes {
        let mut out = BytesMut::with_capacity(FRAME_HEADER + self.payload.len());
        out.put_u16(FRAME_VERSION);
        out.put_u16(self.payload.len() as u16);
        out.put_u16(self.seq);
        out.put_u16(self.ack);
        out.put_u32(self.source);
        out.put_u32(self.destination);
        out.put_u8(0x04);
        out.put_u8(self.flags);
        out.put_u16(0); // the CRC, below
        out.put_u16(0);
        let crc = crc16(&[&out[..], &self.payload[..]]);
        out[18..20].copy_from_slice(&crc.to_be_bytes());
        out[20..22].copy_from_slice(&keys);
        out.put_slice(&xor(&self.payload, keys, true));
        out.freeze()
    }

    /// A datagram of frames: each with its length, then the closing `0000`.
    pub fn encode_datagram(frames: &[(Frame, [u8; 2])]) -> Bytes {
        let mut out = BytesMut::new();
        for (frame, keys) in frames {
            let bytes = frame.encode(*keys);
            out.put_u16(bytes.len() as u16);
            out.put_slice(&bytes);
        }
        out.put_u16(0);
        out.freeze()
    }
}

/// The rolling XOR (`NetworkStreamSink.cpp`): the key is the first key byte inverted, or the second inverted when that
/// is odd; each byte is XORed with the previous plaintext byte.
fn xor(data: &[u8], keys: [u8; 2], encrypt: bool) -> Vec<u8> {
    let mut k = !keys[0];
    if k & 1 == 1 {
        k = !keys[1];
    }
    data.iter()
        .map(|&b| {
            let out = b ^ k;
            k = if encrypt { b } else { out };
            out
        })
        .collect()
}

/// CRC-16/CCITT (polynomial 0x1021, seed 0xffff), over several slices in a row.
fn crc16(parts: &[&[u8]]) -> u16 {
    let mut crc = 0xffffu16;
    for &byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ---- Messages ---------------------------------------------------------------------------------------------------

/// A message in a frame's payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub flags: u8,
    pub timestamp: Option<i32>,
    pub values: Vec<u32>,
    pub data: Bytes,
}

/// Message flags.
pub mod message {
    /// The game's data (a session message).
    pub const GAME: u8 = 0x80;
    pub const TIMESTAMP: u8 = 0x40;
    pub const COUNT: u8 = 0x3f;
}

impl Message {
    /// A message of bare data with `flags`.
    pub fn new(flags: u8, data: impl Into<Bytes>) -> Self {
        Message {
            flags,
            timestamp: None,
            values: Vec::new(),
            data: data.into(),
        }
    }

    pub fn decode_all(payload: &[u8]) -> Result<Vec<Message>, P2pError> {
        let mut r = payload;
        let mut out = Vec::new();
        while !r.is_empty() {
            let at = payload.len() - r.len();
            let bad = || P2pError::Malformed(at);
            let len = usize::from(r.try_get_u16().map_err(|_| bad())?);
            let flags = r.try_get_u8().map_err(|_| bad())?;
            let timestamp = match flags & message::TIMESTAMP {
                0 => None,
                _ => Some(r.try_get_i32().map_err(|_| bad())?),
            };
            let count = usize::from(flags & message::COUNT);
            let values = match count {
                0 => Vec::new(),
                n => (0..=n)
                    .map(|_| r.try_get_u32().map_err(|_| bad()))
                    .collect::<Result<_, _>>()?,
            };
            if r.len() < len {
                return Err(bad());
            }
            out.push(Message {
                flags,
                timestamp,
                values,
                data: Bytes::copy_from_slice(&r[..len]),
            });
            r.advance(len);
        }
        Ok(out)
    }

    pub fn encode_all(messages: &[Message]) -> Bytes {
        let mut out = BytesMut::new();
        for m in messages {
            out.put_u16(m.data.len() as u16);
            out.put_u8(m.flags);
            if let Some(ts) = m.timestamp {
                out.put_i32(ts);
            }
            for v in &m.values {
                out.put_u32(*v);
            }
            out.put_slice(&m.data);
        }
        out.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes32(text: &str) -> [u8; 32] {
        hex::decode(text).unwrap().try_into().unwrap()
    }

    #[test]
    fn crc_is_ccitt() {
        assert_eq!(crc16(&[b"1234", b"56789"]), 0x29b1);
    }

    /// The two GT2 connects recorded in `s04-city-join`: the challenges are valid, and our responses match the
    /// consoles' (but for the two free bytes).
    #[test]
    fn gt2_responses_match_the_consoles() {
        let server = hex::decode("792e67765c3d3b30292f39725d39783e5d422d24367c2e6e306543317c69302631627e335c6d4c494e635c6776432c5a787d243e2c7a327976413e237d415e5f").unwrap();
        let client_challenge =
            bytes32("5a43794f374a635d5e3d71414b5e7c59656b72555f293b5c7e753e743a3c5362");
        let (to_client, server_challenge): (&[u8], [u8; 32]) =
            (&server[..32], server[32..].try_into().unwrap());
        let to_server = bytes32("6c4e2f46725f23263a5a3f6d263469636a3a70592768277369382775396d3a3c");
        for (challenge, recorded) in [
            (client_challenge, to_client),
            (server_challenge, &to_server[..]),
        ] {
            assert!(gt2_valid_challenge(&challenge));
            let ours = gt2_response(&challenge, [recorded[0], recorded[13]]);
            assert_eq!(&ours[..], recorded);
        }
    }

    #[test]
    fn our_challenges_are_valid() {
        for seed in 0..=255u8 {
            let random: [u8; 32] =
                std::array::from_fn(|i| seed.wrapping_mul(31).wrapping_add(i as u8 * 7));
            let challenge = gt2_challenge(random);
            assert!(gt2_valid_challenge(&challenge), "{challenge:?}");
            assert!(challenge.iter().all(|b| (33..=126).contains(b)));
        }
    }

    #[test]
    fn gt2_packets_round_trip() {
        for raw in ["fefe0400010002", "fefe640002", "fefe68"] {
            let raw = hex::decode(raw).unwrap();
            assert_eq!(Gt2::decode(&raw).unwrap().encode(), raw);
        }
    }

    /// dev2's hello and a ping from `s04-city-join`: decoded (CRC checked), re-encoded byte for byte.
    #[test]
    fn recorded_frames_round_trip() {
        let hello = "003801010022279200000000000100000001041256436000ff1f1f8184ae8cb5920000161503000030630605060553000000000145145c1eedc30000";
        let ping = "001f0101000927d6cb9800000001000000010412ae3011fcee06069191000113940000";
        for (raw, data) in [
            (
                hello,
                "8105ab2792000000160300000030535550565300000000000144500c12ff3c",
            ),
            (ping, "910000011286"),
        ] {
            let raw = hex::decode(raw).unwrap();
            let frames = Frame::decode_datagram(&raw).unwrap();
            assert_eq!(frames.len(), 1);
            let frame = &frames[0];
            assert_eq!(frame.flags, flags::CONNECTION | flags::CHANNEL_1);
            let messages = Message::decode_all(&frame.payload).unwrap();
            assert_eq!(hex::encode(&messages[0].data), data);
            assert_eq!(Message::encode_all(&messages), frame.payload);
            let keys = [raw[2 + 20], raw[2 + 21]];
            assert_eq!(Frame::encode_datagram(&[(frame.clone(), keys)]), raw);
        }
    }

    #[test]
    fn a_damaged_frame_is_refused() {
        let mut raw =
            hex::decode("001f0101000927d6cb9800000001000000010412ae3011fcee06069191000113940000")
                .unwrap();
        raw[30] ^= 1;
        assert!(Frame::decode_datagram(&raw).is_err());
    }
}
