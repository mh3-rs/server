//! Event quests: MH3SP's quest files (JSON) and the binary the console downloads for each (types `0x06`–`0x0f`).
//!
//! The layout is MH3SP's (`mh/quest_utils.py`, `mh/equipment_utils.py`): a fixed block of 0x4B4 bytes (texts,
//! monsters, objectives, rewards), then the small monsters of each area and wave, then, for arena quests, the
//! four equipment sets.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use bytes::BufMut;
use serde::Deserialize;

use super::pad;

/// The fixed part of a quest binary; a day with fewer quests than slots sends this many zero bytes.
pub const QUEST_SIZE: usize = 0x4B4;

#[derive(Debug, Deserialize)]
pub struct Quest {
    pub quest_info: QuestInfo,
    large_monsters: LargeMonsters,
    objective_details: Objectives,
    unknown: Unknown,
    /// Per wave, per area of the location: the monsters placed there. None when the quest names a small monster
    /// data file instead (`smallmonster_data_file`).
    #[serde(default)]
    small_monsters: Vec<Vec<SmallMonster>>,
    /// That file's bytes, if it exists (read by `load`).
    #[serde(skip)]
    small_monster_file: Option<Vec<u8>>,
    /// Arena quests: four sets of (weapon, weapon, weapon, helm, chest, gauntlets, faulds, legs, items, pouch).
    arena_equipment: Option<Vec<ArenaSet>>,
}

#[derive(Debug, Deserialize)]
pub struct QuestInfo {
    pub quest_id: u16,
    pub name: String,
    client: String,
    description: String,
    details: String,
    success_message: String,
    failure_message: Option<String>,
    /// Four bytes, each written as 8 bits, least significant first.
    pub flags: [[u8; 8]; 4],
    penalty_per_cart: u32,
    quest_fee: u32,
    time_limit: i16,
    main_monster_1: u8,
    main_monster_2: u8,
    location: u8,
    quest_rank: u8,
    hrp_restriction: i16,
    resources: u8,
    supply_set_number: u16,
    starting_position: i16,
    general_enemy_level: i16,
    summon: u32,
    wave_1_transition_type: i16,
    wave_1_transition_target: i16,
    wave_1_transition_quantity: i16,
    wave_2_transition_type: i16,
    wave_2_transition_target: i16,
    wave_2_transition_quantity: i16,
    /// A file (next to the quest, hexadecimal text) holding the small monster tables as the game reads them, instead
    /// of `small_monsters`. MH3SP's live server names files it doesn't have: those quests get no small monsters.
    smallmonster_data_file: Option<String>,
}

impl QuestInfo {
    /// The flags as the game reads them (byte 1 most significant).
    pub fn flags(&self) -> u32 {
        self.flags.iter().fold(0, |acc, bits| {
            acc << 8
                | bits
                    .iter()
                    .rev()
                    .fold(0, |b, &bit| b << 1 | u32::from(bit & 1))
        })
    }

    pub fn is_arena(&self) -> bool {
        self.flags() & 0x10 != 0
    }
}

#[derive(Debug, Deserialize)]
struct LargeMonsters {
    monster_1: LargeMonster,
    monster_2: LargeMonster,
    monster_3: LargeMonster,
}

#[derive(Debug, Deserialize)]
struct LargeMonster {
    #[serde(rename = "type")]
    kind: u8,
    starting_area: u8,
    boss_id: u8,
    spawn_count: u8,
    level: u8,
    hp_spread: u8,
    size: u8,
    size_spread: u8,
}

#[derive(Debug, Deserialize)]
struct Objectives {
    main_quest: Objective,
    subquest_1: Objective,
    subquest_2: Objective,
}

