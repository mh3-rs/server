//! The hunter's public profile: the 256-byte block the console writes (`UserBinarySet`), in windows, and the server
//! hands to other consoles, which draw the hunter from it. A crafted block is a risk to other players' games, so the
//! known fields are parsed and validated, and the block others receive is rebuilt: the raw bytes with the known
//! fields written over them. As fields get mapped they move from raw to parsed (global roadmap, M3).

use anyhow::{Result, bail};
use bytes::{Buf, BufMut, Bytes};

/// The size of the block.
pub const SIZE: usize = 0x100;
/// The highest hunter rank in the game.
const MAX_HR: u16 = 999;
/// The highest weapon type. ponytail: a guess, every recording has 1 (a sword and shield, whose equipment class
/// is 8): read as the class minus 7 (great sword 7 … switch axe 15, MH3SP's `EquipmentClasses`); tighten once
/// recordings show other weapons.
const MAX_WEAPON: u8 = 15 - 7;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    raw: Box<[u8; SIZE]>,
    /// Hunter rank (u16 @0).
    pub hr: u16,
    /// Weapon type (u8 @5).
    pub weapon: u8,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            raw: Box::new([0; SIZE]),
            hr: 0,
            weapon: 0,
        }
    }
}

impl Profile {
    /// Parses and validates a whole block.
    pub fn parse(raw: &[u8; SIZE]) -> Result<Profile> {
        let hr = (&raw[0..]).get_u16();
        let weapon = raw[5];
        if hr > MAX_HR {
            bail!("hunter rank {hr} over {MAX_HR}");
        }
        if weapon > MAX_WEAPON {
            bail!("weapon type {weapon} over {MAX_WEAPON}");
        }
        Ok(Profile {
            raw: Box::new(*raw),
            hr,
            weapon,
        })
    }

    /// This profile with `data` written at `offset`, validated.
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<Profile> {
        let Some(end) = offset.checked_add(data.len()).filter(|&end| end <= SIZE) else {
            bail!("write past {SIZE} bytes ({offset}, {} bytes)", data.len());
        };
        let mut raw = self.bytes_array();
        raw[offset..end].copy_from_slice(data);
        Profile::parse(&raw)
    }

    /// The raw block as the console last wrote it (stored until every field is mapped).
    pub fn raw(&self) -> &[u8; SIZE] {
        &self.raw
    }

    /// The block other consoles receive: the raw bytes with the known fields written over them.
    pub fn bytes(&self) -> Bytes {
        Bytes::copy_from_slice(&self.bytes_array()[..])
    }

    fn bytes_array(&self) -> [u8; SIZE] {
        let mut block = *self.raw;
        (&mut block[0..]).put_u16(self.hr);
        block[5] = self.weapon;
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_validated_and_the_block_rebuilt() {
        let profile = Profile::default()
            .write(0, &[0x00, 0x01, 0, 0, 0, 0x01, 0x38])
            .unwrap();
        assert_eq!((profile.hr, profile.weapon), (1, 1));
        assert_eq!(profile.bytes()[..7], [0x00, 0x01, 0, 0, 0, 0x01, 0x38]);
        // A window elsewhere keeps the fields.
        let profile = profile.write(244, &[0xff; 7]).unwrap();
        assert_eq!(
            (profile.hr, profile.weapon, profile.bytes()[250]),
            (1, 1, 0xff)
        );
        // Invalid: refused, the caller keeps the previous profile.
        assert!(profile.write(0, &[0x03, 0xe8]).is_err());
        assert!(profile.write(5, &[MAX_WEAPON + 1]).is_err());
        assert!(profile.write(250, &[0; 7]).is_err());
        assert!(profile.write(usize::MAX, &[0]).is_err());
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
                    let mut raw = *profile.raw;
                    raw[offset..offset + len].copy_from_slice(p);
                    profile = profile.write(offset, p).unwrap();
                    assert_eq!(profile.bytes()[..], raw[..]);
                    writes += 1;
                }
            }
        }
        assert!(writes > 0, "no profile writes in {corpus}");
    }
}
