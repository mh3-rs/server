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

/// Gates per world server, cities per gate, and how many players each holds.
pub const GATES: u16 = 40;
pub const GATE_CAPACITY: u32 = 100;
pub const CITIES: u16 = 40;
pub const CITY_CAPACITY: u32 = 4;

/// Where the players are, and the cities that exist (D3 of M2). Resources exist on demand: a world server or a
/// gate is just where players are; a city exists from its reservation until its last player leaves. Populations
/// and lists are counted from the players.
#[derive(Default)]
pub struct World {
    places: HashMap<String, Place>,
    cities: HashMap<CityId, City>,
    /// Orders players by arrival (lists show the earliest first).
    arrivals: u64,
}

/// World server, gate, city (1-based).
pub type CityId = (u32, u16, u16);

/// Where a player is, and what others see of them.
#[derive(Clone, Debug)]
pub struct Place {
    /// 1-based world server.
    pub server: u32,
    pub gate: Option<u16>,
    pub city: Option<u16>,
    pub name: Bytes,
    pub profile: Box<[u8; PROFILE_SIZE]>,
    /// The connection that placed the player (a newer login replaces the place, the older one mustn't remove it).
    conn: u64,
    arrival: u64,
}

#[derive(Debug)]
pub struct City {
    pub leader: String,
    /// Between `LayerCreateHead` and `LayerCreateFoot`: the city shows as full.
    pub reserved_by: Option<String>,
    /// The city's settings (`LayerCreateSet`): shown in the city list.
    pub settings: OptionalFields,
}

/// Why a city can't be created or entered.
#[derive(Debug, PartialEq, Eq)]
pub enum CityRefusal {
    Exists,
    Missing,
    Full,
}

impl CityRefusal {
    pub fn text(&self) -> Text {
        match self {
            CityRefusal::Exists => Text::CityExists,
            CityRefusal::Missing => Text::CityMissing,
            CityRefusal::Full => Text::CityFull,
        }
    }
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
            city: None,
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
            self.leave_city(capcom_id);
            self.places.remove(capcom_id);
        }
    }

    pub fn place(&mut self, capcom_id: &str) -> Option<&mut Place> {
        self.places.get_mut(capcom_id)
    }

    /// The players where `capcom_id` is (same world server, gate and city), the earliest first.
    pub fn neighbours(&self, capcom_id: &str) -> Vec<(&str, &Place)> {
        let Some(here) = self.places.get(capcom_id) else {
            return Vec::new();
        };
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == here.server && p.gate == here.gate && p.city == here.city)
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    pub fn server_population(&self, server: u32) -> u32 {
        self.count(|p| p.server == server)
    }

    pub fn gate_population(&self, server: u32, gate: u16) -> u32 {
        self.count(|p| p.server == server && p.gate == Some(gate))
    }

    pub fn city_population(&self, (server, gate, city): CityId) -> u32 {
        self.count(|p| p.server == server && p.gate == Some(gate) && p.city == Some(city))
    }

    fn count(&self, f: impl Fn(&Place) -> bool) -> u32 {
        self.places.values().filter(|p| f(p)).count() as u32
    }

    /// Reserves a city for `capcom_id` to create (`LayerCreateHead`).
    pub fn reserve_city(&mut self, id: CityId, capcom_id: &str) -> Result<(), CityRefusal> {
        if self.cities.contains_key(&id) {
            return Err(CityRefusal::Exists);
        }
        let city = City {
            leader: capcom_id.to_owned(),
            reserved_by: Some(capcom_id.to_owned()),
            settings: OptionalFields::default(),
        };
        self.cities.insert(id, city);
        Ok(())
    }

    /// Creates the reserved city and puts its creator in it (`LayerCreateSet`).
    pub fn create_city(
        &mut self,
        id: CityId,
        capcom_id: &str,
        settings: OptionalFields,
    ) -> Result<(), CityRefusal> {
        match self.cities.get_mut(&id) {
            Some(city) if city.reserved_by.as_deref() == Some(capcom_id) => {
                city.settings = settings
            }
            Some(_) => return Err(CityRefusal::Exists),
            None => return Err(CityRefusal::Missing),
        }
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.city = Some(id.2);
        }
        Ok(())
    }

    /// Ends a reservation (`LayerCreateFoot`).
    pub fn release_city(&mut self, id: CityId, capcom_id: &str) {
        if let Some(city) = self.cities.get_mut(&id)
            && city.reserved_by.as_deref() == Some(capcom_id)
        {
            city.reserved_by = None;
        }
        self.tidy(id);
    }

    /// Puts the player in an existing city of its gate (`LayerDown` from a gate).
    pub fn join_city(&mut self, id: CityId, capcom_id: &str) -> Result<(), CityRefusal> {
        let Some(city) = self.cities.get(&id) else {
            return Err(CityRefusal::Missing);
        };
        if city.reserved_by.is_some() || self.city_population(id) >= CITY_CAPACITY {
            return Err(CityRefusal::Full);
        }
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.city = Some(id.2);
        }
        Ok(())
    }

    /// The player leaves its city, if in one, or drops its reservation: an empty city goes, a leader leaving hands
    /// the city to the earliest player left.
    pub fn leave_city(&mut self, capcom_id: &str) {
        let Some(place) = self.places.get_mut(capcom_id) else {
            return;
        };
        let (server, gate) = (place.server, place.gate);
        let left = place.city.take();
        for (&id, city) in self.cities.iter_mut() {
            if city.reserved_by.as_deref() == Some(capcom_id) {
                city.reserved_by = None;
            }
            if Some(id.2) == left
                && (id.0, Some(id.1)) == (server, gate)
                && city.leader == capcom_id
            {
                city.leader.clear();
            }
        }
        let ids: Vec<CityId> = self.cities.keys().copied().collect();
        for id in ids {
            self.tidy(id);
        }
    }

    /// Removes a city nobody is in or about to create; gives a leaderless city to its earliest player.
    fn tidy(&mut self, id: CityId) {
        let mut members: Vec<(&String, &Place)> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == id.0 && p.gate == Some(id.1) && p.city == Some(id.2))
            .collect();
        members.sort_by_key(|(_, p)| p.arrival);
        let first = members.first().map(|(cid, _)| (*cid).clone());
        let Some(city) = self.cities.get_mut(&id) else {
            return;
        };
        match first {
            None if city.reserved_by.is_none() => {
                self.cities.remove(&id);
            }
            Some(first) if city.leader.is_empty() => city.leader = first,
            _ => {}
        }
    }
}

