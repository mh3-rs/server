//! Content the consoles download (`ReqBinaryHead`/`Data`/`Foot`), built when asked (decision D2 of M2): a binary's
//! version is derived from its bytes, so a change reaches connected consoles at their next `ReqBinaryVersion`.
//!
//! Types `0x01`–`0x0f` are the English set (the console asks for those, D7).

use std::time::SystemTime;

use crate::text::{Language, Text, text};

/// HR range per server type (Open, Rookie, Expert, Recruiting).
const SERVER_TYPE_HR: [(u16, u16); 4] = [(0, 999), (0, 30), (31, 999), (0, 999)];
/// City HR limits (None, Low, High, then unused ones).
const CITY_HR_LIMITS: [(u16, u16); 8] = [
    (0, 999),
    (0, 30),
    (31, 999),
    (0, 999),
    (0, 999),
    (0, 999),
    (0, 999),
    (0, 999),
];
/// Ticks of a day/night cycle (6.4 hours; the game defaults to 3000 when 0).
pub const TICKS_PER_CYCLE: u32 = 23040;

/// A content binary: its bytes and the version they give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    pub version: u32,
    pub bytes: Vec<u8>,
}

impl Binary {
    fn new(bytes: Vec<u8>) -> Self {
        let hash = openssl::sha::sha256(&bytes);
        Binary {
            version: u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]),
            bytes,
        }
    }
}

/// The binary of type `kind`, or `None` for a type the server doesn't know.
pub fn binary(kind: u8, started: SystemTime, language: Language) -> Option<Binary> {
    let bytes = match kind {
        0x01 => server_types(started, language),
        // Unknown purpose (the game fetches it as file 5): MH3SP's placeholder, which the console accepts.
        0x05 => b"5".repeat(0x10),
        _ => return None,
    };
    Some(Binary::new(bytes))
}

/// `s` in a field of `size` bytes, NUL-padded, the last byte always NUL.
fn pad(out: &mut Vec<u8>, s: &str, size: usize) {
    let start = out.len();
    out.extend_from_slice(&s.as_bytes()[..s.len().min(size - 1)]);
    out.resize(start + size, 0);
}

/// Server types (names, descriptions, HR ranges), city seekings, HR limits and goals, timeouts.
fn server_types(started: SystemTime, language: Language) -> Vec<u8> {
    let mut out = Vec::with_capacity(5068);
    for i in 0..SERVER_TYPE_HR.len() {
        pad(&mut out, &text(Text::ServerTypeName(i), language), 24);
        pad(
            &mut out,
            &text(Text::ServerTypeDescription(i), language),
            168,
        );
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    // The server's start, as the reference of the day/night cycle.
    let started = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    out.extend_from_slice(&started.to_be_bytes());
    out.extend_from_slice(&TICKS_PER_CYCLE.to_be_bytes());
    for i in 0..32 {
        pad(&mut out, &text(Text::CitySeeking(i), language), 48);
        // Unknown flags (MH3SP's values).
        out.extend_from_slice(b"\0\x01\xff\0");
    }
    for (i, (min, max)) in CITY_HR_LIMITS.iter().enumerate() {
        pad(&mut out, &text(Text::CityHrLimit(i), language), 30);
        // Enabled; restricted by HR.
        out.extend_from_slice(&[1, 0]);
        out.extend_from_slice(&min.to_be_bytes());
        out.extend_from_slice(&max.to_be_bytes());
    }
    for i in 0..64 {
        pad(&mut out, &text(Text::CityGoal(i), language), 30);
        // Enabled; no restriction; no HR range.
        out.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
    }
    for (min, max) in SERVER_TYPE_HR {
        out.extend_from_slice(&min.to_be_bytes());
        out.extend_from_slice(&max.to_be_bytes());
    }
    // 8 timeouts, 30 s each, in frames (30 per second).
    for _ in 0..8 {
        out.extend_from_slice(&(30u16 * 30).to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_types_have_the_recorded_size() {
        let b = binary(0x01, SystemTime::now(), Language::English).unwrap();
        assert_eq!(b.bytes.len(), 5068);
        assert_eq!(&b.bytes[..5], b"Open\0");
        assert!(binary(0x7f, SystemTime::now(), Language::English).is_none());
    }
}
