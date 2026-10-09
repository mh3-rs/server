//! The hunter's public profile: the 256-byte block the console writes (`UserBinarySet`), in windows, and the server
//! hands to other consoles, which draw the hunter from it.
//!
//! Every byte is mapped, from the game's code (`buildNetUserProfile` fills it from the save, `applyNetUserProfile`
//! is the game's own sanitiser; read in the PAL `main.dol`, same layout as RMHE08's symbols). A crafted block is a
//! risk to other players' games: profile notices reach them without the game's sanitiser, and some fields index
//! tables unchecked. So every write is validated against the game's rules (stricter: refused, not clamped), and the
//! block others receive is built from the parsed fields only. Fields whose meaning is unknown are save bytes the
//! game passes through: kept as they are, checked where the game checks them.

use anyhow::{Result, bail, ensure};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// The size of the block.
pub const SIZE: usize = 0x100;
/// The profile comment's field (NUL-terminated).
const COMMENT_SIZE: usize = 0x56;
/// Valid item ids per equipment kind (`< count`), kinds 1–15: the game's table (PAL 0x80570E80).
const ITEMS: [u16; 15] = [
    131, 125, 126, 125, 141, 8, 94, 92, 86, 98, 20, 21, 20, 44, 62,
];

/// Equipment kinds (MH3SP's `EquipmentClasses`, the game's numbering).
const CHEST: u8 = 1;
const ARMS: u8 = 2;
const WAIST: u8 = 3;
const LEGS: u8 = 4;
const HELMET: u8 = 5;
const TALISMAN: u8 = 6;
const SWORD_AND_SHIELD: u8 = 8;
const BOWGUN_FRAME: u8 = 11;
const BOWGUN_BARREL: u8 = 12;
const BOWGUN_STOCK: u8 = 13;
/// What each slot holds, after the weapon and the two bowgun parts.
const ARMOUR: [u8; 5] = [HELMET, CHEST, WAIST, ARMS, LEGS];

/// An equipment slot (`_EQUIP`, 12 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slot {
    pub kind: u8,
    pub deco_count: u8,
    pub item: u16,
    pub deco_level: u16,
    pub skills: [u16; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Hunter rank (@0x00).
    pub hr: u16,
    /// Appearance choices whose meaning is unknown (face, hair…), with the game's bounds: @0x02 (< 11),
    /// @0x03 (< 4), @0x96 (< 9), @0x98 (< 10), @0x99 (< 13).
    pub appearance: [u8; 5],
    /// @0x04: a save byte (≤ 3).
    pub unknown_04: u8,
    /// Weapon type (@0x05): a blade's kind − 7 (0 great sword … 8 switch axe), 4–6 a bowgun by weight.
    pub weapon: u8,
    /// RGBA (@0x08, @0x0C).
    pub hair_colour: u32,
    pub inner_colour: u32,
    /// Weapon, bowgun barrel and stock, helmet, chest, waist, arms, legs, talisman (@0x10).
    pub equipment: [Slot; 9],
    /// Save bytes the game doesn't check (@0x7C: 5, @0x81: 17, @0x92, @0x93).
    pub unknown_7c: [u8; 5],
    pub unknown_81: [u8; 17],
    pub unknown_92: u8,
    pub unknown_93: u8,
    /// @0x94.
    pub poogie_costume: u8,
    /// @0x95: 0 male, 1 female.
    pub gender: u8,
    /// @0x97 (< 20).
    pub voice: u8,
    /// A save byte pair (@0x9A ≤ 2, @0x9B ≤ 100).
    pub unknown_9a: [u8; 2],
    /// The profile comment (@0x9C), without its NUL: as the game sends it (may be Shift-JIS).
    pub comment: Bytes,
    /// @0xF2 (≤ 117), @0xF3 (≤ 8).
    pub title: u8,
    pub status: u8,
    /// Where the player is, not who they are (not stored): the city's settings (@0xF4, all 0xFF outside a city:
    /// unknown, HR limit, goal, seeking), the server type (@0xF8, 0-based), in a party (@0xF9).
    pub city: [u8; 4],
    pub server_type: u8,
    pub in_party: u8,
}