/// A layer's state in lists: 1 empty, 2 full (or reserved), 3 joinable.
fn state(population: u32, capacity: u32, reserved: bool) -> u8 {
    match population {
        _ if reserved => 2,
        0 => 1,
        p if p >= capacity => 2,
        _ => 3,
    }
}

/// The gates of world server `server`, `count` from `first` (1-based), with their optional fields (none).
pub fn gates(world: &World, server: u32, first: u32, count: u32) -> Vec<(PatData, OptionalFields)> {
    (first.max(1)..first.saturating_add(count).min(u32::from(GATES) + 1))
        .map(|index| {
            let gate = index as u16;
            let population = world.gate_population(server, gate);
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
                .with(0x10, Item::Byte(state(population, GATE_CAPACITY, false)))
                // Depth: gates are layer 2.
                .with(0x15, Item::Byte(2));
            (record, OptionalFields::default())
        })
        .collect()
}

/// The cities of a gate, `count` from `first` (1-based), with their settings.
pub fn cities(
    world: &World,
    server: u32,
    gate: u16,
    first: u32,
    count: u32,
) -> Vec<(PatData, OptionalFields)> {
    let language = Language::default();
    let server_name = SERVERS[server as usize - 1].0;
    let gate_name = text(Text::GateName(gate), language);
    (first.max(1)..first.saturating_add(count).min(u32::from(CITIES) + 1))
        .map(|index| {
            let id = (server, gate, index as u16);
            let city = world.cities.get(&id);
            let population = world.city_population(id);
            let name = text(Text::CityName(index as u16), language);
            let path = format!("{server_name}\t{gate_name}\t{name}");
            let record = PatData::new()
                .with(0x01, Item::Long(index))
                .with(0x05, Item::Word(index as u16))
                .with(0x11, Item::Long(500))
                .with(0x12, Item::Byte(1))
                .with(0x03, Item::String(Bytes::copy_from_slice(name.as_bytes())))
                .with(0x06, Item::Long(population))
                .with(0x07, Item::Long(population))
                .with(0x09, Item::Long(CITY_CAPACITY))
                // Players of the city on a quest (circles come with quests).
                .with(0x0b, Item::Long(0))
                // Unknown; MH3SP's value.
                .with(0x0c, Item::Long(0xc))
                .with(
                    0x10,
                    Item::Byte(state(
                        population,
                        CITY_CAPACITY,
                        city.is_some_and(|c| c.reserved_by.is_some()),
                    )),
                )
                // Depth: cities are layer 3.
                .with(0x15, Item::Byte(3))
                .with(0x16, Item::String(Bytes::from(path.into_bytes())));
            (record, city.map(|c| c.settings.clone()).unwrap_or_default())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_with(players: &[&str]) -> World {
        let mut world = World::default();
        for (conn, id) in players.iter().enumerate() {
            world.enter(
                id,
                conn as u64,
                1,
                Bytes::new(),
                Box::new([0; PROFILE_SIZE]),
            );
            world.place(id).unwrap().gate = Some(1);
        }
        world
    }

    #[test]
    fn a_city_lives_from_its_reservation_to_its_last_player() {
        let mut world = world_with(&["AAAAAA", "BBBBBB"]);
        let id = (1, 1, 7);
        world.reserve_city(id, "AAAAAA").unwrap();
        assert_eq!(world.reserve_city(id, "BBBBBB"), Err(CityRefusal::Exists));
        // Reserved: shown full, can't be joined.
        assert_eq!(world.join_city(id, "BBBBBB"), Err(CityRefusal::Full));
        world
            .create_city(id, "AAAAAA", OptionalFields::default())
            .unwrap();
        world.release_city(id, "AAAAAA");
        world.join_city(id, "BBBBBB").unwrap();
        assert_eq!(world.city_population(id), 2);
        // The leader leaves: the city stays, led by the other player.
        world.leave_city("AAAAAA");
        assert_eq!(world.cities[&id].leader, "BBBBBB");
        world.leave_city("BBBBBB");
        assert!(world.cities.is_empty());
    }

    #[test]
    fn a_reservation_goes_with_its_player() {
        let mut world = world_with(&["AAAAAA"]);
        world.reserve_city((1, 1, 2), "AAAAAA").unwrap();
        world.leave("AAAAAA", 0);
        assert!(world.cities.is_empty() && world.places.is_empty());
    }
}