#[derive(Debug, Deserialize)]
struct Objective {
    #[serde(rename = "type")]
    kind: Option<u32>,
    #[serde(default)]
    objective_type: i16,
    #[serde(default)]
    objective_num: i16,
    zenny_reward: u32,
    hrp_reward: u32,
    #[serde(default)]
    description: String,
    /// (item, amount, percent).
    rewards_row_1: Vec<(i16, u8, u8)>,
    #[serde(default)]
    rewards_row_2: Vec<(i16, u8, u8)>,
}

/// Values whose meaning is unknown, with MH3SP's defaults.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Unknown {
    #[serde(rename = "unk_4")]
    unk_4: u8,
    #[serde(rename = "unk_5")]
    unk_5: u8,
    #[serde(rename = "unk_6")]
    unk_6: u8,
    #[serde(rename = "unk_7")]
    unk_7: u16,
    #[serde(rename = "unk_12")]
    unk_12: u32,
    #[serde(default)]
    unk_int0: u32,
    #[serde(default)]
    unk_short0: u16,
    #[serde(default)]
    unk_short1: u16,
    #[serde(default)]
    unk_short2: u16,
    #[serde(default)]
    unk_byte0: u8,
    #[serde(default)]
    unk_byte1: u8,
    #[serde(default)]
    unk_byte2: u8,
    #[serde(default)]
    unk_byte3: u8,
    #[serde(default)]
    unk_byte4: u8,
    #[serde(default)]
    unk_byte7: u8,
    #[serde(default)]
    unk_byte8: u8,
    #[serde(default, rename = "unkBytes0_0")]
    unk_bytes0_0: u8,
    #[serde(default, rename = "unkBytes0_1")]
    unk_bytes0_1: u8,
    #[serde(default, rename = "unkBytes0_2")]
    unk_bytes0_2: u8,
    #[serde(default, rename = "unkBytes1_0")]
    unk_bytes1_0: u8,
    #[serde(default, rename = "unkBytes1_1")]
    unk_bytes1_1: u8,
    #[serde(default, rename = "unkBytes1_2")]
    unk_bytes1_2: u8,
    #[serde(default, rename = "unkBytes2_0")]
    unk_bytes2_0: u8,
    #[serde(default, rename = "unkBytes2_1")]
    unk_bytes2_1: u8,
    #[serde(default = "always_15")]
    unk_uint_always15: u32,
}

fn always_15() -> u32 {
    0x0f
}

#[derive(Debug, Deserialize)]
struct SmallMonster {
    #[serde(rename = "type")]
    kind: u32,
    variant: u8,
    room: u8,
    quantity: i8,
    unk1: u8,
    unk2: u8,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
}

/// (weapon, weapon, weapon, helm, chest, gauntlets, faulds, legs, items, gunner pouch). A weapon is
/// (class, id[, slots]).
type ArenaSet = (
    Vec<u16>,
    Option<Vec<u16>>,
    Option<Vec<u16>>,
    u16,
    u16,
    u16,
    u16,
    u16,
    Vec<(u16, u16)>,
    Vec<(u16, u16)>,
);

/// Areas per location, for the small monster tables.
fn areas(location: u8) -> Option<usize> {
    Some(match location {
        1 => 13,
        2 => 12,
        3 | 5 => 11,
        4 => 10,
        6 => 3,
        7 => 4,
        8..=12 => 2,
        _ => return None,
    })
}

pub fn load(path: &Path) -> Result<Quest> {
    let text = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut quest: Quest =
        serde_json::from_slice(&text).with_context(|| format!("parse {}", path.display()))?;
    if let Some(file) = &quest.quest_info.smallmonster_data_file {
        let file = path.with_file_name(file);
        if let Ok(hex) = std::fs::read_to_string(&file) {
            let hex: String = hex.split_whitespace().collect();
            let bytes = hex::decode(hex).with_context(|| format!("parse {}", file.display()))?;
            ensure!(
                bytes.len() >= 4,
                "{}: shorter than its size field",
                file.display()
            );
            quest.small_monster_file = Some(bytes);
        }
    }
    Ok(quest)
}