impl Default for Profile {
    /// The game's default hunter (the sanitiser's defaults; every recording's block until the save loads).
    fn default() -> Self {
        let mut equipment = [Slot::default(); 9];
        equipment[0] = Slot {
            kind: SWORD_AND_SHIELD,
            item: 1,
            ..Slot::default()
        };
        for (slot, kind) in equipment[3..]
            .iter_mut()
            .zip(ARMOUR.iter().chain([&TALISMAN]))
        {
            slot.kind = *kind;
        }
        Profile {
            hr: 0,
            appearance: [0; 5],
            unknown_04: 0,
            weapon: SWORD_AND_SHIELD - 7,
            hair_colour: 0x381a_00ff,
            inner_colour: 0xffff_ffff,
            equipment,
            unknown_7c: [0, 0xff, 0xff, 0xff, 0xff],
            unknown_81: [0xff; 17],
            unknown_92: 0,
            unknown_93: 0,
            poogie_costume: 0,
            gender: 0,
            voice: 0,
            unknown_9a: [2, 0],
            comment: Bytes::new(),
            title: 0,
            status: 0,
            city: [0xff; 4],
            server_type: 0,
            in_party: 0,
        }
    }
}

impl Profile {
    /// Parses and validates a whole block.
    pub fn parse(block: &[u8; SIZE]) -> Result<Profile> {
        let mut b = &block[..];
        let hr = b.get_u16();
        let (a02, a03, unknown_04, weapon) = (b.get_u8(), b.get_u8(), b.get_u8(), b.get_u8());
        ensure!(b.get_u16() == 0, "bytes 0x06–0x07 not zero");
        let (hair_colour, inner_colour) = (b.get_u32(), b.get_u32());
        let mut equipment = [Slot::default(); 9];
        for slot in &mut equipment {
            *slot = Slot {
                kind: b.get_u8(),
                deco_count: b.get_u8(),
                item: b.get_u16(),
                deco_level: b.get_u16(),
                skills: [b.get_u16(), b.get_u16(), b.get_u16()],
            };
        }
        let mut unknown_7c = [0; 5];
        b.copy_to_slice(&mut unknown_7c);
        let mut unknown_81 = [0; 17];
        b.copy_to_slice(&mut unknown_81);
        let (unknown_92, unknown_93, poogie_costume) = (b.get_u8(), b.get_u8(), b.get_u8());
        let (gender, a96, voice, a98, a99) =
            (b.get_u8(), b.get_u8(), b.get_u8(), b.get_u8(), b.get_u8());
        let unknown_9a = [b.get_u8(), b.get_u8()];
        let field = &b[..COMMENT_SIZE];
        b.advance(COMMENT_SIZE);
        // What follows the NUL is left over from older comments in the save: dropped (rebuilt as zeros).
        let Some(end) = field.iter().position(|&c| c == 0) else {
            bail!("comment without its NUL");
        };
        let comment = Bytes::copy_from_slice(&field[..end]);
        let (title, status) = (b.get_u8(), b.get_u8());
        let mut city = [0; 4];
        b.copy_to_slice(&mut city);
        let (server_type, in_party) = (b.get_u8(), b.get_u8());
        ensure!(b.iter().all(|&c| c == 0), "bytes 0xFA–0xFF not zero");
        let profile = Profile {
            hr,
            appearance: [a02, a03, a96, a98, a99],
            unknown_04,
            weapon,
            hair_colour,
            inner_colour,
            equipment,
            unknown_7c,
            unknown_81,
            unknown_92,
            unknown_93,
            poogie_costume,
            gender,
            voice,
            unknown_9a,
            comment,
            title,
            status,
            city,
            server_type,
            in_party,
        };
        profile.validate()?;
        Ok(profile)
    }

    /// The game's rules for every field it reads (`applyNetUserProfile`, and what it indexes unchecked).
    fn validate(&self) -> Result<()> {
        ensure!(self.hr <= 999, "hunter rank {}", self.hr);
        for (value, bound, at) in [
            (self.appearance[0], 11, 0x02),
            (self.appearance[1], 4, 0x03),
            (self.unknown_04, 4, 0x04),
            (self.gender, 2, 0x95),
            (self.appearance[2], 9, 0x96),
            (self.voice, 20, 0x97),
            (self.appearance[3], 10, 0x98),
            (self.appearance[4], 13, 0x99),
            (self.unknown_9a[0], 3, 0x9a),
            (self.unknown_9a[1], 101, 0x9b),
            (self.title, 118, 0xf2),
            (self.status, 9, 0xf3),
            (self.server_type, 4, 0xf8),
            (self.in_party, 2, 0xf9),
        ] {
            ensure!(value < bound, "byte {at:#04x}: {value} (< {bound})");
        }
        // HR limit, goal, seeking: an index into the server's lists (content binary 0x01), or 0xFF outside a city.
        for (value, bound, at) in [
            (self.city[1], 8, 0xf5),
            (self.city[2], 64, 0xf6),
            (self.city[3], 32, 0xf7),
        ] {
            ensure!(
                value < bound || value == 0xff,
                "byte {at:#04x}: {value} (< {bound})"
            );
        }
        // A trailing Shift-JIS lead byte would swallow the NUL in the game's text filter.
        if let Some(&last) = self.comment.last() {
            ensure!(
                !matches!(last, 0x81..=0x9f | 0xe0..=0xef),
                "comment ends with a Shift-JIS lead byte"
            );
        }
        self.validate_equipment()
    }

