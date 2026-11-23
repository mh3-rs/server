//! The world: world servers (game constants, D2 of M1), how consoles find them, and where players are.

use std::collections::{BTreeMap, HashMap};

use anyhow::anyhow;
use bytes::{Buf, BufMut, Bytes};
use wire::{Item, OptionalFields, PatData};

use super::Fault;
use super::profile::Profile;
use crate::Shared;
use crate::text::{Language, Text, text};

/// How often a console sends its own position in a layer, in milliseconds (the layer record's field `0x11`, read by
/// the console on entering a layer: the decomp's `applyLoginRecord`). MH3SP's value; Capcom's is unknown (M5 D7).
pub const POSITION_INTERVAL: u32 = 500;

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
/// The highest HR of a rookie server, the lowest of an expert one minus one (the types' descriptions, `text.rs`).
const ROOKIE_MAX_HR: u16 = 30;

/// Why a hunter of this HR may not enter world server `index` (1-based): rookie servers take HR 30 or lower, expert
/// ones HR 31 or higher, as their descriptions say. An unknown HR (a hunter never online here yet) is let through.
/// The game checks this itself and never asks (the lab, 2026-10-11): only a modified console meets this refusal,
/// whose alert no console has shown yet.
pub fn hr_refusal(index: u32, hr: Option<u16>) -> Option<Text> {
    let hr = hr?;
    let (_, kind) = SERVERS.get(index.checked_sub(1)? as usize)?;
    match kind {
        2 if hr > ROOKIE_MAX_HR => Some(Text::RookiesOnly),
        3 if hr <= ROOKIE_MAX_HR => Some(Text::ExpertsOnly),
        _ => None,
    }
}
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
                .with(0x08, Item::Long(shared.world().server_population(index)))
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
        .ok_or_else(|| Fault::Server(anyhow!("external_address unset")))?;
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
    /// Quests made so far: each gets the next number (`Circle::number`).
    quests: u64,
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
    /// Where on that world server. Read through `gate`, `city` and `circle`; only the world's moves write it, so a
    /// combination that can't exist (in a quest outside a city) can't be built.
    pub(crate) at: Layer,
    pub name: Bytes,
    pub profile: Profile,
    /// The connection that placed the player (a newer login replaces the place, the older one mustn't remove it).
    conn: u64,
    arrival: u64,
}

/// Where a player is on its world server (the protocol's layers, depth 1 to 3): everything that belongs to one layer
/// is in it, so it goes with the layer when the player moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The world server's top level.
    Root,
    /// In a gate, maybe holding a city's number between `LayerCreateHead` and `LayerCreateSet`: the city shows as full
    /// meanwhile, and the reservation ends with the gate.
    Gate { gate: u16, reserving: Option<u16> },
    /// In a city, maybe in one of its quests.
    City {
        gate: u16,
        city: u16,
        /// A position sent since entering the city (its scene loaded; messages reach the inbox).
        standing: bool,
        party: Option<Member>,
    },
}

/// A player's place in a quest of its city: the only record of who is in it (a quest's members are the players of its
/// city whose membership names it), so leaving the city leaves the quest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    /// The quest's index in its city (1-based).
    pub circle: u32,
    /// The quest's number, to tell it from another made again at the same index.
    pub number: u64,
    /// Its place in the party (0-based: the player's index is its slot + 1).
    pub slot: u8,
    /// Ready to depart (`CircleMatchOptionSet`): only meant while the quest recruits.
    pub ready: bool,
}

impl Place {
    pub fn gate(&self) -> Option<u16> {
        match self.at {
            Layer::Root => None,
            Layer::Gate { gate, .. } | Layer::City { gate, .. } => Some(gate),
        }
    }

    pub fn city(&self) -> Option<u16> {
        match self.at {
            Layer::City { city, .. } => Some(city),
            _ => None,
        }
    }

    pub fn party(&self) -> Option<&Member> {
        match &self.at {
            Layer::City { party, .. } => party.as_ref(),
            _ => None,
        }
    }

    /// The index of the quest the player is in.
    pub fn circle(&self) -> Option<u32> {
        self.party().map(|member| member.circle)
    }

    /// Standing in its city: a position sent since entering it.
    pub fn standing(&self) -> bool {
        matches!(self.at, Layer::City { standing: true, .. })
    }

    /// Sets the membership, if in a city (nowhere else has a quest).
    fn set_party(&mut self, member: Option<Member>) {
        if let Layer::City { party, .. } = &mut self.at {
            *party = member;
        }
    }
}

/// A city that exists: created, and with at least one player in it, its leader among them.
#[derive(Debug)]
pub struct City {
    pub leader: String,
    /// The city's settings (`LayerCreateSet`): shown in the city list.
    pub settings: OptionalFields,
    /// Seats (1–12) and arm wrestling places (13–14) taken (`LayerMediationLock`), and by whom: one player each.
    pub seats: BTreeMap<u8, String>,
}

/// A quest being prepared or played: from its creation until its leader leaves. Its members are not here: they are
/// the players of its city whose membership names it (`Member`).
#[derive(Debug)]
pub struct Circle {
    /// Never reused: a quest made again at the same index is another one.
    number: u64,
    pub leader: String,
    state: Quest,
    /// Whether the quest was created with a password: lists keep showing it after the hash is dropped.
    has_password: bool,
    remarks: Option<Bytes>,
    /// Unknown (from `CircleCreate`).
    unk_0e: u8,
    quest_id: u32,
    /// How many players it takes.
    capacity: u32,
}

/// What a quest is doing, and what only that state has.
#[derive(Debug)]
pub enum Quest {
    /// Being prepared: players join and say they're ready. The password's argon2 hash (PHC string) is kept until
    /// departure (M3 D9): the password itself never is.
    Recruiting { password_hash: Option<String> },
    /// The quest started: no one joins any more, and every member went on it (those not ready were sent away).
    Departed,
}

/// What `CircleCreate` asks for.
pub struct NewCircle {
    pub password_hash: Option<String>,
    pub remarks: Option<Bytes>,
    pub unk_0e: u8,
    pub capacity: u32,
    pub quest_id: u32,
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
    /// Players per world server (1-based), cities, quests (metrics).
    pub fn census(&self) -> (HashMap<u32, usize>, usize, usize) {
        let mut by_server = HashMap::new();
        for place in self.places.values() {
            *by_server.entry(place.server).or_default() += 1;
        }
        (by_server, self.cities.len(), self.circles.len())
    }

