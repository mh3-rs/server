//! The building blocks of PAT payloads: big-endian integers, length-prefixed strings, typed items,
//! `PatData` records, byte lists and optional fields.
//!
//! Strings are bytes, never UTF-8: names and texts can be Shift-JIS.

use bytes::{Buf, BufMut, Bytes, BytesMut};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("truncated at byte {offset}: {needed} more bytes needed, {available} available")]
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    #[error("unknown item type {tag} at byte {offset}")]
    UnknownItem { offset: usize, tag: u8 },
    #[error("{remaining} unexpected trailing bytes at byte {offset}")]
    Trailing { offset: usize, remaining: usize },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("{len} bytes do not fit a length prefix of at most {max}")]
    TooLong { len: usize, max: usize },
}

/// A typed value, as written in `PatData` records: a type tag then the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Byte(u8),
    Word(u16),
    Long(u32),
    LongLong(u64),
    String(Bytes),
    Binary(Bytes),
}

impl Item {
    pub fn tag(&self) -> u8 {
        match self {
            Item::Byte(_) => 1,
            Item::Word(_) => 2,
            Item::Long(_) => 3,
            Item::LongLong(_) => 4,
            Item::String(_) => 5,
            Item::Binary(_) => 6,
        }
    }
}

/// An ordered list of `(field id, item)`. Order is part of the bytes, so it is kept as given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatData(pub Vec<(u8, Item)>);

impl PatData {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a field (builder style).
    pub fn with(mut self, field: u8, item: Item) -> Self {
        self.0.push((field, item));
        self
    }

    pub fn get(&self, field: u8) -> Option<&Item> {
        self.0
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, item)| item)
    }

    /// Only the given fields, in this record's order. Some answers carry only the fields the
    /// client asked for (e.g. `AnsFmpInfo`).
    pub fn select(&self, fields: &[u8]) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(f, _)| fields.contains(f))
                .cloned()
                .collect(),
        )
    }
}

/// `(field id, value)` pairs where the value may be absent, used by city, circle and search
/// settings: `u8 count`, then per field `u8 id, u8 has_value[, u32 value]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptionalFields(pub Vec<(u8, Option<u32>)>);

