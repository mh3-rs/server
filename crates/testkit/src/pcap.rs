//! Minimal reader/writer for the pcap files Dolphin writes (`DumpAsPCAP`): classic
//! little-endian pcap, microsecond timestamps, Ethernet + IPv4 + TCP/UDP.

use std::net::{Ipv4Addr, SocketAddrV4};

use anyhow::{Context, Result, bail, ensure};

const GLOBAL_HEADER_LEN: usize = 24;
const RECORD_HEADER_LEN: usize = 16;
const MAGIC_LE_MICROS: u32 = 0xa1b2_c3d4;
const LINKTYPE_ETHERNET: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// A TCP or UDP packet carrying data. Empty segments (handshakes, ACKs) are skipped.
#[derive(Debug)]
pub struct Packet<'a> {
    /// Microseconds since the Unix epoch.
    pub ts_us: u64,
    pub proto: Proto,
    pub src: SocketAddrV4,
    pub dst: SocketAddrV4,
    pub payload: &'a [u8],
    /// The whole pcap record (header included), to write it back unchanged.
    pub record: &'a [u8],
}

pub struct Capture<'a> {
    /// The pcap global header, to write a filtered copy with the same format.
    pub header: &'a [u8],
    pub packets: Vec<Packet<'a>>,
}

pub fn parse(data: &[u8]) -> Result<Capture<'_>> {
    ensure!(data.len() >= GLOBAL_HEADER_LEN, "truncated pcap header");
    let magic = u32::from_le_bytes(data[0..4].try_into()?);
    ensure!(
        magic == MAGIC_LE_MICROS,
        "unsupported pcap magic {magic:#010x}"
    );
    let linktype = u32::from_le_bytes(data[20..24].try_into()?);
    ensure!(
        linktype == LINKTYPE_ETHERNET,
        "unsupported link type {linktype}"
    );

    let mut packets = Vec::new();
    let mut off = GLOBAL_HEADER_LEN;
    while off < data.len() {
        let header = data
            .get(off..off + RECORD_HEADER_LEN)
            .context("truncated pcap record header")?;
        let le = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
        let (secs, micros, incl_len) = (le(0), le(4), le(8) as usize);
        let end = off + RECORD_HEADER_LEN + incl_len;
        let record = data.get(off..end).context("truncated pcap record")?;
        let ts_us = u64::from(secs) * 1_000_000 + u64::from(micros);
        if let Some(packet) =
            parse_ethernet(ts_us, record).with_context(|| format!("bad packet at offset {off}"))?
        {
            packets.push(packet);
        }
        off = end;
    }
    Ok(Capture {
        header: &data[..GLOBAL_HEADER_LEN],
        packets,
    })
}

fn parse_ethernet(ts_us: u64, record: &[u8]) -> Result<Option<Packet<'_>>> {
    let frame = &record[RECORD_HEADER_LEN..];
    ensure!(frame.len() >= 14, "truncated Ethernet header");
    if frame[12..14] != [0x08, 0x00] {
        return Ok(None); // not IPv4
    }
    let ip = &frame[14..];
    ensure!(ip.len() >= 20 && ip[0] >> 4 == 4, "bad IPv4 header");
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    let ip = ip.get(..total).context("truncated IPv4 packet")?;
    let src = Ipv4Addr::from(<[u8; 4]>::try_from(&ip[12..16])?);
    let dst = Ipv4Addr::from(<[u8; 4]>::try_from(&ip[16..20])?);
    let l4 = ip.get(ihl..).context("bad IPv4 header length")?;
    ensure!(l4.len() >= 8, "truncated transport header");
    let ports = (
        u16::from_be_bytes([l4[0], l4[1]]),
        u16::from_be_bytes([l4[2], l4[3]]),
    );
    let (proto, payload) = match ip[9] {
        6 => {
            let data_off = usize::from(l4.get(12).context("truncated TCP header")? >> 4) * 4;
            (
                Proto::Tcp,
                l4.get(data_off..).context("bad TCP data offset")?,
            )
        }
        17 => (Proto::Udp, &l4[8..]),
        other => bail!("unexpected IP protocol {other}"),
    };
    if payload.is_empty() {
        return Ok(None);
    }
    Ok(Some(Packet {
        ts_us,
        proto,
        src: SocketAddrV4::new(src, ports.0),
        dst: SocketAddrV4::new(dst, ports.1),
        payload,
        record,
    }))
}