    fn validate_equipment(&self) -> Result<()> {
        let known = |slot: &Slot| {
            ITEMS
                .get(usize::from(slot.kind).wrapping_sub(1))
                .is_some_and(|&count| slot.item < count)
        };
        let [weapon, barrel, stock, armour @ .., talisman] = &self.equipment;
        ensure!(
            matches!(weapon.kind, 7..=11 | 14 | 15) && known(weapon),
            "weapon: kind {}, item {}",
            weapon.kind,
            weapon.item
        );
        // The weapon type is derived from the weapon: a bowgun's from its parts' weight (4–6), not checked here.
        let expected = match weapon.kind {
            BOWGUN_FRAME => 4..=6,
            kind => kind - 7..=kind - 7,
        };
        ensure!(
            expected.contains(&self.weapon),
            "weapon type {} for weapon kind {}",
            self.weapon,
            weapon.kind
        );
        if weapon.kind == BOWGUN_FRAME {
            for (part, kind) in [(barrel, BOWGUN_BARREL), (stock, BOWGUN_STOCK)] {
                ensure!(
                    part.kind == kind && known(part),
                    "bowgun part: kind {}",
                    part.kind
                );
            }
        } else {
            ensure!(
                *barrel == Slot::default() && *stock == Slot::default(),
                "bowgun parts without a bowgun"
            );
        }
        for (slot, kind) in armour.iter().zip(ARMOUR) {
            // Item 0: nothing worn there.
            ensure!(
                slot.kind == kind && known(slot),
                "armour slot of kind {kind}: kind {}, item {}",
                slot.kind,
                slot.item
            );
        }
        for slot in [weapon, barrel, stock].into_iter().chain(armour) {
            ensure!(slot.deco_count <= 3, "{} decorations", slot.deco_count);
        }
        ensure!(
            talisman.kind == TALISMAN && known(talisman) && talisman.deco_count <= 3,
            "talisman: kind {}, item {}, {} slots",
            talisman.kind,
            talisman.item,
            talisman.deco_count
        );
        // The game's check of a talisman's skills: the two from its slot count on (0xCE skills).
        for skill in talisman
            .skills
            .iter()
            .skip(talisman.deco_count.into())
            .take(2)
        {
            ensure!(*skill < 0xce, "talisman skill {skill}");
        }
        Ok(())
    }

    /// This profile with `data` written at `offset`, validated.
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<Profile> {
        let Some(end) = offset.checked_add(data.len()).filter(|&end| end <= SIZE) else {
            bail!("write past {SIZE} bytes ({offset}, {} bytes)", data.len());
        };
        let mut block = [0; SIZE];
        block.copy_from_slice(&self.bytes());
        block[offset..end].copy_from_slice(data);
        Profile::parse(&block)
    }