/// The binary of a quest.
pub fn binary(quest: &Quest) -> Result<Vec<u8>> {
    let q = &quest.quest_info;
    let o = &quest.objective_details;
    let u = &quest.unknown;
    let mut d = Vec::with_capacity(QUEST_SIZE + 0x400);
    pad(&mut d, &q.name, 0x28);
    d.put_u32(0);
    d.put_u16(q.quest_id);
    pad(&mut d, &q.description, 80);
    d.put_bytes(0, 0xC);
    d.extend_from_slice(&[q.quest_rank, q.location]);
    pad(&mut d, &o.subquest_1.description, 0x29);
    pad(&mut d, &o.subquest_2.description, 0x29);
    pad(&mut d, &q.success_message, 0x5C);
    d.put_i16(q.time_limit);
    pad(
        &mut d,
        q.failure_message
            .as_deref()
            .unwrap_or("Reward hits 0, or time\nexpires."),
        0x5C,
    );
    d.put_i16(q.hrp_restriction);
    pad(&mut d, &q.client, 0x28);
    d.put_u32(u.unk_int0);
    d.put_u16(u.unk_short0);
    d.push(u.unk_byte0);
    pad(&mut d, &q.details, 0x100);
    d.put_bytes(0, 0x3D);
    d.extend_from_slice(&[
        u.unk_bytes0_0,
        u.unk_bytes0_1,
        u.unk_bytes0_2,
        u.unk_bytes1_0,
        u.unk_bytes1_1,
        u.unk_bytes1_2,
        q.main_monster_1,
        q.main_monster_2,
        u.unk_bytes2_0,
        u.unk_bytes2_1,
    ]);
    d.put_u32(q.flags());
    let m = &quest.large_monsters;
    for monster in [&m.monster_1, &m.monster_2, &m.monster_3] {
        match monster.kind {
            0 => d.put_bytes(0, 8),
            _ => d.extend_from_slice(&[
                monster.kind,
                monster.starting_area,
                monster.boss_id,
                monster.spawn_count,
                monster.level,
                monster.hp_spread,
                monster.size,
                monster.size_spread,
            ]),
        }
    }
    d.put_u32(q.summon);
    for objective in [&o.main_quest, &o.subquest_1, &o.subquest_2] {
        properties(&mut d, objective);
    }
    for v in [
        q.quest_fee,
        o.main_quest.zenny_reward,
        o.subquest_1.zenny_reward,
        o.subquest_2.zenny_reward,
        q.penalty_per_cart,
        o.main_quest.hrp_reward,
        u.unk_uint_always15,
        o.subquest_1.hrp_reward,
        o.subquest_2.hrp_reward,
    ] {
        d.put_u32(v);
    }
    d.extend_from_slice(&[u.unk_4, q.resources, u.unk_5, u.unk_6]);
    for v in [u.unk_short1, q.supply_set_number, u.unk_short2, u.unk_7] {
        d.put_u16(v);
    }
    d.extend_from_slice(&[u.unk_byte1, u.unk_byte2]);
    d.put_i16(q.starting_position);
    d.extend_from_slice(&[u.unk_byte3, u.unk_byte4]);
    d.put_u16(0x04b8);
    d.extend_from_slice(&[u.unk_byte7, u.unk_byte8]);
    for v in [
        q.general_enemy_level,
        q.wave_1_transition_type,
        q.wave_1_transition_target,
        q.wave_1_transition_quantity,
        q.wave_2_transition_type,
        q.wave_2_transition_target,
        q.wave_2_transition_quantity,
    ] {
        d.put_i16(v);
    }
    d.put_u32(u.unk_12);
    // The objectives again, each with its rewards; a sub-quest has one reward row, then an empty one.
    properties(&mut d, &o.main_quest);
    rewards(&mut d, &o.main_quest.rewards_row_1);
    rewards(&mut d, &o.main_quest.rewards_row_2);
    for sub in [&o.subquest_1, &o.subquest_2] {
        properties(&mut d, sub);
        match sub.kind {
            Some(_) => rewards(&mut d, &sub.rewards_row_1),
            None => d.put_bytes(0, 44),
        }
        d.put_bytes(0, 44);
    }
    ensure!(
        d.len() == QUEST_SIZE,
        "quest {}: fixed part is {:#x} bytes",
        q.name,
        d.len()
    );

    // The small monster tables, after their total size (MH3SP: a data file's own first 4 bytes are replaced by it;
    // a missing one leaves the size alone).
    let small = match (&q.smallmonster_data_file, &quest.small_monster_file) {
        (Some(_), Some(file)) => file[4..].to_vec(),
        (Some(_), None) => Vec::new(),
        (None, _) => small_monsters(quest)?,
    };
    d.put_u32((QUEST_SIZE + 4 + small.len()) as u32);
    d.extend_from_slice(&small);
    if let Some(sets) = &quest.arena_equipment {
        ensure!(
            (0xEA60..=0xEA6B).contains(&q.quest_id),
            "arena quest id {:#x} out of range",
            q.quest_id
        );
        arena(&mut d, sets)?;
    }
    Ok(d)
}

