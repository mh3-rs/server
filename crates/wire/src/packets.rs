//! Typed packets: one struct per message, decoded from and encoded to its payload byte for byte.
//!
//! Field types map to the wire as: `u8`/`u16`/`u32`/`u64` big-endian, `Bytes` a lp2 string, `PatData` a record,
//! `Vec<u8>` a byte list (u8 count, then the bytes), `Vec<PatData>` a record list (u32 count, then the records),
//! `Vec<(PatData, OptionalFields)>` the same with optional fields after each record, [`ShortList`] a record list
//! with a u8 count, `OptionalFields`, and [`Rest`] the bytes left (opaque).
//! Fields whose meaning is unknown are kept as `unk*` so re-encoding gives back the same bytes.
//! Layouts come from MH3SP and are checked against every recorded payload (`testkit/tests/corpus_roundtrip.rs`);
//! the few never recorded say so.

use bytes::Bytes;

use crate::{DecodeError, EncodeError, OptionalFields, PacketId, PatData, PatWrite, Reader};

pub trait Packet: Sized {
    const ID: PacketId;
    /// Decodes the whole payload: trailing bytes are an error.
    fn decode(payload: &[u8]) -> Result<Self, DecodeError>;
    fn encode(&self, out: &mut impl PatWrite) -> Result<(), EncodeError>;
}

/// How a field type reads and writes itself.
trait Field: Sized {
    fn read(r: &mut Reader) -> Result<Self, DecodeError>;
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError>;
}

macro_rules! int_field {
    ($($t:ident $put:ident),*) => {$(
        impl Field for $t {
            fn read(r: &mut Reader) -> Result<Self, DecodeError> {
                r.$t()
            }
            fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
                out.$put(*self);
                Ok(())
            }
        }
    )*};
}

int_field!(u8 put_u8, u16 put_u16, u32 put_u32, u64 put_u64);

impl Field for Bytes {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        r.lp2_string()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_lp2_string(self)
    }
}

impl Field for PatData {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        r.pat_data()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_pat_data(self)
    }
}

impl Field for Vec<u8> {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        r.byte_list()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_byte_list(self)
    }
}

impl Field for Vec<PatData> {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        (0..r.u32()?).map(|_| r.pat_data()).collect()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_u32(count(self.len(), u32::MAX as usize)? as u32);
        self.iter().try_for_each(|d| out.put_pat_data(d))
    }
}

impl Field for OptionalFields {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        r.optional_fields()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_optional_fields(self)
    }
}

impl Field for Vec<(PatData, OptionalFields)> {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        (0..r.u32()?)
            .map(|_| Ok((r.pat_data()?, r.optional_fields()?)))
            .collect()
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_u32(count(self.len(), u32::MAX as usize)? as u32);
        self.iter().try_for_each(|(d, o)| {
            out.put_pat_data(d)?;
            out.put_optional_fields(o)
        })
    }
}

/// A record list with a u8 count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShortList(pub Vec<PatData>);

impl Field for ShortList {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(ShortList(
            (0..r.u8()?)
                .map(|_| r.pat_data())
                .collect::<Result<_, _>>()?,
        ))
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_u8(count(self.0.len(), u8::MAX as usize)? as u8);
        self.0.iter().try_for_each(|d| out.put_pat_data(d))
    }
}

/// The bytes left in the payload, kept as they are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rest(pub Bytes);

impl Field for Rest {
    fn read(r: &mut Reader) -> Result<Self, DecodeError> {
        let n = r.rest().len();
        Ok(Rest(Bytes::copy_from_slice(r.bytes(n)?)))
    }
    fn write(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
        out.put_slice(&self.0);
        Ok(())
    }
}

fn count(len: usize, max: usize) -> Result<usize, EncodeError> {
    if len > max {
        return Err(EncodeError::TooLong { len, max });
    }
    Ok(len)
}

