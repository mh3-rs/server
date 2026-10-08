use std::fmt;

use crate::catalogue::CATALOGUE;

/// A PAT packet id: `0xCCNNKK00` (category, message number, kind, always-zero low byte).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PacketId(pub u32);

/// The family a packet belongs to (the id's high byte).
///
/// Categories are not servers: the game sends `Rfp` packets to LMP, for instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Opn,
    Rfp,
    Lmp,
    Fmp,
    Layer,
    Circle,
    User,
    Agreement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Request, answered by the matching `Ans`.
    Req,
    /// Answer to a `Req`.
    Ans,
    /// Notification, no answer expected.
    Ntc,
}

/// A catalogue entry: the game's own name for the packet and its description.
#[derive(Debug)]
pub struct Entry {
    pub id: u32,
    /// Unique identifier (MH3SP's `PatID4` naming).
    pub name: &'static str,
    /// The game's Japanese debug name.
    pub japanese: &'static str,
    pub english: &'static str,
}

impl PacketId {
    pub fn category(self) -> Option<Category> {
        Some(match self.0 >> 24 {
            0x60 => Category::Opn,
            0x61 => Category::Rfp,
            0x62 => Category::Lmp,
            0x63 => Category::Fmp,
            0x64 => Category::Layer,
            0x65 => Category::Circle,
            0x66 => Category::User,
            0x69 => Category::Agreement,
            _ => return None,
        })
    }

    pub fn kind(self) -> Option<Kind> {
        Some(match (self.0 >> 8) & 0xff {
            0x01 => Kind::Req,
            0x02 => Kind::Ans,
            0x10 => Kind::Ntc,
            _ => return None,
        })
    }

    /// The catalogue entry, if the game knows this id.
    pub fn entry(self) -> Option<&'static Entry> {
        CATALOGUE
            .binary_search_by_key(&self.0, |e| e.id)
            .ok()
            .map(|i| &CATALOGUE[i])
    }

    pub fn name(self) -> Option<&'static str> {
        self.entry().map(|e| e.name)
    }

    /// Every known packet id, sorted.
    pub fn all() -> impl Iterator<Item = PacketId> {
        CATALOGUE.iter().map(|e| PacketId(e.id))
    }
}

impl fmt::Debug for PacketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// `ReqLineCheck(60010100)`, or `Unknown(12345678)`.
impl fmt::Display for PacketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({:08x})", self.name().unwrap_or("Unknown"), self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_sorted_unique_and_well_formed() {
        assert_eq!(CATALOGUE.len(), 301);
        assert!(CATALOGUE.windows(2).all(|w| w[0].id < w[1].id));
        let mut names: Vec<_> = CATALOGUE.iter().map(|e| e.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CATALOGUE.len(), "names must be unique");
        for id in PacketId::all() {
            assert_eq!(id.0 & 0xff, 0, "{id}: low byte must be zero");
            assert!(id.category().is_some(), "{id}: unknown category");
            let kind = id.kind().unwrap_or_else(|| panic!("{id}: unknown kind"));
            // The game's own table names these two `recvNtcLayerJump{Ready,Go}` although their kind
            // byte is Ans and their Japanese descriptions say "reply" (返答): a naming slip kept as is.
            if matches!(id.0, 0x6416_0200 | 0x6417_0200) {
                continue;
            }
            let prefix = format!("{kind:?}");
            assert!(
                id.name().unwrap().starts_with(&prefix),
                "{id}: kind and name disagree"
            );
        }
    }

    #[test]
    fn lookup_and_display() {
        let id = PacketId(0x6081_0100);
        assert_eq!(id.name(), Some("ReqMemoryCheck"));
        assert_eq!(id.category(), Some(Category::Opn));
        assert_eq!(id.kind(), Some(Kind::Req));
        assert_eq!(id.to_string(), "ReqMemoryCheck(60810100)");
        assert_eq!(PacketId(0x6014_0100).to_string(), "Unknown(60140100)");
        // The LMP-stage and FMP-stage server list packets have distinct names.
        assert_eq!(PacketId(0x6134_0100).name(), Some("ReqFmpInfo"));
        assert_eq!(PacketId(0x6314_0100).name(), Some("ReqFmpInfo2"));
    }
}