/// An objective: type, objective type and count; all zero when there is none.
fn properties(d: &mut Vec<u8>, o: &Objective) {
    match o.kind {
        Some(kind) => {
            d.put_u32(kind);
            d.put_i16(o.objective_type);
            d.put_i16(o.objective_num);
        }
        None => d.put_bytes(0, 8),
    }
}

/// A reward row: 11 slots of (item, amount, percent), the unused ones zero.
fn rewards(d: &mut Vec<u8>, row: &[(i16, u8, u8)]) {
    for slot in 0..11 {
        match row.get(slot) {
            Some(&(item, amount, percent)) => {
                d.put_i16(item);
                d.extend_from_slice(&[amount, percent]);
            }
            None => d.put_bytes(0, 4),
        }
    }
}

/// The small monster tables: three wave offsets, then (offset, length) per wave and area, then per area its
/// monsters (0x30 bytes each) and an empty record.
fn small_monsters(quest: &Quest) -> Result<Vec<u8>> {
    let q = &quest.quest_info;
    let areas = areas(q.location)
        .with_context(|| format!("quest {}: unknown location {}", q.name, q.location))?;
    let entries = &quest.small_monsters;
    ensure!(
        entries.len().is_multiple_of(areas),
        "quest {}: small monsters not a whole number of waves",
        q.name
    );
    let waves = entries.len() / areas;
    ensure!(waves <= 3, "quest {}: {waves} waves (3 at most)", q.name);
    let mut d = Vec::new();
    for v in [0x0C, 0x0C + 8 * areas as u32] {
        d.put_u32(v);
    }
    let third: u32 = if waves < 3 {
        0xCCCC_CCCC
    } else {
        0x0C + 16 * areas as u32
    };
    d.put_u32(third);
    let mut records = Vec::new();
    let mut offset = 0x0C + 8 * entries.len();
    for (entry, monsters) in entries.iter().enumerate() {
        let area = (entry % areas) as u8;
        let mut block = Vec::new();
        for m in monsters {
            if m.room != area {
                bail!(
                    "quest {}: a small monster of area {area} is in room {}",
                    q.name,
                    m.room
                );
            }
            block.put_u32(m.kind);
            block.extend_from_slice(&[
                0,
                0,
                0,
                m.quantity as u8,
                m.unk2,
                m.room,
                m.unk1,
                m.variant,
            ]);
            block.put_u32(0);
            for p in [m.pos_x, m.pos_y, m.pos_z] {
                block.put_f32(p as f32);
            }
            for r in [m.rot_x, m.rot_y, m.rot_z] {
                block.put_i32((180.05 * r) as i32);
            }
            block.put_u32(0xFF00_0000);
            block.put_u32(0);
        }
        block.put_bytes(0, 0x30);
        d.put_u32(offset as u32);
        d.put_u32(block.len() as u32);
        offset += block.len();
        records.extend_from_slice(&block);
    }
    d.extend_from_slice(&records);
    Ok(d)
}

