//! Typed packets: one struct per message, decoded from and encoded to its payload byte for byte.
//!
//! Field types map to the wire as: `u8`/`u16`/`u32`/`u64` big-endian, `Bytes` a lp2 string, `PatData` a record.
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
    )*};
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_the_catalogue() {
        macro_rules! check {
            ($($p:ident),*) => {$(
                assert_eq!(<$p as Packet>::ID.name(), Some(stringify!($p)));
            )*};
        }
        check!(
            ReqLineCheck,
            AnsLineCheck,
            ReqShut,
            AnsShut,
            ReqConnection,
            AnsConnection,
            NtcLogin,
            ReqMemoryCheck,
            AnsMemoryCheck,
            ReqCommonKey,
            ReqLmpConnect,
            AnsLmpConnect,
            ReqTermsVersion,
            AnsTermsVersion,
            ReqTerms,
            AnsTerms,
            ReqSubTermsInfo,
            AnsSubTermsInfo,
            ReqSubTerms,
            AnsSubTerms,
            ReqMaintenance,
            AnsMaintenance,
            ReqAnnounce,
            AnsAnnounce,
            ReqNoCharge,
            AnsNoCharge,
            ReqVulgarityInfoLow,
            AnsVulgarityInfoLow,
            ReqAuthenticationToken,
            AnsAuthenticationToken
        );
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
