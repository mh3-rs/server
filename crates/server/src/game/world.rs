//! The world: world servers (game constants, D2 of M1), and how consoles find them.

use anyhow::anyhow;
use bytes::Bytes;
use wire::{Item, PatData};

use super::Fault;
use crate::Shared;

/// The world servers (MH3SP's): name and type (1 open, 2 rookie, 3 expert, 4 recruiting).
pub const SERVERS: [(&str, u64); 12] = [
    ("Valor1", 1),
    ("Valor2", 1),
    ("Valor3", 1),
    ("Valor4", 1),
    ("Beginners1", 2),
    ("Beginners2", 2),
    ("Veterans1", 3),
    ("Veterans2", 3),
    ("Greed1", 4),
    ("Greed2", 4),
    ("Greed3", 4),
    ("Greed4", 4),
];
pub const SERVER_CAPACITY: u32 = 2000;
pub const LIST_VERSION: u32 = 1;

/// The world server list (`AnsFmpListData`, `AnsFmpListData2`): `count` servers from `first` (1-based).
pub fn list(shared: &Shared, first: u32, count: u32) -> Result<Vec<PatData>, Fault> {
    let address = external_address(shared)?;
    Ok(SERVERS
        .iter()
        .zip(1u32..)
        .skip(first.saturating_sub(1) as usize)
        .take(count as usize)
        .map(|(&(name, kind), index)| {
            PatData::new()
                .with(0x01, Item::Long(index))
                .with(0x02, Item::String(address.clone()))
                .with(0x03, Item::Word(shared.fmp_port))
                .with(0x07, Item::LongLong(kind))
                // Populations come with the world (M2).
                .with(0x08, Item::Long(0))
                .with(0x09, Item::Long(SERVER_CAPACITY))
                .with(0x0a, Item::String(Bytes::from_static(name.as_bytes())))
                // Unknown; MH3SP's values.
                .with(0x0b, Item::String(Bytes::from_static(b"X")))
                .with(0x0c, Item::Long(0x1234_5678))
        })
        .collect())
}

/// Where world server `index` is (`AnsFmpInfo`, `AnsFmpInfo2`): the `fields` asked, address (0x02) and port (0x03).
pub fn fmp_info(shared: &Shared, index: u32, fields: &[u8]) -> Result<PatData, Fault> {
    if !(1..=SERVERS.len() as u32).contains(&index) {
        return Err(Fault::Client(anyhow!("no world server {index}")));
    }
    let address = external_address(shared)?;
    let mut data = PatData::new();
    for &field in fields {
        data = match field {
            0x02 => data.with(field, Item::String(address.clone())),
            0x03 => data.with(field, Item::Word(shared.fmp_port)),
            _ => return Err(Fault::Client(anyhow!("FMP info field {field:#x} unknown"))),
        };
    }
    Ok(data)
}

fn external_address(shared: &Shared) -> Result<Bytes, Fault> {
    let address = shared
        .config
        .external_address
        .as_ref()
        .ok_or_else(|| Fault::Server(anyhow!("EXTERNAL_ADDRESS unset")))?;
    Ok(Bytes::copy_from_slice(address.as_bytes()))
}