/// Four arena sets: three weapons, five armour pieces (helm, chest, faulds, gauntlets, legs), 24 items, 8 pouch
/// slots.
fn arena(d: &mut Vec<u8>, sets: &[ArenaSet]) -> Result<()> {
    // Equipment classes.
    const CHEST: u16 = 1;
    const GAUNTLETS: u16 = 2;
    const FAULDS: u16 = 3;
    const LEGS: u16 = 4;
    const HELM: u16 = 5;
    ensure!(
        sets.len() >= 4,
        "arena equipment: {} sets, 4 needed",
        sets.len()
    );
    let slot = |d: &mut Vec<u8>, class: u16, id: u16, slots: u16| {
        d.extend_from_slice(&[slots as u8, class as u8]);
        d.put_u16(id);
    };
    let weapon = |d: &mut Vec<u8>, w: &[u16]| -> Result<()> {
        ensure!(
            (2..=3).contains(&w.len()),
            "arena weapon: (class, id[, slots]) expected"
        );
        slot(d, w[0], w[1], w.get(2).copied().unwrap_or(0));
        Ok(())
    };
    for (w1, w2, w3, helm, chest, gauntlets, faulds, legs, items, pouch) in &sets[..4] {
        weapon(d, w1)?;
        for w in [w2, w3] {
            match w {
                Some(w) => weapon(d, w)?,
                None => d.put_bytes(0xFF, 4),
            }
        }
        for (class, id) in [
            (HELM, helm),
            (CHEST, chest),
            (FAULDS, faulds),
            (GAUNTLETS, gauntlets),
            (LEGS, legs),
        ] {
            slot(d, class, *id, 0);
        }
        // 24 items and 8 pouch slots; a longer list is written whole, as MH3SP does (Grudge Match Wyvern Trio and
        // Land Lords have 9 in their last set's pouch: the 4 extra bytes end the binary).
        for (list, size) in [(items, 24), (pouch, 8)] {
            for i in 0..size.max(list.len()) {
                let (item, qty) = list.get(i).copied().unwrap_or((0, 0));
                d.put_u16(item);
                d.put_u16(qty);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every quest byte-identical to an MH3SP checkout's own generator (`tools/reference-content.py`): runs with
    /// `REFERENCE_DIR` (its output) and `CONTENT_DIR` (the checkout) set, else skipped.
    #[test]
    fn quests_match_the_reference_generator() {
        let (Some(reference), Some(content)) = (
            std::env::var_os("REFERENCE_DIR"),
            std::env::var_os("CONTENT_DIR"),
        ) else {
            return;
        };
        let mut checked = 0;
        let mut wrong = Vec::new();
        for entry in std::fs::read_dir(&reference).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let expected = std::fs::read(&path).unwrap();
            let quest = Path::new(&content)
                .join("event")
                .join(format!("{name}.json"));
            let got = match load(&quest).and_then(|q| binary(&q)) {
                Ok(got) => got,
                Err(e) => {
                    wrong.push(format!("{name}: {e:#}"));
                    continue;
                }
            };
            if got != expected {
                let at = got.iter().zip(&expected).position(|(a, b)| a != b);
                let at = at.unwrap_or(got.len().min(expected.len()));
                wrong.push(format!(
                    "{name}: {} bytes (expected {}), first difference at {at:#x}",
                    got.len(),
                    expected.len()
                ));
            }
            checked += 1;
        }
        wrong.sort();
        assert!(
            wrong.is_empty(),
            "{} of {checked}:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
}