/// Reads PAT primitives from a payload, with the offset of any error.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// The bytes not read yet.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// Fails if anything is left: a payload must be read entirely.
    pub fn finish(self) -> Result<(), DecodeError> {
        match self.rest().len() {
            0 => Ok(()),
            remaining => Err(DecodeError::Trailing {
                offset: self.pos,
                remaining,
            }),
        }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let available = self.buf.len() - self.pos;
        if n > available {
            return Err(DecodeError::Truncated {
                offset: self.pos,
                needed: n,
                available,
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.bytes(1)?.get_u8())
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(self.bytes(2)?.get_u16())
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(self.bytes(4)?.get_u32())
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(self.bytes(8)?.get_u64())
    }

    /// `u8` length, then bytes.
    pub fn lp_string(&mut self) -> Result<Bytes, DecodeError> {
        let n = self.u8()?.into();
        Ok(Bytes::copy_from_slice(self.bytes(n)?))
    }

    /// `u16` length, then bytes.
    pub fn lp2_string(&mut self) -> Result<Bytes, DecodeError> {
        let n = self.u16()?.into();
        Ok(Bytes::copy_from_slice(self.bytes(n)?))
    }

    /// `u8` count, then that many bytes (e.g. the field ids a request asks for).
    pub fn byte_list(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u8()?.into();
        Ok(self.bytes(n)?.to_vec())
    }

    pub fn item(&mut self) -> Result<Item, DecodeError> {
        let offset = self.pos;
        Ok(match self.u8()? {
            1 => Item::Byte(self.u8()?),
            2 => Item::Word(self.u16()?),
            3 => Item::Long(self.u32()?),
            4 => Item::LongLong(self.u64()?),
            5 => Item::String(self.lp2_string()?),
            6 => Item::Binary(self.lp2_string()?),
            tag => return Err(DecodeError::UnknownItem { offset, tag }),
        })
    }

    pub fn pat_data(&mut self) -> Result<PatData, DecodeError> {
        let count = self.u8()?;
        let mut fields = Vec::with_capacity(count.into());
        for _ in 0..count {
            let field = self.u8()?;
            fields.push((field, self.item()?));
        }
        Ok(PatData(fields))
    }

    pub fn optional_fields(&mut self) -> Result<OptionalFields, DecodeError> {
        let count = self.u8()?;
        let mut fields = Vec::with_capacity(count.into());
        for _ in 0..count {
            let field = self.u8()?;
            let value = match self.u8()? {
                0 => None,
                _ => Some(self.u32()?),
            };
            fields.push((field, value));
        }
        Ok(OptionalFields(fields))
    }
}

/// Writes PAT primitives. Integers use `bytes::BufMut` (`put_u16`, … are big-endian).
pub trait PatWrite: BufMut {
    fn put_lp_string(&mut self, s: &[u8]) -> Result<(), EncodeError> {
        let n = u8::try_from(s.len()).map_err(|_| too_long(s.len(), u8::MAX))?;
        self.put_u8(n);
        self.put_slice(s);
        Ok(())
    }

    fn put_lp2_string(&mut self, s: &[u8]) -> Result<(), EncodeError> {
        let n = u16::try_from(s.len()).map_err(|_| too_long(s.len(), u16::MAX))?;
        self.put_u16(n);
        self.put_slice(s);
        Ok(())
    }

    fn put_byte_list(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.put_lp_string(bytes)
    }

    fn put_item(&mut self, item: &Item) -> Result<(), EncodeError> {
        self.put_u8(item.tag());
        match item {
            Item::Byte(v) => self.put_u8(*v),
            Item::Word(v) => self.put_u16(*v),
            Item::Long(v) => self.put_u32(*v),
            Item::LongLong(v) => self.put_u64(*v),
            Item::String(s) | Item::Binary(s) => self.put_lp2_string(s)?,
        }
        Ok(())
    }

    fn put_pat_data(&mut self, data: &PatData) -> Result<(), EncodeError> {
        let n = u8::try_from(data.0.len()).map_err(|_| too_long(data.0.len(), u8::MAX))?;
        self.put_u8(n);
        for (field, item) in &data.0 {
            self.put_u8(*field);
            self.put_item(item)?;
        }
        Ok(())
    }

    fn put_optional_fields(&mut self, fields: &OptionalFields) -> Result<(), EncodeError> {
        let n = u8::try_from(fields.0.len()).map_err(|_| too_long(fields.0.len(), u8::MAX))?;
        self.put_u8(n);
        for (field, value) in &fields.0 {
            self.put_u8(*field);
            match value {
                Some(v) => {
                    self.put_u8(1);
                    self.put_u32(*v);
                }
                None => self.put_u8(0),
            }
        }
        Ok(())
    }
}

impl PatWrite for BytesMut {}
impl PatWrite for Vec<u8> {}

fn too_long(len: usize, max: impl Into<usize>) -> EncodeError {
    EncodeError::TooLong {
        len,
        max: max.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn encode(f: impl FnOnce(&mut Vec<u8>) -> Result<(), EncodeError>) -> Vec<u8> {
        let mut out = Vec::new();
        f(&mut out).unwrap();
        out
    }

    #[test]
    fn items_have_the_wire_layout() {
        let cases: [(Item, &[u8]); 6] = [
            (Item::Byte(7), &[1, 7]),
            (Item::Word(0x1234), &[2, 0x12, 0x34]),
            (Item::Long(0x0102_0304), &[3, 1, 2, 3, 4]),
            (Item::LongLong(1), &[4, 0, 0, 0, 0, 0, 0, 0, 1]),
            (
                Item::String(Bytes::from_static(b"ab")),
                &[5, 0, 2, b'a', b'b'],
            ),
            (Item::Binary(Bytes::from_static(&[0xff])), &[6, 0, 1, 0xff]),
        ];
        for (item, bytes) in cases {
            assert_eq!(encode(|w| w.put_item(&item)), bytes, "{item:?}");
            assert_eq!(Reader::new(bytes).item().unwrap(), item);
        }
    }

    #[test]
    fn pat_data_keeps_field_order_and_selects() {
        let data = PatData::new()
            .with(2, Item::Byte(1))
            .with(1, Item::Long(5))
            .with(9, Item::Word(3));
        let bytes = encode(|w| w.put_pat_data(&data));
        assert_eq!(bytes[..4], [3, 2, 1, 1]);
        assert_eq!(Reader::new(&bytes).pat_data().unwrap(), data);
        assert_eq!(data.get(1), Some(&Item::Long(5)));
        assert_eq!(
            data.select(&[9, 2])
                .0
                .iter()
                .map(|f| f.0)
                .collect::<Vec<_>>(),
            [2, 9]
        );
    }

    #[test]
    fn optional_fields_layout() {
        let fields = OptionalFields(vec![(1, Some(0xffff_ffff)), (4, None)]);
        let bytes = encode(|w| w.put_optional_fields(&fields));
        assert_eq!(bytes, [2, 1, 1, 0xff, 0xff, 0xff, 0xff, 4, 0]);
        assert_eq!(Reader::new(&bytes).optional_fields().unwrap(), fields);
    }

    #[test]
    fn errors_say_where() {
        let mut r = Reader::new(&[5, 0, 9, b'x']);
        assert_eq!(
            r.item(),
            Err(DecodeError::Truncated {
                offset: 3,
                needed: 9,
                available: 1
            })
        );
        assert_eq!(
            Reader::new(&[7, 0]).item(),
            Err(DecodeError::UnknownItem { offset: 0, tag: 7 })
        );
        let mut r = Reader::new(&[1, 2, 3]);
        r.u8().unwrap();
        assert_eq!(
            r.finish(),
            Err(DecodeError::Trailing {
                offset: 1,
                remaining: 2
            })
        );
        assert_eq!(
            encode(|_| Ok(())).put_lp_string(&[0; 256]),
            Err(EncodeError::TooLong { len: 256, max: 255 })
        );
    }

    fn item() -> impl Strategy<Value = Item> {
        let bytes = || prop::collection::vec(any::<u8>(), 0..64).prop_map(Bytes::from);
        prop_oneof![
            any::<u8>().prop_map(Item::Byte),
            any::<u16>().prop_map(Item::Word),
            any::<u32>().prop_map(Item::Long),
            any::<u64>().prop_map(Item::LongLong),
            bytes().prop_map(Item::String),
            bytes().prop_map(Item::Binary),
        ]
    }

    proptest! {
        #[test]
        fn pat_data_round_trips(fields in prop::collection::vec((any::<u8>(), item()), 0..20)) {
            let data = PatData(fields);
            let bytes = encode(|w| w.put_pat_data(&data));
            let mut r = Reader::new(&bytes);
            prop_assert_eq!(r.pat_data().unwrap(), data);
            prop_assert!(r.finish().is_ok());
        }

        #[test]
        fn optional_fields_round_trip(fields in prop::collection::vec((any::<u8>(), any::<Option<u32>>()), 0..20)) {
            let fields = OptionalFields(fields);
            let bytes = encode(|w| w.put_optional_fields(&fields));
            prop_assert_eq!(Reader::new(&bytes).optional_fields().unwrap(), fields);
        }

        #[test]
        fn decoding_garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let _ = Reader::new(&bytes).pat_data();
            let _ = Reader::new(&bytes).optional_fields();
        }
    }
}