    /// The player enters world server `server` (its top level). A place that exists is never replaced (its city, its
    /// quest and its seats would stay behind with nobody to end them): the connection that holds it comes back, to
    /// be left first (`depart`).
    pub fn enter(
        &mut self,
        capcom_id: &str,
        conn: u64,
        server: u32,
        name: Bytes,
        profile: Profile,
    ) -> Result<(), u64> {
        if let Some(held) = self.places.get(capcom_id) {
            return Err(held.conn);
        }
        self.arrivals += 1;
        let place = Place {
            server,
            at: Layer::Root,
            name,
            profile,
            conn,
            arrival: self.arrivals,
        };
        self.places.insert(capcom_id.to_owned(), place);
        Ok(())
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

    /// The player reached another layer: lists show it after those already there. (MH3SP gives each layer fixed
    /// slots and a newcomer the first free one: the same order until someone leaves.)
    pub fn arrive(&mut self, capcom_id: &str) {
        self.arrivals += 1;
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.arrival = self.arrivals;
        }
    }

    /// The players where `capcom_id` is (same world server, gate and city), the earliest first.
    pub fn neighbours(&self, capcom_id: &str) -> Vec<(&str, &Place)> {
        let Some(here) = self.places.get(capcom_id) else {
            return Vec::new();
        };
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| {
                p.server == here.server && p.gate() == here.gate() && p.city() == here.city()
            })
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    pub fn server_population(&self, server: u32) -> u32 {
        self.count(|p| p.server == server)
    }

    pub fn gate_population(&self, server: u32, gate: u16) -> u32 {
        self.count(|p| p.server == server && p.gate() == Some(gate))
    }

    pub fn city_population(&self, (server, gate, city): CityId) -> u32 {
        self.count(|p| p.server == server && p.gate() == Some(gate) && p.city() == Some(city))
    }

    fn count(&self, f: impl Fn(&Place) -> bool) -> u32 {
        self.places.values().filter(|p| f(p)).count() as u32
    }

    /// Whether the player is in gate `gate` of world server `server`, at its level (not in one of its cities).
    fn at_gate(&self, capcom_id: &str, server: u32, gate: u16) -> bool {
        self.places.get(capcom_id).is_some_and(|p| {
            p.server == server && matches!(p.at, Layer::Gate { gate: g, .. } if g == gate)
        })
    }

    /// The player at the gate of `id`, holding its number (`LayerCreateHead`, not yet created).
    fn reserver(&self, (server, gate, city): CityId) -> Option<&str> {
        self.places
            .iter()
            .find(|(_, p)| {
                p.server == server
                    && p.at
                        == Layer::Gate {
                            gate,
                            reserving: Some(city),
                        }
            })
            .map(|(who, _)| who.as_str())
    }

    /// Whether a player is creating the city: it shows as full meanwhile.
    pub fn reserved(&self, id: CityId) -> bool {
        self.reserver(id).is_some()
    }

