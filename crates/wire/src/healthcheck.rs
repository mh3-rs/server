//! The healthcheck of the PAT roles (OPN, LMP, FMP): `mh3sp check` sends [`REQUEST`] as its first frame, the server
//! answers [`ANSWER`] with the request's `seq`, both without a payload, and closes. Not the game's: the ids are
//! outside its catalogue (all `0x60` to `0x69`), so no console can send one, and the server answers it before any
//! handler starts (`server::net`).
//!
//! The same word, on UDP 27901, is `natneg::Body::Healthcheck`.

use crate::PacketId;

pub const REQUEST: PacketId = PacketId(0x7F00_0100);
pub const ANSWER: PacketId = PacketId(0x7F00_0200);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_are_outside_the_games_catalogue() {
        for id in [REQUEST, ANSWER] {
            assert!(id.entry().is_none(), "{id}");
            assert!(id.category().is_none(), "{id}");
        }
        assert!(!PacketId::all().any(|id| id == REQUEST || id == ANSWER));
    }

    #[test]
    fn the_answer_follows_the_requests_naming_rule() {
        // A request's answer id: kind byte 0x01 becomes 0x02 (`game::answer`).
        assert_eq!(ANSWER.0, REQUEST.0 & !0xff00 | 0x0200);
    }
}