/// Writes a pcap containing only the given packets, byte for byte as they were read.
pub fn write<'a>(header: &[u8], packets: impl IntoIterator<Item = &'a Packet<'a>>) -> Vec<u8> {
    let mut out = header.to_vec();
    for p in packets {
        out.extend_from_slice(p.record);
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a pcap with one Ethernet/IPv4 record per `(ts_us, proto, src, dst, payload)`.
    pub(crate) fn build(packets: &[(u64, Proto, &str, &str, &[u8])]) -> Vec<u8> {
        let mut out = MAGIC_LE_MICROS.to_le_bytes().to_vec();
        out.extend_from_slice(&[2, 0, 4, 0]); // version 2.4
        out.extend_from_slice(&[0; 8]); // timezone, accuracy
        out.extend_from_slice(&65535u32.to_le_bytes());
        out.extend_from_slice(&LINKTYPE_ETHERNET.to_le_bytes());
        for &(ts, proto, src, dst, payload) in packets {
            let src: SocketAddrV4 = src.parse().unwrap();
            let dst: SocketAddrV4 = dst.parse().unwrap();
            let l4_len = if proto == Proto::Tcp { 20 } else { 8 };
            let total = (20 + l4_len + payload.len()) as u16;
            let mut frame = vec![0u8; 12];
            frame.extend_from_slice(&[0x08, 0x00, 0x45, 0]);
            frame.extend_from_slice(&total.to_be_bytes());
            frame.extend_from_slice(&[
                0,
                0,
                0,
                0,
                64,
                if proto == Proto::Tcp { 6 } else { 17 },
                0,
                0,
            ]);
            frame.extend_from_slice(&src.ip().octets());
            frame.extend_from_slice(&dst.ip().octets());
            frame.extend_from_slice(&src.port().to_be_bytes());
            frame.extend_from_slice(&dst.port().to_be_bytes());
            if proto == Proto::Tcp {
                frame.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0x50, 0x18, 0, 0, 0, 0, 0, 0]);
            } else {
                frame.extend_from_slice(&(8 + payload.len() as u16).to_be_bytes());
                frame.extend_from_slice(&[0, 0]);
            }
            frame.extend_from_slice(payload);
            let len = frame.len() as u32;
            for v in [(ts / 1_000_000) as u32, (ts % 1_000_000) as u32, len, len] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&frame);
        }
        out
    }

    #[test]
    fn parses_tcp_and_udp_and_skips_empty_segments() {
        let data = build(&[
            (
                1_500_000,
                Proto::Tcp,
                "10.0.0.2:5000",
                "10.0.0.1:8200",
                b"hello",
            ),
            (1_600_000, Proto::Tcp, "10.0.0.1:8200", "10.0.0.2:5000", b""),
            (
                1_700_000,
                Proto::Udp,
                "0.0.0.0:21769",
                "127.0.0.1:27901",
                b"natneg",
            ),
        ]);
        let capture = parse(&data).unwrap();
        assert_eq!(capture.packets.len(), 2);
        let tcp = &capture.packets[0];
        assert_eq!(
            (tcp.proto, tcp.ts_us, tcp.payload),
            (Proto::Tcp, 1_500_000, &b"hello"[..])
        );
        assert_eq!(tcp.dst.port(), 8200);
        assert_eq!(capture.packets[1].proto, Proto::Udp);
        assert_eq!(capture.packets[1].payload, b"natneg");
    }

    #[test]
    fn filtered_copy_parses_back_identically() {
        let data = build(&[
            (1, Proto::Tcp, "10.0.0.2:5000", "10.0.0.1:80", b"POST /ac"),
            (2, Proto::Tcp, "10.0.0.2:5001", "10.0.0.1:8201", b"frame"),
        ]);
        let capture = parse(&data).unwrap();
        let kept = write(
            capture.header,
            capture.packets.iter().filter(|p| p.dst.port() == 8201),
        );
        let again = parse(&kept).unwrap();
        assert_eq!(again.packets.len(), 1);
        assert_eq!(again.packets[0].payload, b"frame");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"not a pcap").is_err());
        assert!(parse(&[0u8; 24]).is_err());
    }
}
