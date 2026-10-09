//! GameSpy NAT negotiation (UDP 27901) and the QR availability check (UDP 27900), as the game speaks them.
//!
//! A NatNeg record: the magic, version 3, the record type, the session cookie (both consoles of a pair compute it:
//! the XOR of their PAT peer ids), then the type's body. Only the types the game sends or expects (lab and decomp,
//! M4): `INIT`, `INIT_ACK`, `CONNECT`, `CONNECT_ACK`, `CONNECT_PING` (console to console), `REPORT`, `REPORT_ACK`.

use std::net::{Ipv4Addr, SocketAddrV4};

use bytes::{BufMut, Bytes, BytesMut};

use crate::{DecodeError, Reader};

pub const MAGIC: [u8; 6] = [0xfd, 0xfc, 0x1e, 0x66, 0x6a, 0xb2];
const VERSION: u8 = 3;
/// What follows `INIT_ACK`'s port type and client index (GameSpy's own servers', as AltWFC sends it).
const INIT_ACK_TAIL: [u8; 7] = [0xff, 0xff, 0x6d, 0x16, 0xb5, 0x7d, 0xea];
/// `REPORT`'s game name field.
const REPORT_GAME_SIZE: usize = 50;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NatNegError {
    #[error("not a NatNeg record")]
    Magic,
    #[error("NatNeg version {0}")]
    Version(u8),
    #[error("NatNeg record type {0:#04x}")]
    Type(u8),
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub cookie: u32,
    pub body: Body,
}

/// Which socket a console's record comes from, and its role in the pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port {
    /// 0: the game socket to natneg1; 1: the NatNeg socket to natneg1; 2, 3: the NatNeg socket to natneg2, natneg3
    /// (0x80 and 0x90 in later records).
    pub kind: u8,
    /// 0 or 1: one console of the pair each.
    pub client_index: u8,
}

/// Where to reach the peer (`CONNECT` from the server, `CONNECT_PING` between consoles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub addr: SocketAddrV4,
    /// The server's constant 0x42; between consoles 0 or 1 ("I've heard from you").
    pub got_your_data: u8,
    pub finished: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Init {
        port: Port,
        use_game_port: u8,
        /// The console's own (private) address and port.
        local: SocketAddrV4,
        game: Bytes,
    },
    InitAck(Port),
    Connect(Peer),
    /// Unknown trailing bytes (they vary).
    ConnectAck(Port, [u8; 7]),
    Ping(Peer),
    Report {
        port: Port,
        /// 1: the consoles reached each other.
        result: u8,
        nat_type: u32,
        mapping: u32,
        game: Bytes,
    },
    /// The report's first bytes echoed, result cleared (unknown trailing bytes).
    ReportAck(Port, [u8; 7]),
}

impl Body {
    fn kind(&self) -> u8 {
        match self {
            Body::Init { .. } => 0x00,
            Body::InitAck(_) => 0x01,
            Body::Connect(_) => 0x05,
            Body::ConnectAck(..) => 0x06,
            Body::Ping(_) => 0x07,
            Body::Report { .. } => 0x0d,
            Body::ReportAck(..) => 0x0e,
        }
    }
}

impl Record {
    pub fn decode(datagram: &[u8]) -> Result<Record, NatNegError> {
        let mut r = Reader::new(datagram);
        if r.bytes(MAGIC.len()).ok() != Some(&MAGIC[..]) {
            return Err(NatNegError::Magic);
        }
        match r.u8()? {
            VERSION => {}
            v => return Err(NatNegError::Version(v)),
        }
        let kind = r.u8()?;
        let cookie = r.u32()?;
        let port = |r: &mut Reader| -> Result<Port, DecodeError> {
            Ok(Port {
                kind: r.u8()?,
                client_index: r.u8()?,
            })
        };
        let addr = |r: &mut Reader| -> Result<SocketAddrV4, DecodeError> {
            let ip = Ipv4Addr::from(r.u32()?);
            Ok(SocketAddrV4::new(ip, r.u16()?))
        };
        let tail = |r: &mut Reader| -> Result<[u8; 7], DecodeError> {
            Ok(r.bytes(7)?.try_into().expect("7 bytes"))
        };
        let body = match kind {
            0x00 => {
                let port = port(&mut r)?;
                let use_game_port = r.u8()?;
                let local = addr(&mut r)?;
                let game = r.rest();
                let end = game.iter().position(|&b| b == 0).unwrap_or(game.len());
                let name = Bytes::copy_from_slice(&game[..end]);
                r.bytes(game.len().min(end + 1))?;
                Body::Init {
                    port,
                    use_game_port,
                    local,
                    game: name,
                }
            }
            0x01 => {
                let port = port(&mut r)?;
                tail(&mut r)?;
                Body::InitAck(port)
            }
            0x05 | 0x07 => {
                let peer = Peer {
                    addr: addr(&mut r)?,
                    got_your_data: r.u8()?,
                    finished: r.u8()?,
                };
                if kind == 0x05 {
                    Body::Connect(peer)
                } else {
                    Body::Ping(peer)
                }
            }
            0x06 => Body::ConnectAck(port(&mut r)?, tail(&mut r)?),
            0x0d => {
                let port = port(&mut r)?;
                let result = r.u8()?;
                let nat_type = r.u32()?;
                let mapping = r.u32()?;
                let game = r.bytes(REPORT_GAME_SIZE)?;
                let end = game.iter().position(|&b| b == 0).unwrap_or(game.len());
                Body::Report {
                    port,
                    result,
                    nat_type,
                    mapping,
                    game: Bytes::copy_from_slice(&game[..end]),
                }
            }
            0x0e => Body::ReportAck(port(&mut r)?, tail(&mut r)?),
            other => return Err(NatNegError::Type(other)),
        };
        r.finish()?;
        Ok(Record { cookie, body })
    }

