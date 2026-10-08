//! The world: world servers (game constants, D2 of M1), how consoles find them, and where players are.

use std::collections::HashMap;

use anyhow::anyhow;
use bytes::Bytes;
use wire::{Item, OptionalFields, PatData};

use super::{Fault, PROFILE_SIZE};
use crate::Shared;
use crate::text::{Language, Text, text};

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
                .with(
                    0x08,
                    Item::Long(shared.world.lock().unwrap().server_population(index)),
                )
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

/// Gates per world server, and how many players a gate holds.
pub const GATES: u16 = 40;
pub const GATE_CAPACITY: u32 = 100;

/// Where the players are (D3 of M2). Only players are stored: a world server or a gate exists while someone is in
/// it, populations and lists are counted from the players.
#[derive(Default)]
pub struct World {
    places: HashMap<String, Place>,
    /// Orders players by arrival (lists show the earliest first).
    arrivals: u64,
}

/// Where a player is, and what others see of them.
#[derive(Clone, Debug)]
pub struct Place {
    /// 1-based world server.
    pub server: u32,
    pub gate: Option<u16>,
    pub name: Bytes,
    pub profile: Box<[u8; PROFILE_SIZE]>,
    /// The connection that placed the player (a newer login replaces the place, the older one mustn't remove it).
    conn: u64,
    arrival: u64,
}

impl World {
    /// The player enters world server `server` (its top level).
    pub fn enter(
        &mut self,
        capcom_id: &str,
        conn: u64,
        server: u32,
        name: Bytes,
        profile: Box<[u8; PROFILE_SIZE]>,
    ) {
        self.arrivals += 1;
        let place = Place {
            server,
            gate: None,
            name,
            profile,
            conn,
            arrival: self.arrivals,
        };
        self.places.insert(capcom_id.to_owned(), place);
    }

    /// The player leaves the world, unless a newer login placed it since.
    pub fn leave(&mut self, capcom_id: &str, conn: u64) {
        if self.places.get(capcom_id).is_some_and(|p| p.conn == conn) {
            self.places.remove(capcom_id);
        }
    }

    pub fn place(&mut self, capcom_id: &str) -> Option<&mut Place> {
        self.places.get_mut(capcom_id)
    }

    /// The players where `capcom_id` is (same world server and gate), the earliest first.
    pub fn neighbours(&self, capcom_id: &str) -> Vec<(&str, &Place)> {
        let Some(here) = self.places.get(capcom_id) else {
            return Vec::new();
        };
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == here.server && p.gate == here.gate)
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    pub fn server_population(&self, server: u32) -> u32 {
        self.places.values().filter(|p| p.server == server).count() as u32
    }

    pub fn gate_population(&self, server: u32, gate: u16) -> u32 {
        self.places
            .values()
            .filter(|p| p.server == server && p.gate == Some(gate))
            .count() as u32
    }
}

/// The gates of world server `server`, `count` from `first` (1-based), with their optional fields (none).
pub fn gates(world: &World, server: u32, first: u32, count: u32) -> Vec<(PatData, OptionalFields)> {
    (first.max(1)..first.saturating_add(count).min(u32::from(GATES) + 1))
        .map(|index| {
            let gate = index as u16;
            let population = world.gate_population(server, gate);
            // 0 joinable, 1 empty, 2 full.
            let state = match population {
                0 => 1,
                p if p >= GATE_CAPACITY => 2,
                _ => 0,
            };
            let name = text(Text::GateName(gate), Language::default());
            let record = PatData::new()
                .with(0x01, Item::Long(index))
                .with(0x05, Item::Word(gate))
                // Position updates, every 500 ms.
                .with(0x11, Item::Long(500))
                .with(0x12, Item::Byte(1))
                .with(0x03, Item::String(Bytes::copy_from_slice(name.as_bytes())))
                .with(0x06, Item::Long(population))
                .with(0x07, Item::Long(population))
                .with(0x09, Item::Long(GATE_CAPACITY))
                .with(0x10, Item::Byte(state))
                // Depth: gates are layer 2.
                .with(0x15, Item::Byte(2));
            (record, OptionalFields::default())
        })
        .collect()
}
