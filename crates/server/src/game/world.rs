//! The world: world servers (game constants, D2 of M1), how consoles find them, and where players are.

use std::collections::HashMap;

use anyhow::anyhow;
use bytes::Bytes;
use wire::{Item, OptionalFields, PatData};

use super::Fault;
use super::profile::Profile;
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
/// Players per quest at most. Quests per city: a player is in one quest at most and a quest ends with its leader,
/// so a city never has more quests than players.
pub const CIRCLE_CAPACITY: u32 = 4;
pub const CIRCLES: u32 = CITY_CAPACITY;

/// Where the players are, and the cities that exist (D3 of M2). Resources exist on demand: a world server or a
/// gate is just where players are; a city exists from its reservation until its last player leaves. Populations
/// and lists are counted from the players.
#[derive(Default)]
pub struct World {
    places: HashMap<String, Place>,
    cities: HashMap<CityId, City>,
    circles: HashMap<CircleId, Circle>,
    /// Orders players by arrival (lists show the earliest first).
    arrivals: u64,
}

/// World server, gate, city (1-based).
pub type CityId = (u32, u16, u16);
/// A city, and a circle in it (1-based).
pub type CircleId = (CityId, u32);

/// Where a player is, and what others see of them.
#[derive(Clone, Debug)]
pub struct Place {
    /// 1-based world server.
    pub server: u32,
    pub gate: Option<u16>,
    pub city: Option<u16>,
    pub circle: Option<u32>,
    pub name: Bytes,
    pub profile: Profile,
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

/// A quest being prepared or played: from its creation until its leader leaves.
#[derive(Debug)]
pub struct Circle {
    pub leader: String,
    /// Player slots (a player's index is its slot + 1).
    slots: Vec<Option<String>>,
    /// The password's argon2 hash (PHC string), until departure (M3 D9): the password itself is never kept.
    password_hash: Option<String>,
    /// Whether the quest was created with a password: lists keep showing it after the hash is dropped.
    has_password: bool,
    remarks: Option<Bytes>,
    /// Unknown (from `CircleCreate`).
    unk_0e: u8,
    quest_id: u32,
    /// The quest started: no one joins any more.
    departed: bool,
    /// Ready to depart (`CircleMatchOptionSet`).
    standby: Vec<String>,
    /// On the quest.
    in_quest: Vec<String>,
}

/// What `CircleCreate` asks for.
pub struct NewCircle {
    pub password_hash: Option<String>,
    pub remarks: Option<Bytes>,
    pub unk_0e: u8,
    pub capacity: u32,
    pub quest_id: u32,
}

impl Circle {
    /// The players, by index (1-based).
    pub fn members(&self) -> impl Iterator<Item = (u8, &str)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_deref().map(|id| (i as u8 + 1, id)))
    }

    pub fn index_of(&self, capcom_id: &str) -> Option<u8> {
        self.members()
            .find(|(_, id)| *id == capcom_id)
            .map(|(i, _)| i)
    }
}

/// A player leaving a circle: its index, the members left, and what happened to the circle.
#[derive(Debug)]
pub struct Leaving {
    pub id: CircleId,
    pub index: u8,
    pub others: Vec<String>,
    /// A departed quest's new leader (index, Capcom ID).
    pub new_leader: Option<(u8, String)>,
    /// The leader left before departure: the party is over (its other members are out of it).
    pub broken: bool,
}

/// Why a quest can't be joined.
#[derive(Debug, PartialEq, Eq)]
pub enum CircleRefusal {
    AlreadyInOne,
    Missing,
    Full,
    Departed,
}