    /// Reserves a city for `capcom_id`, standing in its gate, to create (`LayerCreateHead`): one number at a time, a
    /// second reservation replaces the first.
    pub fn reserve_city(&mut self, id: CityId, capcom_id: &str) -> Result<(), CityRefusal> {
        if !self.at_gate(capcom_id, id.0, id.1) {
            return Err(CityRefusal::Missing);
        }
        if self.cities.contains_key(&id) || self.reserver(id).is_some_and(|who| who != capcom_id) {
            return Err(CityRefusal::Exists);
        }
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.at = Layer::Gate {
                gate: id.1,
                reserving: Some(id.2),
            };
        }
        Ok(())
    }

    /// Creates the city its creator reserved, and puts it in (`LayerCreateSet`). Only a player in the city's gate,
    /// holding its number, creates one: from anywhere else it is refused.
    pub fn create_city(
        &mut self,
        id: CityId,
        capcom_id: &str,
        settings: OptionalFields,
    ) -> Result<(), CityRefusal> {
        if self.reserver(id) != Some(capcom_id) {
            return Err(match self.cities.contains_key(&id) {
                true => CityRefusal::Exists,
                false => CityRefusal::Missing,
            });
        }
        let city = City {
            leader: capcom_id.to_owned(),
            settings,
            seats: BTreeMap::new(),
        };
        self.cities.insert(id, city);
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.at = Layer::City {
                gate: id.1,
                city: id.2,
                standing: false,
                party: None,
            };
        }
        self.arrive(capcom_id);
        Ok(())
    }

    /// Ends a reservation (`LayerCreateFoot`): the city is created by now, or it never will be.
    pub fn release_city(&mut self, id: CityId, capcom_id: &str) {
        if self.reserver(id) == Some(capcom_id)
            && let Some(place) = self.places.get_mut(capcom_id)
        {
            place.at = Layer::Gate {
                gate: id.1,
                reserving: None,
            };
        }
    }

    /// Whether a player can walk into the city: it exists and has room. A city being created shows as full.
    pub fn can_join(&self, id: CityId) -> Result<(), CityRefusal> {
        if !self.cities.contains_key(&id) {
            return Err(match self.reserved(id) {
                true => CityRefusal::Full,
                false => CityRefusal::Missing,
            });
        }
        if self.city_players(id).len() as u32 >= CITY_CAPACITY {
            return Err(CityRefusal::Full);
        }
        Ok(())
    }

    /// Puts the player, standing in the city's gate, in the city (`LayerDown` from a gate).
    pub fn join_city(&mut self, id: CityId, capcom_id: &str) -> Result<(), CityRefusal> {
        self.can_join(id)?;
        if !self.at_gate(capcom_id, id.0, id.1) {
            return Err(CityRefusal::Missing);
        }
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.at = Layer::City {
                gate: id.1,
                city: id.2,
                standing: false,
                party: None,
            };
        }
        self.arrive(capcom_id);
        Ok(())
    }

    /// The player enters gate `gate` of its world server, from the server's root (`LayerDown`).
    pub fn enter_gate(&mut self, capcom_id: &str, gate: u16) {
        if let Some(place) = self.places.get_mut(capcom_id)
            && place.at == Layer::Root
        {
            place.at = Layer::Gate {
                gate,
                reserving: None,
            };
            self.arrive(capcom_id);
        }
    }

    /// A position came from the player: if in a city, it is standing in it (its scene is loaded). Whether it is.
    pub fn stand(&mut self, capcom_id: &str) -> bool {
        match self.places.get_mut(capcom_id) {
            Some(Place {
                at: Layer::City { standing, .. },
                ..
            }) => {
                *standing = true;
                true
            }
            _ => false,
        }
    }

    /// The player goes up one layer (`LayerUp`): a city to its gate, a gate to the world server's root, which ends a
    /// reservation with the gate.
    pub fn up(&mut self, capcom_id: &str) {
        match self.places.get(capcom_id).map(|p| p.at) {
            Some(Layer::City { .. }) => self.leave_city(capcom_id),
            Some(Layer::Gate { .. }) => {
                if let Some(place) = self.places.get_mut(capcom_id) {
                    place.at = Layer::Root;
                }
                self.arrive(capcom_id);
            }
            _ => {}
        }
    }

    /// The player leaves its city, if in one, for its gate (arriving there): an empty city goes, a leader leaving
    /// hands the city to the earliest player left.
    pub fn leave_city(&mut self, capcom_id: &str) {
        let Some(city) = self.city_of(capcom_id) else {
            return;
        };
        self.leave_circle(capcom_id);
        self.release_seats(capcom_id);
        self.arrivals += 1;
        let arrival = self.arrivals;
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.at = Layer::Gate {
                gate: city.1,
                reserving: None,
            };
            place.arrival = arrival;
        }
        self.tidy(city);
    }

    /// Every player, on every world server, the earliest arrived first.
    pub fn everyone(&self) -> Vec<(&str, &Place)> {
        let mut list: Vec<_> = self.places.iter().map(|(id, p)| (id.as_str(), p)).collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    pub fn city(&self, id: CityId) -> Option<&City> {
        self.cities.get(&id)
    }

    /// Takes seat `slot` in `city` for `capcom_id`: `false` if someone else has it.
    pub fn take_seat(&mut self, city: CityId, slot: u8, capcom_id: &str) -> bool {
        let Some(city) = self.cities.get_mut(&city) else {
            return false;
        };
        match city.seats.get(&slot) {
            Some(holder) if holder != capcom_id => false,
            _ => {
                city.seats.insert(slot, capcom_id.to_owned());
                true
            }
        }
    }

    /// Frees seat `slot` in `city` if `capcom_id` has it: whether it did.
    pub fn free_seat(&mut self, city: CityId, slot: u8, capcom_id: &str) -> bool {
        let seats = self.cities.get_mut(&city).map(|c| &mut c.seats);
        match seats {
            Some(seats) if seats.get(&slot).is_some_and(|h| h == capcom_id) => {
                seats.remove(&slot);
                true
            }
            _ => false,
        }
    }

    /// Frees every seat `capcom_id` has (leaving its city): the slots freed.
    pub fn release_seats(&mut self, capcom_id: &str) -> Vec<u8> {
        let Some(city) = self.city_of(capcom_id) else {
            return Vec::new();
        };
        let Some(city) = self.cities.get_mut(&city) else {
            return Vec::new();
        };
        let mine: Vec<u8> = city
            .seats
            .iter()
            .filter(|(_, h)| *h == capcom_id)
            .map(|(&s, _)| s)
            .collect();
        for slot in &mine {
            city.seats.remove(slot);
        }
        mine
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
            .filter(|(_, p)| p.server == server && p.gate() == gate && p.city() == city)
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
                    && gate.is_none_or(|g| p.gate() == Some(g))
                    && city.is_none_or(|c| p.city() == Some(c))
            })
            .map(|(id, p)| (id.as_str(), p))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list
    }

    /// The host of a city, if it exists: its leader, who is always in it.
    pub fn city_host(&self, id: CityId) -> Option<&str> {
        Some(self.cities.get(&id)?.leader.as_str())
    }

    /// Where the player's city is, if in one.
    pub fn city_of(&self, capcom_id: &str) -> Option<CityId> {
        let p = self.places.get(capcom_id)?;
        Some((p.server, p.gate()?, p.city()?))
    }

    /// Everyone in a city, the earliest first.
    pub fn city_players(&self, (server, gate, city): CityId) -> Vec<String> {
        let mut list: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| p.server == server && p.gate() == Some(gate) && p.city() == Some(city))
            .collect();
        list.sort_by_key(|(_, p)| p.arrival);
        list.into_iter().map(|(id, _)| id.clone()).collect()
    }

    /// Creates a circle in the player's city, in its first free slot, and puts the player in it as leader. `None` for
    /// a player outside a city or already in a quest (one quest at a time).
    pub fn create_circle(&mut self, capcom_id: &str, new: NewCircle) -> Option<CircleId> {
        let city = self.city_of(capcom_id)?;
        if self.places.get(capcom_id)?.circle().is_some() {
            return None;
        }
        let index = (1..=CIRCLES).find(|i| !self.circles.contains_key(&(city, *i)))?;
        self.quests += 1;
        let number = self.quests;
        let circle = Circle {
            number,
            leader: capcom_id.to_owned(),
            has_password: new.password_hash.is_some(),
            state: Quest::Recruiting {
                password_hash: new.password_hash,
            },
            remarks: new.remarks,
            unk_0e: new.unk_0e,
            quest_id: new.quest_id,
            capacity: new.capacity.clamp(1, CIRCLE_CAPACITY),
        };
        self.circles.insert((city, index), circle);
        let leader = Member {
            circle: index,
            number,
            slot: 0,
            ready: false,
        };
        self.places
            .get_mut(capcom_id)
            .expect("in a city")
            .set_party(Some(leader));
        Some((city, index))
    }

    pub fn circle(&self, id: CircleId) -> Option<&Circle> {
        self.circles.get(&id)
    }

    /// The circle the player is in.
    pub fn circle_of(&self, capcom_id: &str) -> Option<CircleId> {
        let city = self.city_of(capcom_id)?;
        Some((city, self.places.get(capcom_id)?.circle()?))
    }

    /// A quest's members, by index (1-based) and then slot: those of its city whose membership names it.
    fn party(&self, id: CircleId) -> Vec<(u8, &str, &Member)> {
        let Some(circle) = self.circles.get(&id) else {
            return Vec::new();
        };
        let mut members: Vec<_> = self
            .places
            .iter()
            .filter(|(_, p)| (p.server, p.gate(), p.city()) == (id.0.0, Some(id.0.1), Some(id.0.2)))
            .filter_map(|(who, p)| {
                let member = p.party()?;
                (member.circle == id.1 && member.number == circle.number).then_some((
                    member.slot + 1,
                    who.as_str(),
                    member,
                ))
            })
            .collect();
        members.sort_by_key(|(index, _, _)| *index);
        members
    }

    /// A quest's members, by index (1-based).
    pub fn members(&self, id: CircleId) -> Vec<(u8, &str)> {
        self.party(id)
            .into_iter()
            .map(|(index, who, _)| (index, who))
            .collect()
    }

    /// The index (1-based) of a player in a quest.
    pub fn index_of(&self, id: CircleId, capcom_id: &str) -> Option<u8> {
        let member = self.places.get(capcom_id)?.party()?;
        (self.circle_of(capcom_id) == Some(id) && self.circles.get(&id)?.number == member.number)
            .then_some(member.slot + 1)
    }

    /// The player says whether it is ready to depart. Only a quest being prepared takes it.
    pub fn set_standby(&mut self, capcom_id: &str, standby: bool) {
        let recruiting = self
            .circle_of(capcom_id)
            .and_then(|id| self.circles.get(&id))
            .is_some_and(|c| matches!(c.state, Quest::Recruiting { .. }));
        if recruiting
            && let Some(Place {
                at:
                    Layer::City {
                        party: Some(member),
                        ..
                    },
                ..
            }) = self.places.get_mut(capcom_id)
        {
            member.ready = standby;
        }
    }

    /// The quest departs, if it is being prepared and its leader is ready: the players ready go on it (index and
    /// Capcom ID). `None` for a quest that left already, or whose leader isn't ready (the game offers it no
    /// departure): nothing changes. The members not ready are still in it; the caller sends them away.
    pub fn depart(&mut self, id: CircleId) -> Option<Vec<(u8, String)>> {
        let circle = self.circles.get(&id)?;
        if !matches!(circle.state, Quest::Recruiting { .. }) {
            return None;
        }
        let party = self.party(id);
        if !party
            .iter()
            .any(|(_, who, member)| *who == circle.leader && member.ready)
        {
            return None;
        }
        let ready = party
            .into_iter()
            .filter(|(_, _, member)| member.ready)
            .map(|(index, who, _)| (index, who.to_owned()))
            .collect();
        // Nobody joins a departed quest: its password goes with its state.
        self.circles.get_mut(&id)?.state = Quest::Departed;
        Some(ready)
    }

    /// Whether the player is out hunting: in a quest that departed with them.
    pub fn on_quest(&self, capcom_id: &str) -> bool {
        self.circle_of(capcom_id)
            .and_then(|id| self.circles.get(&id))
            .is_some_and(|c| matches!(c.state, Quest::Departed))
    }

    /// A quest's members: index, Capcom ID, name, ready or not.
    pub fn circle_members(&self, id: CircleId) -> Vec<(u8, String, Bytes, bool)> {
        self.party(id)
            .into_iter()
            .map(|(index, who, member)| {
                let name = self
                    .places
                    .get(who)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                (index, who.to_owned(), name, member.ready)
            })
            .collect()
    }

    /// What a join needs to know of a quest before it can check its password: its number, and its password hash if it
    /// has one while it is being prepared. `None` for a quest that doesn't exist.
    pub fn circle_lock(&self, id: CircleId) -> Option<(u64, Option<String>)> {
        let circle = self.circles.get(&id)?;
        let hash = match &circle.state {
            Quest::Recruiting { password_hash } => password_hash.clone(),
            Quest::Departed => None,
        };
        Some((circle.number, hash))
    }

    /// Puts the player in the quest `number` at `id` (the password of that quest checked already, with the world
    /// unlocked meanwhile): its index in the party. Refused as gone unless the player is still in the quest's city
    /// and the quest at that index is the one it checked: it may have broken and been made again since.
    pub fn join_circle(
        &mut self,
        capcom_id: &str,
        id: CircleId,
        number: u64,
    ) -> Result<u8, CircleRefusal> {
        if self.city_of(capcom_id) != Some(id.0) {
            return Err(CircleRefusal::Missing);
        }
        if self.circle_of(capcom_id).is_some() {
            return Err(CircleRefusal::AlreadyInOne);
        }
        let circle = self
            .circles
            .get(&id)
            .filter(|c| c.number == number)
            .ok_or(CircleRefusal::Missing)?;
        if matches!(circle.state, Quest::Departed) {
            return Err(CircleRefusal::Departed);
        }
        let taken: Vec<u8> = self.party(id).iter().map(|(_, _, m)| m.slot).collect();
        let slot = (0..self.circles[&id].capacity as u8)
            .find(|slot| !taken.contains(slot))
            .ok_or(CircleRefusal::Full)?;
        let member = Member {
            circle: id.1,
            number,
            slot,
            ready: false,
        };
        if let Some(place) = self.places.get_mut(capcom_id) {
            place.set_party(Some(member));
        }
        Ok(slot + 1)
    }

    /// The player leaves its circle. A leader leaving hands a departed quest to the next member, and breaks one not
    /// departed yet (MH3SP's rules, M3 D7).
    pub fn leave_circle(&mut self, capcom_id: &str) -> Option<Leaving> {
        let id = self.circle_of(capcom_id)?;
        let index = self.index_of(id, capcom_id).unwrap_or(0);
        self.places.get_mut(capcom_id)?.set_party(None);
        let members: Vec<(u8, String)> = self
            .members(id)
            .into_iter()
            .map(|(i, m)| (i, m.to_owned()))
            .collect();
        let others: Vec<String> = members.iter().map(|(_, m)| m.clone()).collect();
        let mut leaving = Leaving {
            id,
            index,
            others: others.clone(),
            new_leader: None,
            broken: false,
        };
        let circle = self.circles.get_mut(&id)?;
        if circle.leader == capcom_id {
            match (&circle.state, members.first()) {
                (Quest::Departed, Some((i, m))) => {
                    circle.leader = m.clone();
                    leaving.new_leader = Some((*i, m.clone()));
                }
                _ => {
                    self.circles.remove(&id);
                    for m in &others {
                        if let Some(p) = self.places.get_mut(m) {
                            p.set_party(None);
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
        let members = self.members(id);
        // Per player: Capcom ID at 0x10·i, name at 0x40 + 0x10·i, weapon type at 0x80 + i, a flag at 0x84 + i.
        let mut party = vec![0u8; 0x88];
        for &(index, member) in &members {
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
        let capacity = circle.capacity;
        let count = members.len() as u32;
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
                Item::Byte(u8::from(
                    matches!(circle.state, Quest::Departed) || count >= capacity,
                )),
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

    /// How many players of the city are out hunting.
    fn in_quest(&self, city: CityId) -> u32 {
        self.circles
            .iter()
            .filter(|((c, _), circle)| *c == city && matches!(circle.state, Quest::Departed))
            .map(|(&id, _)| self.members(id).len() as u32)
            .sum()
    }

    /// The world's invariants (the sprint "A world with no impossible state"): what each fact says of the others
    /// it is kept beside. One line per violation; empty when the world is consistent. Run after every release of the
    /// world's lock in the test build (`invariants` feature), never in the release.
    #[cfg(any(feature = "invariants", test))]
    pub fn check(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for (id, place) in &self.places {
            if let Some(city) = self.city_of(id)
                && !self.cities.contains_key(&city)
            {
                bad.push(format!("{id} is in city {city:?}, which doesn't exist"));
            }
            if let (Some(city), Some(member)) = (self.city_of(id), place.party()) {
                let index = member.circle;
                match self.circles.get(&(city, index)) {
                    None => bad.push(format!(
                        "{id} is in quest {index} of {city:?}, which doesn't exist"
                    )),
                    Some(c) if c.number != member.number => bad.push(format!(
                        "{id} is in quest {index} of {city:?}, which is another than the one it joined"
                    )),
                    Some(c) if usize::from(member.slot) >= c.capacity as usize => bad.push(format!(
                        "{id} has slot {} of quest {index} of {city:?}, which takes {}",
                        member.slot, c.capacity
                    )),
                    Some(_) => {}
                }
            }
            if let Layer::Gate {
                gate,
                reserving: Some(number),
            } = place.at
            {
                let city = (place.server, gate, number);
                if self.cities.contains_key(&city) {
                    bad.push(format!("city {city:?} exists and is reserved by {id}"));
                }
                let holders = self.places.values().filter(|p| {
                    p.server == city.0
                        && p.at
                            == Layer::Gate {
                                gate,
                                reserving: Some(number),
                            }
                });
                if holders.count() > 1 {
                    bad.push(format!("city {city:?} is reserved by more than one player"));
                }
            }
        }
        for (&id, circle) in &self.circles {
            let (city, index) = id;
            let members = self.members(id);
            if members.is_empty() {
                bad.push(format!("quest {index} of {city:?} has no member"));
            }
            if !members.iter().any(|(_, who)| *who == circle.leader) {
                bad.push(format!(
                    "quest {index} of {city:?} is led by {}, who isn't in it",
                    circle.leader
                ));
            }
            // The members come sorted by index: a slot given twice is two neighbours with the same one.
            let mut slots: Vec<u8> = members.iter().map(|(i, _)| *i).collect();
            slots.dedup();
            if slots.len() != members.len() {
                bad.push(format!(
                    "quest {index} of {city:?} gives a slot to two players"
                ));
            }
        }
        for (&id, city) in &self.cities {
            let here = self.city_players(id);
            if here.is_empty() {
                bad.push(format!("city {id:?} is empty"));
            }
            if !here.contains(&city.leader) {
                bad.push(format!(
                    "city {id:?} is led by {:?}, who isn't in it",
                    city.leader
                ));
            }
            for (slot, holder) in &city.seats {
                if !here.contains(holder) {
                    bad.push(format!(
                        "seat {slot} of city {id:?} is held by {holder}, who isn't in it"
                    ));
                }
            }
        }
        bad.sort();
        bad.dedup();
        bad
    }

    /// After a player left city `id`: it goes when nobody is in it, and passes to the earliest player left when its
    /// leader was the one who left.
    fn tidy(&mut self, id: CityId) {
        let players = self.city_players(id);
        let Some(city) = self.cities.get_mut(&id) else {
            return;
        };
        match players.first() {
            None => {
                self.cities.remove(&id);
            }
            Some(first) if !players.contains(&city.leader) => city.leader = first.clone(),
            Some(_) => {}
        }
    }
}

/// A layer's state in lists: 1 empty, 2 full (or reserved), 3 joinable.
/// A layer's path as MH3SP writes it: depth (1 world server, 2 gate, 3 city), world server, 1, gate, city.
pub fn path(server: u32, gate: Option<u16>, city: Option<u16>) -> [u8; 14] {
    let depth: u32 = match (gate, city) {
        (_, Some(_)) => 3,
        (Some(_), None) => 2,
        (None, None) => 1,
    };
    jump_path(depth, server, gate, city)
}

/// A path a console can jump to (M5 D11): the same, starting with the version constant that console reported at
/// login (ConnectionData item 3) instead of the depth.
pub fn jump_path(version: u32, server: u32, gate: Option<u16>, city: Option<u16>) -> [u8; 14] {
    let mut path = [0; 14];
    let mut out = &mut path[..];
    out.put_u32(version);
    out.put_u32(server);
    out.put_u16(1);
    out.put_u16(gate.unwrap_or(0));
    out.put_u16(city.unwrap_or(0));
    path
}

/// Where a jump goes: world server, gate, and city if any.
pub type Target = (u32, u16, Option<u16>);

/// A jump's target, from its path: an existing world server and gate, and city if any.
pub fn jump_target(path: &[u8]) -> Option<Target> {
    let (server, gate, city) = parse_path(path)?;
    let gate = gate.filter(|g| (1..=GATES).contains(g))?;
    let valid = (1..=SERVERS.len() as u32).contains(&server)
        && city.is_none_or(|c| (1..=CITIES).contains(&c));
    valid.then_some((server, gate, city))
}

/// A path's world server, gate and city (0 parts are absent).
pub fn parse_path(path: &[u8]) -> Option<(u32, Option<u16>, Option<u16>)> {
    let mut path = path.get(4..)?;
    let server = path.try_get_u32().ok()?;
    let mut part = || path.try_get_u16().ok().filter(|v| *v != 0);
    let (_, gate, city) = (part(), part(), part());
    Some((server, gate, city))
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
                .with(0x11, Item::Long(POSITION_INTERVAL))
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
                .with(0x11, Item::Long(POSITION_INTERVAL))
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
                    Item::Byte(state(population, CITY_CAPACITY, world.reserved(id))),
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

    #[test]
    fn rookies_and_experts_by_hr() {
        let (open, rookie, expert, recruiting) = (1, 5, 7, 9);
        for (index, hr, refused) in [
            (rookie, 30, None),
            (rookie, 31, Some(Text::RookiesOnly)),
            (expert, 30, Some(Text::ExpertsOnly)),
            (expert, 31, None),
            (open, 999, None),
            (recruiting, 1, None),
        ] {
            assert_eq!(
                hr_refusal(index, Some(hr)),
                refused,
                "server {index}, HR {hr}"
            );
        }
        assert_eq!(hr_refusal(rookie, None), None, "unknown HR passes");
        assert_eq!(hr_refusal(0, Some(99)), None);
        assert_eq!(hr_refusal(13, Some(99)), None);
    }

    fn world_with(players: &[&str]) -> World {
        let mut world = World::default();
        for (conn, id) in players.iter().enumerate() {
            let entered = world.enter(id, conn as u64 + 1, 1, Bytes::new(), Profile::default());
            entered.unwrap();
            world.enter_gate(id, 1);
        }
        world
    }

    #[test]
    fn a_place_is_never_replaced_by_entering_again() {
        let mut world = world_with(&["AAAAAA", "BBBBBB"]);
        let id = (1, 1, 1);
        world.reserve_city(id, "AAAAAA").unwrap();
        world
            .create_city(id, "AAAAAA", OptionalFields::default())
            .unwrap();
        world.join_city(id, "BBBBBB").unwrap();
        // A's first connection (1) still holds its place, in the city it leads: a login on connection 9 is told
        // whose place it is, and changes nothing.
        let again =
            |world: &mut World| world.enter("AAAAAA", 9, 1, Bytes::new(), Profile::default());
        assert_eq!(again(&mut world), Err(1));
        assert_eq!(world.city_of("AAAAAA"), Some(id));
        assert_eq!(world.city_host(id), Some("AAAAAA"));
        // Once that connection has left (what `depart` does), the new one enters at the root.
        world.leave("AAAAAA", 1);
        assert_eq!(world.city_host(id), Some("BBBBBB"));
        assert_eq!(again(&mut world), Ok(()));
        assert_eq!(world.city_of("AAAAAA"), None);
        assert_eq!(world.check(), Vec::<String>::new());
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

    /// Every list shows the players in the order they reached the place, the earliest first, not by name nor by
    /// slot; one who leaves and comes back is last.
    #[test]
    fn lists_show_players_in_arrival_order() {
        let ids = |list: Vec<(&str, &Place)>| -> Vec<String> {
            list.into_iter().map(|(id, _)| id.to_owned()).collect()
        };
        let mut world = world_with(&["CCCCCC", "AAAAAA", "BBBBBB"]);
        assert_eq!(
            ids(world.neighbours("AAAAAA")),
            ["CCCCCC", "AAAAAA", "BBBBBB"]
        );
        // C goes into a city, B follows, then A; C comes back to the gate.
        let city = (1, 1, 3);
        world.reserve_city(city, "CCCCCC").unwrap();
        world
            .create_city(city, "CCCCCC", OptionalFields::default())
            .unwrap();
        world.release_city(city, "CCCCCC");
        world.join_city(city, "BBBBBB").unwrap();
        world.join_city(city, "AAAAAA").unwrap();
        assert_eq!(world.city_players(city), ["CCCCCC", "BBBBBB", "AAAAAA"]);
        assert_eq!(
            ids(world.players_at(1, Some(1), Some(3))),
            ["CCCCCC", "BBBBBB", "AAAAAA"]
        );
        world.leave_city("CCCCCC");
        assert_eq!(world.city_players(city), ["BBBBBB", "AAAAAA"]);
        world.leave_city("BBBBBB");
        world.join_city(city, "BBBBBB").unwrap();
        assert_eq!(world.city_players(city), ["AAAAAA", "BBBBBB"]);
        // The gate's search covers its cities, each player by its last arrival: A in the city, C back at the gate,
        // B back in the city.
        assert_eq!(
            ids(world.players_under(1, Some(1), None)),
            ["AAAAAA", "CCCCCC", "BBBBBB"]
        );
        assert_eq!(ids(world.everyone()), ["AAAAAA", "CCCCCC", "BBBBBB"]);
    }

    #[test]
    fn a_reservation_goes_with_its_player() {
        let mut world = world_with(&["AAAAAA"]);
        world.reserve_city((1, 1, 2), "AAAAAA").unwrap();
        assert!(world.reserved((1, 1, 2)));
        world.leave("AAAAAA", 1);
        assert!(!world.reserved((1, 1, 2)));
        assert!(world.cities.is_empty() && world.places.is_empty());
    }

    #[test]
    fn a_reservation_ends_when_its_hunter_leaves_the_gate() {
        let mut world = world_with(&["AAAAAA", "BBBBBB"]);
        let id = (1, 1, 2);
        world.reserve_city(id, "AAAAAA").unwrap();
        // Shown full, and not taken by another meanwhile.
        assert!(world.reserved(id));
        assert_eq!(world.reserve_city(id, "BBBBBB"), Err(CityRefusal::Exists));
        world.up("AAAAAA");
        assert!(!world.reserved(id));
        world.reserve_city(id, "BBBBBB").unwrap();
    }

    #[test]
    fn a_hunter_reserves_one_city_at_most() {
        let mut world = world_with(&["AAAAAA", "BBBBBB"]);
        world.reserve_city((1, 1, 2), "AAAAAA").unwrap();
        world.reserve_city((1, 1, 3), "AAAAAA").unwrap();
        assert!(!world.reserved((1, 1, 2)));
        assert!(world.reserved((1, 1, 3)));
        // The number it dropped is free for another.
        world.reserve_city((1, 1, 2), "BBBBBB").unwrap();
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_city_is_created_only_from_its_gate_reservation() {
        let mut world = world_with(&["AAAAAA", "BBBBBB"]);
        let create =
            |world: &mut World, id, who| world.create_city(id, who, OptionalFields::default());
        // Nothing reserved.
        assert_eq!(
            create(&mut world, (1, 1, 1), "AAAAAA"),
            Err(CityRefusal::Missing)
        );
        // Another number than the one reserved (#57's second city).
        world.reserve_city((1, 1, 1), "AAAAAA").unwrap();
        assert_eq!(
            create(&mut world, (1, 1, 2), "AAAAAA"),
            Err(CityRefusal::Missing)
        );
        // Someone else's reservation.
        assert_eq!(
            create(&mut world, (1, 1, 1), "BBBBBB"),
            Err(CityRefusal::Missing)
        );
        create(&mut world, (1, 1, 1), "AAAAAA").unwrap();
        // From inside a city, even a reserved-looking number or one that exists.
        assert_eq!(
            create(&mut world, (1, 1, 2), "AAAAAA"),
            Err(CityRefusal::Missing)
        );
        assert_eq!(
            create(&mut world, (1, 1, 1), "BBBBBB"),
            Err(CityRefusal::Exists)
        );
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_leader_leaving_hands_the_city_on_at_once() {
        let mut world = full_world();
        let id = (1, 1, 1);
        world.join_city(id, "CCCCCC").unwrap();
        assert_eq!(world.city_host(id), Some("AAAAAA"));
        // The earliest player left leads, with no step in between; the last one out removes the city.
        world.leave_city("AAAAAA");
        assert_eq!(world.city_host(id), Some("BBBBBB"));
        assert_eq!(world.check(), Vec::<String>::new());
        world.leave_city("BBBBBB");
        assert_eq!(world.city_host(id), Some("CCCCCC"));
        world.leave_city("CCCCCC");
        assert_eq!(world.city_host(id), None);
        assert!(world.cities.is_empty());
    }

    #[test]
    fn a_quest_goes_with_its_city() {
        let mut world = full_world();
        // A leads the quest: leaving the city ends it, B is out of it too (#57: no quest outside a city).
        world.leave_city("AAAAAA");
        assert_eq!(world.circle_of("BBBBBB"), None);
        assert!(world.circles.is_empty());
        assert_eq!(world.check(), Vec::<String>::new());
    }

    /// `who` joins quest 1 of city (1, 1, 1) as it is now (what a join does once its password is checked).
    fn join(world: &mut World, who: &str) -> Result<u8, CircleRefusal> {
        let number = world.circles[&((1, 1, 1), 1)].number;
        world.join_circle(who, ((1, 1, 1), 1), number)
    }

    /// A and B in city (1, 1, 1), A leading it and quest 1 with B in it, A on seat 1; C at the gate.
    fn full_world() -> World {
        let mut world = world_with(&["AAAAAA", "BBBBBB", "CCCCCC"]);
        let id = (1, 1, 1);
        world.reserve_city(id, "AAAAAA").unwrap();
        world
            .create_city(id, "AAAAAA", OptionalFields::default())
            .unwrap();
        world.release_city(id, "AAAAAA");
        world.join_city(id, "BBBBBB").unwrap();
        let new = NewCircle {
            password_hash: None,
            remarks: None,
            unk_0e: 0,
            capacity: 4,
            quest_id: 1,
        };
        world.create_circle("AAAAAA", new).unwrap();
        join(&mut world, "BBBBBB").unwrap();
        assert!(world.take_seat(id, 1, "AAAAAA"));
        world
    }

    /// The violations of `world`, which must name `expected` (a part of the line) and nothing else.
    fn only(world: &World, expected: &str) {
        let bad = world.check();
        assert!(
            bad.len() == 1 && bad[0].contains(expected),
            "wanted only {expected:?}, got {bad:?}"
        );
    }

    #[test]
    fn a_consistent_world_has_no_violation() {
        assert_eq!(full_world().check(), Vec::<String>::new());
        assert_eq!(World::default().check(), Vec::<String>::new());
    }

    /// C in city (1, 1, 1), with the membership `member`.
    fn with_member(member: Member) -> World {
        let mut world = full_world();
        world.join_city((1, 1, 1), "CCCCCC").unwrap();
        world.place("CCCCCC").unwrap().set_party(Some(member));
        world
    }

    #[test]
    fn a_member_of_another_quest_than_the_one_at_its_index_is_a_violation() {
        // Quest 1 was made again since: its number is not the one the membership holds.
        let world = with_member(Member {
            circle: 1,
            number: 999,
            slot: 2,
            ready: false,
        });
        only(
            &world,
            "quest 1 of (1, 1, 1), which is another than the one it joined",
        );
    }

    #[test]
    fn a_hunter_in_a_quest_that_is_gone_is_a_violation() {
        let world = with_member(Member {
            circle: 3,
            number: 1,
            slot: 0,
            ready: false,
        });
        only(&world, "quest 3 of (1, 1, 1), which doesn't exist");
    }

    #[test]
    fn two_players_in_one_slot_are_a_violation() {
        let number = full_world().circles[&((1, 1, 1), 1)].number;
        let world = with_member(Member {
            circle: 1,
            number,
            slot: 0,
            ready: false,
        });
        only(&world, "quest 1 of (1, 1, 1) gives a slot to two players");
    }

    #[test]
    fn a_slot_past_the_quest_capacity_is_a_violation() {
        let number = full_world().circles[&((1, 1, 1), 1)].number;
        let world = with_member(Member {
            circle: 1,
            number,
            slot: 4,
            ready: false,
        });
        only(&world, "has slot 4 of quest 1 of (1, 1, 1), which takes 4");
    }

    #[test]
    fn a_hunter_in_a_city_that_does_not_exist_is_a_violation() {
        let mut world = full_world();
        world.place("CCCCCC").unwrap().at = Layer::City {
            gate: 1,
            city: 9,
            standing: false,
            party: None,
        };
        only(&world, "CCCCCC is in city (1, 1, 9), which doesn't exist");
    }

    #[test]
    fn a_quest_led_by_a_stranger_is_a_violation() {
        let mut world = full_world();
        world.circles.get_mut(&((1, 1, 1), 1)).unwrap().leader = "ZZZZZZ".into();
        only(&world, "is led by ZZZZZZ, who isn't in it");
    }

    #[test]
    fn a_city_led_from_outside_is_a_violation() {
        let mut world = full_world();
        world.cities.get_mut(&(1, 1, 1)).unwrap().leader = "CCCCCC".into();
        only(
            &world,
            "city (1, 1, 1) is led by \"CCCCCC\", who isn't in it",
        );
    }

    #[test]
    fn a_seat_held_from_outside_is_a_violation() {
        let mut world = full_world();
        world
            .cities
            .get_mut(&(1, 1, 1))
            .unwrap()
            .seats
            .insert(2, "CCCCCC".into());
        only(&world, "seat 2 of city (1, 1, 1) is held by CCCCCC");
    }

    #[test]
    fn a_city_reserved_by_two_hunters_is_a_violation() {
        let mut world = full_world();
        let entered = world.enter("DDDDDD", 4, 1, Bytes::new(), Profile::default());
        entered.unwrap();
        world.enter_gate("DDDDDD", 1);
        world.reserve_city((1, 1, 5), "CCCCCC").unwrap();
        assert_eq!(world.check(), Vec::<String>::new());
        world.place("DDDDDD").unwrap().at = Layer::Gate {
            gate: 1,
            reserving: Some(5),
        };
        only(&world, "city (1, 1, 5) is reserved by more than one player");
    }

    const QUEST: CircleId = ((1, 1, 1), 1);

    #[test]
    fn a_departed_quest_takes_no_join_and_no_second_departure() {
        let mut world = full_world();
        world.set_standby("AAAAAA", true);
        world.set_standby("BBBBBB", true);
        let went = vec![(1, "AAAAAA".to_owned()), (2, "BBBBBB".to_owned())];
        assert_eq!(world.depart(QUEST), Some(went));
        assert_eq!(world.depart(QUEST), None);
        world.join_city(QUEST.0, "CCCCCC").unwrap();
        assert_eq!(join(&mut world, "CCCCCC"), Err(CircleRefusal::Departed));
        assert!(world.on_quest("AAAAAA") && !world.on_quest("CCCCCC"));
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_leader_not_ready_cannot_depart() {
        let mut world = full_world();
        world.set_standby("BBBBBB", true);
        assert_eq!(world.depart(QUEST), None);
        // Nothing changed: still being prepared, still led by A, B still ready.
        assert!(!world.on_quest("AAAAAA"));
        assert_eq!(world.circle(QUEST).unwrap().leader, "AAAAAA");
        assert!(
            world
                .circle_members(QUEST)
                .iter()
                .any(|m| m.1 == "BBBBBB" && m.3)
        );
        world.set_standby("AAAAAA", true);
        assert_eq!(world.depart(QUEST).map(|went| went.len()), Some(2));
    }

    #[test]
    fn readiness_on_a_departed_quest_changes_nothing() {
        let mut world = full_world();
        world.set_standby("AAAAAA", true);
        world.set_standby("BBBBBB", true);
        world.depart(QUEST).unwrap();
        world.set_standby("BBBBBB", false);
        let ready: Vec<bool> = world.circle_members(QUEST).iter().map(|m| m.3).collect();
        assert_eq!(ready, [true, true]);
    }

    #[test]
    fn departing_drops_the_password() {
        let mut world = world_with(&["AAAAAA"]);
        world.reserve_city(QUEST.0, "AAAAAA").unwrap();
        world
            .create_city(QUEST.0, "AAAAAA", OptionalFields::default())
            .unwrap();
        let new = NewCircle {
            password_hash: Some("$argon2id$hash".into()),
            remarks: None,
            unk_0e: 0,
            capacity: 4,
            quest_id: 1,
        };
        world.create_circle("AAAAAA", new).unwrap();
        let hash = |world: &World| world.circle_lock(QUEST).and_then(|(_, hash)| hash);
        assert_eq!(hash(&world).as_deref(), Some("$argon2id$hash"));
        world.set_standby("AAAAAA", true);
        world.depart(QUEST).unwrap();
        assert_eq!(hash(&world), None);
        // Lists keep showing that it had one.
        let (info, _) = world.circle_info(QUEST);
        assert_eq!(info.get(0x03), Some(&Item::Byte(1)));
    }

    #[test]
    fn a_quest_made_again_at_the_same_index_is_another_quest() {
        let mut world = full_world();
        let first = world.circles[&QUEST].number;
        let new = || NewCircle {
            password_hash: None,
            remarks: None,
            unk_0e: 0,
            capacity: 4,
            quest_id: 1,
        };
        // Its leader leaves: it breaks, B is out of it, and the index is free for a new one.
        world.leave_circle("AAAAAA").unwrap();
        assert_eq!(world.circle_of("BBBBBB"), None);
        assert_eq!(world.create_circle("BBBBBB", new()), Some(QUEST));
        assert_ne!(world.circles[&QUEST].number, first);
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_player_leaving_a_quest_frees_its_slot_for_the_next() {
        let mut world = full_world();
        world.join_city(QUEST.0, "CCCCCC").unwrap();
        assert_eq!(join(&mut world, "CCCCCC"), Ok(3));
        world.leave_circle("BBBBBB").unwrap();
        assert_eq!(world.members(QUEST), [(1, "AAAAAA"), (3, "CCCCCC")]);
        let entered = world.enter("DDDDDD", 4, 1, Bytes::new(), Profile::default());
        entered.unwrap();
        world.enter_gate("DDDDDD", 1);
        world.join_city(QUEST.0, "DDDDDD").unwrap();
        assert_eq!(join(&mut world, "DDDDDD"), Ok(2));
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_departed_quest_hands_its_lead_on_and_a_recruiting_one_breaks() {
        let mut world = full_world();
        world.set_standby("AAAAAA", true);
        world.set_standby("BBBBBB", true);
        world.depart(QUEST).unwrap();
        let left = world.leave_circle("AAAAAA").unwrap();
        assert_eq!(left.new_leader, Some((2, "BBBBBB".to_owned())));
        assert!(!left.broken);
        assert_eq!(world.circle(QUEST).unwrap().leader, "BBBBBB");
        // And a quest not departed yet breaks with its leader.
        let mut world = full_world();
        let left = world.leave_circle("AAAAAA").unwrap();
        assert!(left.broken && left.new_leader.is_none());
        assert_eq!(world.circle_of("BBBBBB"), None);
        assert_eq!(world.check(), Vec::<String>::new());
    }

    /// What a join noted of quest 1 before its password check, and the world afterwards (#73): C looks at the quest,
    /// the world is unlocked for the check, `then` happens, and C joins with what it noted.
    fn join_after(then: impl FnOnce(&mut World)) -> (World, Result<u8, CircleRefusal>) {
        let mut world = full_world();
        world.join_city(QUEST.0, "CCCCCC").unwrap();
        let (number, _) = world.circle_lock(QUEST).unwrap();
        then(&mut world);
        let joined = world.join_circle("CCCCCC", QUEST, number);
        (world, joined)
    }

    #[test]
    fn a_join_of_the_same_quest_still_succeeds() {
        let (world, joined) = join_after(|_| {});
        assert_eq!(joined, Ok(3));
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_join_after_the_quest_was_made_again_is_refused() {
        let (world, joined) = join_after(|world| {
            // Its leader leaves (it breaks, B is out of it), and B makes a quest at the same index.
            world.leave_circle("AAAAAA").unwrap();
            let new = NewCircle {
                password_hash: None,
                remarks: None,
                unk_0e: 0,
                capacity: 4,
                quest_id: 1,
            };
            assert_eq!(world.create_circle("BBBBBB", new), Some(QUEST));
        });
        assert_eq!(joined, Err(CircleRefusal::Missing));
        assert_eq!(world.circle_of("CCCCCC"), None);
        assert_eq!(world.members(QUEST), [(1, "BBBBBB")]);
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_join_after_the_hunter_left_the_city_is_refused() {
        let (world, joined) = join_after(|world| world.leave_city("CCCCCC"));
        assert_eq!(joined, Err(CircleRefusal::Missing));
        // It is at its gate: no quest, and nothing in the quest it didn't join.
        assert_eq!(world.circle_of("CCCCCC"), None);
        assert_eq!(world.members(QUEST), [(1, "AAAAAA"), (2, "BBBBBB")]);
        assert_eq!(world.check(), Vec::<String>::new());
    }

    #[test]
    fn a_join_after_the_hunter_went_to_another_city_is_refused() {
        let (world, joined) = join_after(|world| {
            world.leave_city("CCCCCC");
            world.reserve_city((1, 1, 2), "CCCCCC").unwrap();
            world
                .create_city((1, 1, 2), "CCCCCC", OptionalFields::default())
                .unwrap();
        });
        assert_eq!(joined, Err(CircleRefusal::Missing));
        assert_eq!(world.city_of("CCCCCC"), Some((1, 1, 2)));
        assert_eq!(world.members(QUEST), [(1, "AAAAAA"), (2, "BBBBBB")]);
        assert_eq!(world.check(), Vec::<String>::new());
    }

    /// The violations of `world` must include one naming `expected` (a part of the line): others may come with it.
    fn includes(world: &World, expected: &str) {
        let bad = world.check();
        assert!(
            bad.iter().any(|line| line.contains(expected)),
            "wanted {expected:?} among {bad:?}"
        );
    }

    #[test]
    fn a_quest_nobody_is_in_is_a_violation() {
        let mut world = full_world();
        // Both members gone from it (not through `leave_circle`, which would end the quest).
        world.place("AAAAAA").unwrap().set_party(None);
        world.place("BBBBBB").unwrap().set_party(None);
        includes(&world, "quest 1 of (1, 1, 1) has no member");
    }

    #[test]
    fn a_city_nobody_is_in_is_a_violation() {
        let mut world = full_world();
        // Both players at the root again, the city's record left behind.
        world.place("AAAAAA").unwrap().at = Layer::Root;
        world.place("BBBBBB").unwrap().at = Layer::Root;
        includes(&world, "city (1, 1, 1) is empty");
    }

    #[test]
    fn a_reservation_of_a_city_that_exists_is_a_violation() {
        let mut world = full_world();
        world.place("CCCCCC").unwrap().at = Layer::Gate {
            gate: 1,
            reserving: Some(1),
        };
        only(&world, "city (1, 1, 1) exists and is reserved by CCCCCC");
    }
}