macro_rules! packets {
    ($(
        $(#[$meta:meta])*
        $name:ident = $id:literal { $($(#[$fmeta:meta])* $field:ident: $ty:ty),* $(,)? }
    )*) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct $name { $($(#[$fmeta])* pub $field: $ty),* }

        impl Packet for $name {
            const ID: PacketId = PacketId($id);

            fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
                #[allow(unused_mut)]
                let mut r = Reader::new(payload);
                let p = Self { $($field: Field::read(&mut r)?),* };
                r.finish()?;
                Ok(p)
            }

            #[allow(unused_variables)]
            fn encode(&self, out: &mut impl PatWrite) -> Result<(), EncodeError> {
                $(self.$field.write(out)?;)*
                Ok(())
            }
        }
    )*

        /// Every typed packet's id and name.
        pub const TYPED: &[(PacketId, &str)] = &[$((PacketId($id), stringify!($name))),*];

        /// Decodes a payload with the packet type of `id` and encodes it again (`None`: not a typed packet). For
        /// the corpus tests: a layout is right when every recorded payload comes back byte for byte.
        #[doc(hidden)]
        pub fn reencode(id: PacketId, payload: &[u8]) -> Option<Result<Vec<u8>, String>> {
            fn run<P: Packet>(payload: &[u8]) -> Result<Vec<u8>, String> {
                let mut out = Vec::new();
                P::decode(payload).map_err(|e| e.to_string())?.encode(&mut out).map_err(|e| e.to_string())?;
                Ok(out)
            }
            match id.0 {
                $($id => Some(run::<$name>(payload)),)*
                _ => None,
            }
        }
    };
}

packets! {
    // Every role.
    ReqLineCheck = 0x60010100 {}
    AnsLineCheck = 0x60010200 {}
    /// Sent by the console when it leaves a connection.
    ReqShut = 0x60100100 {
        /// 1: full logout, 2: moving on to another server (per MH3SP, unconfirmed).
        kind: u8,
    }
    /// Echoes `ReqShut::kind`.
    AnsShut = 0x60100200 { kind: u8 }
    ReqConnection = 0x60200100 { unk: u32 }
    AnsConnection = 0x60200200 {
        /// Support code (0x01), PAT ticket (0x02), media version (0x07), country and language codes, …
        data: PatData,
    }
    /// Which server the console is talking to: OPN 5, LMP 1 or 2, FMP 3; 2 on OPN makes it ask `ReqMaintenance`.
    NtcLogin = 0x60211000 { status: u8 }
    /// Reads `size` bytes at `address` of the console's memory (the disc's NAT negotiation hostname).
    ReqMemoryCheck = 0x60810100 { data: PatData, address: u32, size: u32 }
    AnsMemoryCheck = 0x60810200 { data: PatData, address: u32, memory: Bytes }
    /// Answered with `AnsAuthenticationToken`, as MH3SP does; `AnsCommonKey` is never sent.
    ReqCommonKey = 0x60700100 {}

    // OPN.
    /// Asks the console for an LMP address.
    ReqLmpConnect = 0x62010100 {}
    AnsLmpConnect = 0x62010200 { address: Bytes, port: u16 }
    ReqTermsVersion = 0x62100100 {}
    AnsTermsVersion = 0x62100200 { version: u32, size: u32 }
    ReqTerms = 0x62110100 { version: u32, offset: u32, size: u32 }
    AnsTerms = 0x62110200 { offset: u32, size: u32, chunk: Bytes }
    ReqSubTermsInfo = 0x62130100 { unk: u8 }
    AnsSubTermsInfo = 0x62130200 { unk1: u32, unk: u8, size: u32 }
    ReqSubTerms = 0x62140100 { version: u32, unk: u8, offset: u32, size: u32 }
    AnsSubTerms = 0x62140200 { unk: u8, offset: u32, size: u32, chunk: Bytes }
    /// Never recorded: empty per MH3SP, which ignores the payload.
    ReqMaintenance = 0x62200100 {}
    /// Never recorded: the text the console shows when refused (`NtcLogin` status 2).
    AnsMaintenance = 0x62200200 { text: Bytes }
    ReqAnnounce = 0x62300100 {}
    AnsAnnounce = 0x62300200 { text: Bytes }
    ReqNoCharge = 0x62310100 {}
    AnsNoCharge = 0x62310200 { text: Bytes }
    ReqVulgarityInfoLow = 0x62560100 { info: u32 }
    AnsVulgarityInfoLow = 0x62560200 { unk: u32, info: u32, size: u32 }
    ReqAuthenticationToken = 0x62600100 { token: Bytes }
    AnsAuthenticationToken = 0x62600200 {}

    // LMP.
    ReqServerTime = 0x60020100 { country: u32 }
    /// Seconds since the epoch, twice (a clock, and the time for the Wii Shop's tickets, per MH3SP).
    AnsServerTime = 0x60020200 { server_time: u32, current_time: u32 }
    ReqTicket = 0x60300100 {}
    /// The PAT ticket: the session's key on the next connection.
    AnsTicket = 0x60300200 { ticket: Bytes }
    /// The Nintendo login's token (0x0a) and a state (0x09).
    ReqLoginInfo = 0x61010100 { data: PatData }
    /// `data` carries the console's online support code (0x07), issued here to a first-time console.
    AnsLoginInfo = 0x61010200 { need_ticket: u8, unk: Bytes, data: PatData }
    /// The Capcom ID slots, `count` from `first` (1-based); `fields` lists the record fields wanted.
    ReqUserListHead = 0x61100100 { first: u32, count: u32, fields: Vec<u8> }
    AnsUserListHead = 0x61100200 { first: u32, count: u32 }
    ReqUserListData = 0x61110100 { first: u32, count: u32 }
    /// One record per slot: index (0x01), Capcom ID (0x02, `******` when empty), hunter name (0x03).
    AnsUserListData = 0x61110200 { first: u32, users: Vec<PatData> }
    ReqUserListFoot = 0x61120100 {}
    AnsUserListFoot = 0x61120200 {}
    /// The chosen slot and the hunter playing it (name 0x03, …).
    ReqUserObject = 0x61200100 { unk: u8, slot: u32, user: PatData }
    /// The request's record with the slot's Capcom ID (0x02).
    AnsUserObject = 0x61200200 { need_ticket: u8, unk: Bytes, unk2: u32, user: PatData }
    ReqFmpListVersion = 0x61300100 {}
    AnsFmpListVersion = 0x61300200 { version: u32 }
    ReqFmpListHead = 0x61310100 { version: u32, first: u32, count: u32, fields: Vec<u8> }
    AnsFmpListHead = 0x61310200 { unk: u32, count: u32 }
    ReqFmpListData = 0x61320100 { first: u32, count: u32 }
    /// One record per world server: index, address, port, type, population, capacity, name, …
    AnsFmpListData = 0x61320200 { unk: u32, servers: Vec<PatData> }
    ReqFmpListFoot = 0x61330100 {}
    AnsFmpListFoot = 0x61330200 {}
    /// The FMP address (0x02) and port (0x03) of world server `index`: `fields` lists the fields wanted.
    ReqFmpInfo = 0x61340100 { index: u32, fields: Vec<u8> }
    AnsFmpInfo = 0x61340200 { data: PatData }

    // FMP: content downloads.
    /// `kind`: the content type (`0x01`–`0x0f` the English set: server types, time events, quests, …).
    ReqBinaryVersion = 0x63010100 { kind: u8 }
    AnsBinaryVersion = 0x63010200 { unk: u8, version: u32 }
    ReqBinaryHead = 0x63020100 { kind: u8 }
    AnsBinaryHead = 0x63020200 { version: u32, size: u32 }
    ReqBinaryData = 0x63030100 { kind: u8, version: u32, offset: u32, size: u32 }
    AnsBinaryData = 0x63030200 { version: u32, offset: u32, size: u32, chunk: Bytes }
    ReqBinaryFoot = 0x63040100 { kind: u8 }
    AnsBinaryFoot = 0x63040200 {}
    // FMP: the world server list, as LMP's (the `2` versions are the ones the console shows).
    ReqFmpListVersion2 = 0x63100100 {}
    AnsFmpListVersion2 = 0x63100200 { version: u32 }
    ReqFmpListHead2 = 0x63110100 { version: u32, first: u32, count: u32, fields: Vec<u8> }
    AnsFmpListHead2 = 0x63110200 { unk: u32, count: u32 }
    ReqFmpListData2 = 0x63120100 { first: u32, count: u32 }
    AnsFmpListData2 = 0x63120200 { unk: u32, servers: Vec<PatData> }
    ReqFmpListFoot2 = 0x63130100 {}
    AnsFmpListFoot2 = 0x63130200 {}
    ReqFmpInfo2 = 0x63140100 { index: u32, fields: Vec<u8> }
    AnsFmpInfo2 = 0x63140200 { data: PatData }
    // FMP: layers (world server, gates, cities).
    ReqLayerStart = 0x64010100 { fields: Vec<u8>, fields2: Vec<u8> }
    AnsLayerStart = 0x64010200 { layer: PatData }
    ReqLayerEnd = 0x64020100 {}
    AnsLayerEnd = 0x64020200 {}
    ReqLayerChildListHead = 0x64240100 { first: u32, count: u32, fields: Vec<u8> }
    AnsLayerChildListHead = 0x64240200 { unk: u32, count: u32 }
    ReqLayerChildListData = 0x64250100 { first: u32, count: u32 }
    AnsLayerChildListData = 0x64250200 { first: u32, children: Vec<(PatData, OptionalFields)> }
    ReqLayerChildListFoot = 0x64260100 {}
    AnsLayerChildListFoot = 0x64260200 {}
    /// `rest`: a list of (field, byte) pairs, meaning unknown.
    ReqLayerChildInfo = 0x64230100 { unk: u16, fields: Vec<u8>, rest: Rest }
    AnsLayerChildInfo = 0x64230200 { unk: u16, layer: PatData, unk2: u16 }
    ReqLayerDown = 0x64140100 { layer: u16, info: PatData }
    AnsLayerDown = 0x64140200 { layer: u16 }
    ReqLayerUp = 0x64150100 {}
    AnsLayerUp = 0x64150200 {}
    ReqLayerCreateHead = 0x64110100 { number: u16 }
    AnsLayerCreateHead = 0x64110200 { number: u16 }
    ReqLayerCreateSet = 0x64120100 { number: u16, layer: PatData, extra: OptionalFields }
    AnsLayerCreateSet = 0x64120200 { number: u16 }
    ReqLayerCreateFoot = 0x64130100 { number: u16, unk: u8 }
    AnsLayerCreateFoot = 0x64130200 { number: u16 }
    /// `layer`: a layer path (depth, world server, …), 14 bytes.
    ReqLayerHost = 0x64410100 { layer: Bytes }
    AnsLayerHost = 0x64410200 { layer: Bytes, capcom_id: Bytes, name: Bytes }
    NtcLayerHost = 0x64411000 { layer: Bytes, capcom_id: Bytes, name: Bytes }
    ReqLayerUserList = 0x64630100 { fields: Vec<u8> }
    AnsLayerUserList = 0x64630200 { users: Vec<PatData> }
    ReqLayerUserListHead = 0x64640100 { unk: u8, layer: Bytes, first: u32, count: u32, fields: Vec<u8> }
    AnsLayerUserListHead = 0x64640200 { first: u32, count: u32 }
    ReqLayerUserListData = 0x64650100 { first: u32, count: u32 }
    AnsLayerUserListData = 0x64650200 { unk: u32, users: Vec<(PatData, OptionalFields)> }
    ReqLayerUserListFoot = 0x64660100 {}
    AnsLayerUserListFoot = 0x64660200 {}
    ReqLayerMediationList = 0x64820100 { unk: u8, unk2: u8, fields: Vec<u8> }
    AnsLayerMediationList = 0x64820200 { unk: u8, items: ShortList }
    NtcLayerIn = 0x64141000 { capcom_id: Bytes, user: PatData }
    NtcLayerOut = 0x64151000 { capcom_id: Bytes }
    /// The console's position in town; relayed with the sender's Capcom ID in front (a layout per direction).
    NtcLayerUserPosition = 0x64711000 { data: Rest }
    /// Game data relayed to the layer (a layout per direction).
    NtcLayerBinary = 0x64701000 { data: Rest }
    /// Game data for one player (`capcom_id`: the partner from the console, the sender from the server).
    NtcLayerBinary2 = 0x64751000 { capcom_id: Bytes, data: Rest }
    // FMP: circles (quests).
    ReqCircleInfoNoticeSet = 0x65800100 { fields: Vec<u8>, fields2: Vec<u8> }
    AnsCircleInfoNoticeSet = 0x65800200 {}
    /// `rest`: a list of (field, byte) pairs, meaning unknown.
    ReqCircleListLayer = 0x65270100 { fields: Vec<u8>, rest: Rest }
    AnsCircleListLayer = 0x65270200 { unk: u32, circles: Vec<(PatData, OptionalFields)> }
    NtcCircleListLayerChange = 0x65821000 { circle: u32, info: PatData, extra: OptionalFields }
    ReqCircleCreate = 0x65010100 { info: PatData, extra: OptionalFields }
    AnsCircleCreate = 0x65010200 { circle: u32 }
    ReqCircleInfo = 0x65020100 { circle: u32, fields: Vec<u8>, fields2: Vec<u8> }
    AnsCircleInfo = 0x65020200 { circle: u32, info: PatData, extra: OptionalFields }
    ReqCircleInfoSet = 0x65200100 { circle: u32, info: PatData, extra: OptionalFields }
    AnsCircleInfoSet = 0x65200200 { circle: u32 }
    NtcCircleInfoSet = 0x65201000 { circle: u32, info: PatData, extra: OptionalFields }
    ReqCircleJoin = 0x65030100 { circle: u32, info: PatData }
    AnsCircleJoin = 0x65030200 { circle: u32, player: u8 }
    NtcCircleJoin = 0x65031000 { circle: u32, capcom_id: Bytes, name: Bytes, player: u8, state: u8 }
    ReqCircleLeave = 0x65040100 { circle: u32 }
    AnsCircleLeave = 0x65040200 { circle: u32 }
    NtcCircleLeave = 0x65041000 { circle: u32, capcom_id: Bytes, player: u8, unk: u8 }
    ReqCircleHost = 0x65410100 { circle: u32 }
    AnsCircleHost = 0x65410200 { circle: u32, leader: u8, capcom_id: Bytes, name: Bytes }
    NtcCircleHost = 0x65411000 { unk: u32, leader: u8, capcom_id: Bytes, name: Bytes }
    ReqCircleUserList = 0x65600100 { fields: Vec<u8> }
    AnsCircleUserList = 0x65600200 { users: Vec<PatData> }
    ReqCircleMatchOptionSet = 0x65100100 { options: PatData }
    AnsCircleMatchOptionSet = 0x65100200 {}
    NtcCircleMatchOptionSet = 0x65101000 { options: PatData }
    ReqCircleMatchStart = 0x65120100 {}
    AnsCircleMatchStart = 0x65120200 {}
    /// The players departing: a count, then per player its index, Capcom ID and a flag.
    NtcCircleMatchStart = 0x65121000 { data: Bytes, rest: Rest }
    ReqCircleMatchEnd = 0x65130100 { unk: u8 }
    AnsCircleMatchEnd = 0x65130200 {}
    /// Game data relayed to the circle (a layout per direction).
    NtcCircleBinary = 0x65701000 { circle: u32, data: Rest }
    NtcCircleBinary2 = 0x65711000 { circle: u32, capcom_id: Bytes, data: Rest }
    // FMP: users.
    ReqUserSearchInfoMine = 0x66370100 { info: PatData }
    AnsUserSearchInfoMine = 0x66370200 { info: PatData }
    /// Part of the hunter's 256-byte profile block.
    ReqUserBinarySet = 0x66310100 { offset: u32, data: Bytes }
    AnsUserBinarySet = 0x66310200 {}
    ReqUserBinaryNotice = 0x66320100 { unk: u8, capcom_id: Bytes, offset: u32, size: u32 }
    AnsUserBinaryNotice = 0x66320200 {}
    NtcUserBinaryNotice = 0x66321000 { unk: u8, capcom_id: Bytes, offset: u32, data: Bytes }
    ReqUserSearchSet = 0x66300100 { extra: OptionalFields }
    AnsUserSearchSet = 0x66300200 {}
    ReqUserStatusSet = 0x66400100 { status: PatData }
    AnsUserStatusSet = 0x66400200 {}
    ReqFriendList = 0x66540100 { first: u32, count: u32, fields: Vec<u8> }
    AnsFriendList = 0x66540200 { unk: u32, friends: Vec<PatData> }
    ReqBlackList = 0x66620100 { first: u32, count: u32, fields: Vec<u8> }
    AnsBlackList = 0x66620200 { unk: u32, users: Vec<PatData> }
    /// The game's own integrity report (anti-cheat, M6).
    NtcCheatCheck = 0x60801000 { data: Rest }

    // FMP: chat and messages.
    /// City chat (`info`: colour, time, sender's Capcom ID and name, filled in by the server when relayed).
    NtcLayerChat = 0x64721000 { unk: u8, info: PatData, text: Bytes }
    /// A message to one player (`capcom_id`: the recipient; the server fills in the sender in `info`).
    ReqTell = 0x66110100 { capcom_id: Bytes, info: PatData, text: Bytes }
    AnsTell = 0x66110200 {}
    NtcTell = 0x66111000 { capcom_id: Bytes, info: PatData, text: Bytes }
    // FMP: friends and black list.
    /// A message to a player carrying a friend request.
    ReqFriendAdd = 0x66500100 { capcom_id: Bytes, info: PatData, text: Bytes }
    AnsFriendAdd = 0x66500200 {}
    /// A friend request, as a message kept in the recipient's inbox.
    NtcFriendAccept = 0x66511000 { capcom_id: Bytes, info: PatData, text: Bytes }
    /// The answer to a friend request (1 accepted).
    ReqFriendAccept = 0x66510100 { capcom_id: Bytes, accepted: u8 }
    AnsFriendAccept = 0x66510200 {}
    /// A new friend, to both players (`friend`: index, Capcom ID, name).
    NtcFriendAdd = 0x66501000 { capcom_id: Bytes, friend: PatData, accepted: u8 }
    ReqFriendDelete = 0x66530100 { capcom_id: Bytes }
    AnsFriendDelete = 0x66530200 {}
    ReqBlackAdd = 0x66600100 { capcom_id: Bytes, info: PatData }
    AnsBlackAdd = 0x66600200 {}
    ReqBlackDelete = 0x66610100 { capcom_id: Bytes }
    AnsBlackDelete = 0x66610200 {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_catalogue() {
        for &(id, name) in TYPED {
            assert_eq!(id.name(), Some(name));
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        assert!(matches!(
            NtcLogin::decode(&[5, 0]),
            Err(DecodeError::Trailing { .. })
        ));
        assert!(matches!(
            AnsShut::decode(&[]),
            Err(DecodeError::Truncated { .. })
        ));
    }
}
