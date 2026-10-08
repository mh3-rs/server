//! Typed packets: one struct per message, decoded from and encoded to its payload byte for byte.
//!
//! Field types map to the wire as: `u8`/`u16`/`u32`/`u64` big-endian, `Bytes` a lp2 string, `PatData` a record,
//! `Vec<u8>` a byte list (u8 count, then the bytes), `Vec<PatData>` a record list (u32 count, then the records).
//! Fields whose meaning is unknown are kept as `unk*` so re-encoding gives back the same bytes.
//! Layouts come from MH3SP and are checked against every recorded payload (`testkit/tests/corpus_roundtrip.rs`);
//! the few never recorded say so.

use bytes::Bytes;

use crate::{DecodeError, EncodeError, PacketId, PatData, PatWrite, Reader};

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
        let n = u32::try_from(self.len()).map_err(|_| EncodeError::TooLong {
            len: self.len(),
            max: u32::MAX as usize,
        })?;
        out.put_u32(n);
        self.iter().try_for_each(|d| out.put_pat_data(d))
    }
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