    pub fn encode(&self) -> Bytes {
        let mut b = BytesMut::new();
        b.put_slice(&MAGIC);
        b.put_u8(VERSION);
        b.put_u8(self.body.kind());
        b.put_u32(self.cookie);
        let put_port = |b: &mut BytesMut, p: &Port| b.put_slice(&[p.kind, p.client_index]);
        let put_addr = |b: &mut BytesMut, a: &SocketAddrV4| {
            b.put_slice(&a.ip().octets());
            b.put_u16(a.port());
        };
        match &self.body {
            Body::Init {
                port,
                use_game_port,
                local,
                game,
            } => {
                put_port(&mut b, port);
                b.put_u8(*use_game_port);
                put_addr(&mut b, local);
                b.put_slice(game);
                b.put_u8(0);
            }
            Body::InitAck(port) => {
                put_port(&mut b, port);
                b.put_slice(&INIT_ACK_TAIL);
            }
            Body::Connect(peer) | Body::Ping(peer) => {
                put_addr(&mut b, &peer.addr);
                b.put_slice(&[peer.got_your_data, peer.finished]);
            }
            Body::ConnectAck(port, tail) | Body::ReportAck(port, tail) => {
                put_port(&mut b, port);
                b.put_slice(tail);
            }
            Body::Report {
                port,
                result,
                nat_type,
                mapping,
                game,
            } => {
                put_port(&mut b, port);
                b.put_u8(*result);
                b.put_u32(*nat_type);
                b.put_u32(*mapping);
                let n = game.len().min(REPORT_GAME_SIZE - 1);
                b.put_slice(&game[..n]);
                b.put_bytes(0, REPORT_GAME_SIZE - n);
            }
        }
        b.freeze()
    }
}

/// The QR availability check: `09`, four zero bytes, the game name; `None` if `datagram` isn't one.
pub fn availability_request(datagram: &[u8]) -> Option<&[u8]> {
    let name = datagram.strip_prefix(&[0x09, 0, 0, 0, 0][..])?;
    name.strip_suffix(&[0][..])
}

/// The answer: the game is available.
pub const AVAILABLE: [u8; 7] = [0xfe, 0xfd, 0x09, 0, 0, 0, 0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        for hex in [
            "fdfc1e666ab203005a8f90df010001c0a86e9600006d6833757377696900",
            "fdfc1e666ab203015a8f90df0200ffff6d16b57dea",
            "fdfc1e666ab203055a8f90df7f0000014af64200",
            "fdfc1e666ab203065a8f90df8000b968804d44ac80",
            "fdfc1e666ab203075a8f90df7f00000157bb0100",
            "fdfc1e666ab2030e5a8f90df800100000000060000",
        ] {
            let datagram = hex::decode(hex).unwrap();
            let record = Record::decode(&datagram).unwrap();
            assert_eq!(record.cookie, 0x5a8f90df);
            assert_eq!(&record.encode()[..], &datagram[..], "{hex}");
        }
        assert_eq!(Record::decode(b"hello"), Err(NatNegError::Magic));
        assert!(
            availability_request(&hex::decode("09000000006d6833757377696900").unwrap())
                == Some(&b"mh3uswii"[..])
        );
    }
}