    /// The block other consoles receive, built from the fields.
    pub fn bytes(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(SIZE);
        b.put_u16(self.hr);
        b.put_slice(&[
            self.appearance[0],
            self.appearance[1],
            self.unknown_04,
            self.weapon,
        ]);
        b.put_u16(0);
        b.put_u32(self.hair_colour);
        b.put_u32(self.inner_colour);
        for slot in &self.equipment {
            b.put_u8(slot.kind);
            b.put_u8(slot.deco_count);
            b.put_u16(slot.item);
            b.put_u16(slot.deco_level);
            for skill in slot.skills {
                b.put_u16(skill);
            }
        }
        b.put_slice(&self.unknown_7c);
        b.put_slice(&self.unknown_81);
        b.put_slice(&[
            self.unknown_92,
            self.unknown_93,
            self.poogie_costume,
            self.gender,
        ]);
        b.put_slice(&[
            self.appearance[2],
            self.voice,
            self.appearance[3],
            self.appearance[4],
        ]);
        b.put_slice(&self.unknown_9a);
        b.put_slice(&self.comment);
        b.put_bytes(0, COMMENT_SIZE - self.comment.len());
        b.put_slice(&[self.title, self.status]);
        b.put_slice(&self.city);
        b.put_slice(&[self.server_type, self.in_party]);
        b.put_bytes(0, SIZE - b.len());
        b.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_recorded_default_hunter() {
        let block = Profile::default().bytes();
        assert_eq!(block.len(), SIZE);
        assert_eq!(
            Profile::parse(&block[..].try_into().unwrap()).unwrap(),
            Profile::default()
        );
        assert_eq!(
            hex::encode(&block[..0x14]),
            "0000000000010000381a00ffffffffff08000001"
        );
    }

    #[test]
    fn writes_are_validated() {
        let profile = Profile::default().write(0, &[0x00, 0x05]).unwrap();
        assert_eq!(profile.hr, 5);
        // A comment, then a window elsewhere keeps it.
        let profile = profile.write(0x9c, b"Hello\0").unwrap();
        let profile = profile
            .write(0xf4, &[0xff, 0xff, 0xff, 0x03, 0, 1])
            .unwrap();
        assert_eq!(
            (&profile.comment[..], profile.city[3], profile.in_party),
            (&b"Hello"[..], 3, 1)
        );
        // Refused: the caller keeps the previous profile.
        for (offset, data) in [
            (0, &[0x03, 0xe8][..]),                // HR 1000
            (5, &[2]),                             // a hammer's weapon type with a sword and shield
            (0x10, &[SWORD_AND_SHIELD, 0, 0, 92]), // an item past the table
            (0x1c, &[BOWGUN_BARREL]),              // a barrel without a bowgun
            (0x34, &[CHEST]),                      // a chest piece in the helmet slot
            (0x95, &[2]),                          // gender
            (0x9c, &[b'a'; COMMENT_SIZE]),         // a comment without its NUL
            (0x9c, &[b'a', 0x82, 0]),              // a dangling Shift-JIS lead byte
            (0xf6, &[64]),                         // goal
            (0xfa, &[1]),                          // past the last field
            (0xff, &[0, 0]),                       // past the block
            (usize::MAX, &[0]),
        ] {
            assert!(
                profile.write(offset, data).is_err(),
                "{offset:#x}: {data:?}"
            );
        }
    }

    #[test]
    fn a_bowgun_needs_its_parts() {
        let mut block = [0; SIZE];
        block.copy_from_slice(&Profile::default().bytes());
        block[5] = 5;
        block[0x10..0x14].copy_from_slice(&[BOWGUN_FRAME, 0, 0, 3]);
        assert!(Profile::parse(&block).is_err());
        block[0x1c..0x20].copy_from_slice(&[BOWGUN_BARREL, 0, 0, 2]);
        block[0x28..0x2c].copy_from_slice(&[BOWGUN_STOCK, 0, 0, 4]);
        assert_eq!(Profile::parse(&block).unwrap().weapon, 5);
    }

    /// Every profile the recorded consoles wrote is accepted, and rebuilt byte for byte.
    #[test]
    fn recorded_profiles_are_valid() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus");
        let mut writes = 0;
        for scenario in std::fs::read_dir(corpus).unwrap() {
            let Ok(files) = std::fs::read_dir(scenario.unwrap().path().join("tcp")) else {
                continue;
            };
            for file in files {
                let mut profile = Profile::default();
                for line in std::fs::read_to_string(file.unwrap().path())
                    .unwrap()
                    .lines()
                {
                    let frame: serde_json::Value = serde_json::from_str(line).unwrap();
                    if frame["id"] != "66310100" {
                        continue;
                    }
                    let p = hex::decode(frame["payload"].as_str().unwrap()).unwrap();
                    let mut p = &p[..];
                    let offset = p.get_u32() as usize;
                    let len = usize::from(p.get_u16());
                    let p = &p[..len];
                    let mut expected = BytesMut::from(&profile.bytes()[..]);
                    expected[offset..offset + len].copy_from_slice(p);
                    profile = profile.write(offset, p).unwrap();
                    assert_eq!(profile.bytes()[..], expected[..]);
                    writes += 1;
                }
            }
        }
        assert!(writes > 0, "no profile writes in {corpus}");
    }
}