impl CircleRefusal {
    pub fn text(&self) -> Text {
        match self {
            CircleRefusal::AlreadyInOne | CircleRefusal::Missing | CircleRefusal::Departed => {
                Text::QuestGone
            }
            CircleRefusal::Full => Text::QuestFull,
        }
    }
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
        profile: Profile,
    ) {
        self.arrivals += 1;
        let place = Place {
            server,
            gate: None,
            city: None,
            circle: None,
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

    /// Whether this connection placed the player (a newer login may have replaced the place).
    pub fn placed_by(&self, capcom_id: &str, conn: u64) -> bool {
        self.places.get(capcom_id).is_some_and(|p| p.conn == conn)
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
        self.leave_circle(capcom_id);
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

    /// The players at the same place as `capcom_id`, without them.
    pub fn others(&self, capcom_id: &str) -> Vec<String> {
        self.neighbours(capcom_id)
            .into_iter()
            .filter(|(id, _)| *id != capcom_id)
            .map(|(id, _)| id.to_owned())
            .collect()
    }

    /// The players at a place given by its parts (the layer search), the earliest first.
    pub fn players_at(
        &self,
        server: u32,
        gate: Option<u16>,
        city: Option<u16>,
    ) -> Vec<(&str, &Place)> {
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == server && p.gate == gate && p.city == city)
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    /// The players under a place (a world server: everyone on it; a gate: its cities too), the earliest first.
    pub fn players_under(
        &self,
        server: u32,
        gate: Option<u16>,
        city: Option<u16>,
    ) -> Vec<(&str, &Place)> {
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| {
                p.server == server
                    && gate.is_none_or(|g| p.gate == Some(g))
                    && city.is_none_or(|c| p.city == Some(c))
            })
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    /// The host of a city (its leader), while they're in it.
    pub fn city_host(&self, id: CityId) -> Option<&str> {
        let leader = self.cities.get(&id)?.leader.as_str();
        self.city_players(id)
            .iter()
            .any(|p| p == leader)
            .then_some(leader)
    }

    /// Where the player's city is, if in one.
    pub fn city_of(&self, capcom_id: &str) -> Option<CityId> {
        let p = self.places.get(capcom_id)?;
        Some((p.server, p.gate?, p.city?))
    }

    /// Everyone in a city, the earliest first.
    pub fn city_players(&self, (server, gate, city): CityId) -> Vec<String> {
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == server && p.gate == Some(gate) && p.city == Some(city))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list.into_iter().map(|(id, _)| id.clone()).collect()
    }

    /// Creates a circle in the player's city, in its first free slot, and puts the player in it as leader. `None` for
    /// a player outside a city or already in a quest (one quest at a time).
    pub fn create_circle(&mut self, capcom_id: &str, new: NewCircle) -> Option<CircleId> {
        let city = self.city_of(capcom_id)?;
        if self.places.get(capcom_id)?.circle.is_some() {
            return None;
        }
        let index = (1..=CIRCLES).find(|i| !self.circles.contains_key(&(city, *i)))?;
        let mut slots = vec![None; new.capacity.clamp(1, CIRCLE_CAPACITY) as usize];
        slots[0] = Some(capcom_id.to_owned());
        let circle = Circle {
            leader: capcom_id.to_owned(),
            slots,
            has_password: new.password_hash.is_some(),
            password_hash: new.password_hash,
            remarks: new.remarks,
            unk_0e: new.unk_0e,
            quest_id: new.quest_id,
            departed: false,
            standby: Vec::new(),
            in_quest: Vec::new(),
        };
        self.circles.insert((city, index), circle);
        self.places.get_mut(capcom_id).expect("in a city").circle = Some(index);
        Some((city, index))
    }

    pub fn circle(&self, id: CircleId) -> Option<&Circle> {
        self.circles.get(&id)
    }

    /// The circle the player is in.
    pub fn circle_of(&self, capcom_id: &str) -> Option<CircleId> {
        let city = self.city_of(capcom_id)?;
        Some((city, self.places.get(capcom_id)?.circle?))
    }

    pub fn set_standby(&mut self, capcom_id: &str, standby: bool) {
        if let Some(circle) = self
            .circle_of(capcom_id)
            .and_then(|id| self.circles.get_mut(&id))
        {
            circle.standby.retain(|id| id != capcom_id);
            if standby {
                circle.standby.push(capcom_id.to_owned());
            }
        }
    }

    /// The quest departs: the players ready go on it (index and Capcom ID).
    pub fn depart(&mut self, id: CircleId) -> Vec<(u8, String)> {
        let Some(circle) = self.circles.get_mut(&id) else {
            return Vec::new();
        };
        circle.departed = true;
        // Nobody joins a departed quest: its password goes.
        circle.password_hash = None;
        let ready: Vec<(u8, String)> = circle
            .members()
            .filter(|(_, m)| circle.standby.iter().any(|s| s == m))
            .map(|(i, m)| (i, m.to_owned()))
            .collect();
        circle.in_quest = ready.iter().map(|(_, m)| m.clone()).collect();
        ready
    }

    /// Whether the player is out hunting: in a quest that departed with them.
    pub fn on_quest(&self, capcom_id: &str) -> bool {
        self.circle_of(capcom_id)
            .and_then(|id| self.circles.get(&id))
            .is_some_and(|c| c.in_quest.iter().any(|m| m == capcom_id))
    }

    /// A quest's members: index, Capcom ID, name, ready or not.
    pub fn circle_members(&self, id: CircleId) -> Vec<(u8, String, Bytes, bool)> {
        let Some(circle) = self.circles.get(&id) else {
            return Vec::new();
        };
        circle
            .members()
            .map(|(i, m)| {
                let name = self
                    .places
                    .get(m)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                (i, m.to_owned(), name, circle.standby.iter().any(|s| s == m))
            })
            .collect()
    }

    /// The password hash of a quest that has one.
    pub fn circle_password(&self, id: CircleId) -> Option<String> {
        self.circles.get(&id)?.password_hash.clone()
    }

    /// Puts the player in a quest of its city (the password checked already): its index in the party.
    pub fn join_circle(&mut self, capcom_id: &str, id: CircleId) -> Result<u8, CircleRefusal> {
        if self.circle_of(capcom_id).is_some() {
            return Err(CircleRefusal::AlreadyInOne);
        }
        let circle = self.circles.get_mut(&id).ok_or(CircleRefusal::Missing)?;
        if circle.departed {
            return Err(CircleRefusal::Departed);
        }
        let slot = circle
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(CircleRefusal::Full)?;
        circle.slots[slot] = Some(capcom_id.to_owned());
        self.places.get_mut(capcom_id).expect("in a city").circle = Some(id.1);
        Ok(slot as u8 + 1)
    }

    /// The player leaves its circle. A leader leaving hands a departed quest to the next member, and breaks one not
    /// departed yet (MH3SP's rules, M3 D7).
    pub fn leave_circle(&mut self, capcom_id: &str) -> Option<Leaving> {
        let id = self.circle_of(capcom_id)?;
        self.places.get_mut(capcom_id).expect("in a circle").circle = None;
        let circle = self.circles.get_mut(&id)?;
        let index = circle.index_of(capcom_id).unwrap_or(0);
        for slot in circle
            .slots
            .iter_mut()
            .filter(|s| s.as_deref() == Some(capcom_id))
        {
            *slot = None;
        }
        circle.standby.retain(|m| m != capcom_id);
        circle.in_quest.retain(|m| m != capcom_id);
        let others: Vec<String> = circle.members().map(|(_, m)| m.to_owned()).collect();
        let mut leaving = Leaving {
            id,
            index,
            others: others.clone(),
            new_leader: None,
            broken: false,
        };
        if circle.leader == capcom_id {
            let next = circle.members().next().map(|(i, m)| (i, m.to_owned()));
            match (circle.departed, next) {
                (true, Some((i, m))) => {
                    circle.leader = m.clone();
                    leaving.new_leader = Some((i, m));
                }
                _ => {
                    self.circles.remove(&id);
                    for m in &others {
                        if let Some(p) = self.places.get_mut(m) {
                            p.circle = None;
                        }
                    }
                    leaving.broken = true;
                }
            }
        } else if others.is_empty() {
            self.circles.remove(&id);
        }
        Some(leaving)
    }

    /// A circle as lists show it (`CircleInfo`): an empty slot shows its index only.
    pub fn circle_info(&self, id: CircleId) -> (PatData, OptionalFields) {
        let index = id.1;
        let mut info = PatData::new().with(0x01, Item::Long(index));
        let Some(circle) = self.circles.get(&id) else {
            return (
                info,
                OptionalFields(vec![(1, Some(CIRCLE_CAPACITY)), (2, Some(0))]),
            );
        };
        if circle.has_password {
            // The flag, and a placeholder where MH3SP put the password itself: it never leaves the server (M3 D9;
            // whether the console needs field 0x04 at all is to test).
            info = info
                .with(0x03, Item::Byte(1))
                .with(0x04, Item::String(Bytes::from_static(b"****")));
        }
        // Per player: Capcom ID at 0x10·i, name at 0x40 + 0x10·i, weapon type at 0x80 + i, a flag at 0x84 + i.
        let mut party = vec![0u8; 0x88];
        for (index, member) in circle.members() {
            let i = usize::from(index - 1);
            let place = self.places.get(member);
            let name = place.map_or(&[][..], |p| &p.name[..]);
            for (offset, bytes) in [(0x10 * i, member.as_bytes()), (0x40 + 0x10 * i, name)] {
                let n = bytes.len().min(0x0f);
                party[offset..offset + n].copy_from_slice(&bytes[..n]);
            }
            party[0x80 + i] = place.map_or(0, |p| p.profile.weapon);
            party[0x84 + i] = 0xff;
        }
        info = info.with(0x05, Item::Binary(party.into()));
        if let Some(remarks) = &circle.remarks {
            info = info.with(0x06, Item::String(remarks.clone()));
        }
        let capacity = circle.slots.len() as u32;
        let count = circle.members().count() as u32;
        let info = info
            .with(0x07, Item::Long(count))
            .with(0x09, Item::Long(capacity))
            .with(0x0c, Item::Long(index))
            .with(
                0x0d,
                Item::String(Bytes::copy_from_slice(circle.leader.as_bytes())),
            )
            .with(0x0e, Item::Byte(circle.unk_0e))
            .with(
                0x0f,
                Item::Byte(u8::from(circle.departed || count >= capacity)),
            );
        (
            info,
            OptionalFields(vec![(1, Some(capacity)), (2, Some(circle.quest_id))]),
        )
    }

    /// The circles of a city that exist.
    pub fn circles_in(&self, city: CityId) -> Vec<(PatData, OptionalFields)> {
        (1..=CIRCLES)
            .filter(|i| self.circles.contains_key(&(city, *i)))
            .map(|i| self.circle_info((city, i)))
            .collect()
    }

    fn in_quest(&self, city: CityId) -> u32 {
        self.circles
            .iter()
            .filter(|((c, _), _)| *c == city)
            .map(|(_, circle)| circle.in_quest.len() as u32)
            .sum()
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
/// A layer's path as consoles address it: depth (1 world server, 2 gate, 3 city), world server, 1, gate, city.
pub fn path(server: u32, gate: Option<u16>, city: Option<u16>) -> [u8; 14] {
    let depth: u32 = match (gate, city) {
        (_, Some(_)) => 3,
        (Some(_), None) => 2,
        (None, None) => 1,
    };
    let mut path = [0; 14];
    path[..4].copy_from_slice(&depth.to_be_bytes());
    path[4..8].copy_from_slice(&server.to_be_bytes());
    path[8..10].copy_from_slice(&1u16.to_be_bytes());
    path[10..12].copy_from_slice(&gate.unwrap_or(0).to_be_bytes());
    path[12..14].copy_from_slice(&city.unwrap_or(0).to_be_bytes());
    path
}

/// A path's world server, gate and city (0 parts are absent).
pub fn parse_path(path: &[u8]) -> Option<(u32, Option<u16>, Option<u16>)> {
    if path.len() < 8 {
        return None;
    }
    let server = u32::from_be_bytes(path[4..8].try_into().ok()?);
    let part = |at: usize| {
        path.get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .filter(|v| *v != 0)
    };
    Some((server, part(10), part(12)))
}

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
                // Players of the city on a quest.
                .with(0x0b, Item::Long(world.in_quest(id)))
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
            // An occupied city shows where its host is.
            let record = match world.city_host(id) {
                Some(_) => record.with(
                    0x02,
                    Item::Binary(Bytes::copy_from_slice(&self::path(
                        server,
                        Some(gate),
                        Some(index as u16),
                    ))),
                ),
                None => record,
            };
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
            world.enter(id, conn as u64, 1, Bytes::new(), Profile::default());
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
    fn one_quest_per_player() {
        let mut world = world_with(&["AAAAAA"]);
        world.reserve_city((1, 1, 1), "AAAAAA").unwrap();
        world
            .create_city((1, 1, 1), "AAAAAA", OptionalFields::default())
            .unwrap();
        let new = || NewCircle {
            password_hash: None,
            remarks: None,
            unk_0e: 0,
            capacity: 4,
            quest_id: 1,
        };
        assert!(world.create_circle("AAAAAA", new()).is_some());
        assert!(world.create_circle("AAAAAA", new()).is_none());
        world.leave_circle("AAAAAA");
        assert!(world.create_circle("AAAAAA", new()).is_some());
    }

    #[test]
    fn a_reservation_goes_with_its_player() {
        let mut world = world_with(&["AAAAAA"]);
        world.reserve_city((1, 1, 2), "AAAAAA").unwrap();
        world.leave("AAAAAA", 0);
        assert!(world.cities.is_empty() && world.places.is_empty());
    }
}
