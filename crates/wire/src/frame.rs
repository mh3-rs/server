use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::PacketId;

/// Header size: `size: u16`, `seq: u16`, `id: u32`, big-endian.
pub const HEADER_LEN: usize = 8;

/// One PAT packet as it travels on a TCP stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Sequence number chosen by the sender; answers echo the request's.
    pub seq: u16,
    pub id: PacketId,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("payload of {0} bytes does not fit the u16 size field")]
    PayloadTooLarge(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Splits a byte stream into [`Frame`]s and back.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = FrameError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, FrameError> {
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
        let size = u16::from_be_bytes([src[0], src[1]]) as usize;
        if src.len() < HEADER_LEN + size {
            src.reserve(HEADER_LEN + size - src.len());
            return Ok(None);
        }
        let _size = src.get_u16();
        let seq = src.get_u16();
        let id = PacketId(src.get_u32());
        let payload = src.split_to(size).freeze();
        Ok(Some(Frame { seq, id, payload }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = FrameError;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<(), FrameError> {
        let size = u16::try_from(frame.payload.len())
            .map_err(|_| FrameError::PayloadTooLarge(frame.payload.len()))?;
        dst.reserve(HEADER_LEN + frame.payload.len());
        dst.put_u16(size);
        dst.put_u16(frame.seq);
        dst.put_u32(frame.id.0);
        dst.put_slice(&frame.payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first frame of a real session: OPN's `ReqMemoryCheck` asking for the game id at 0x80000000.
    const REQ_MEMORY_CHECK: &[u8] = &[
        0x00, 0x0c, 0x00, 0x00, 0x60, 0x81, 0x01, 0x00, // size 12, seq 0, ReqMemoryCheck
        0x01, 0x01, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06,
    ];

    fn decode_all(bytes: &[u8]) -> Vec<Frame> {
        let mut buf = BytesMut::from(bytes);
        let mut frames = Vec::new();
        while let Some(f) = FrameCodec.decode(&mut buf).unwrap() {
            frames.push(f);
        }
        assert!(buf.is_empty(), "{} trailing bytes", buf.len());
        frames
    }

    #[test]
    fn decodes_a_real_frame() {
        let frames = decode_all(REQ_MEMORY_CHECK);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 0);
        assert_eq!(frames[0].id.name(), Some("ReqMemoryCheck"));
        assert_eq!(frames[0].payload.len(), 12);
    }

    #[test]
    fn waits_for_partial_header_and_payload() {
        for cut in 0..REQ_MEMORY_CHECK.len() {
            let mut buf = BytesMut::from(&REQ_MEMORY_CHECK[..cut]);
            assert!(
                FrameCodec.decode(&mut buf).unwrap().is_none(),
                "cut at {cut}"
            );
            buf.extend_from_slice(&REQ_MEMORY_CHECK[cut..]);
            assert!(
                FrameCodec.decode(&mut buf).unwrap().is_some(),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn splits_back_to_back_frames() {
        let empty = [0x00, 0x00, 0x12, 0x34, 0x60, 0x01, 0x01, 0x00]; // ReqLineCheck, no payload
        let stream = [REQ_MEMORY_CHECK, &empty, REQ_MEMORY_CHECK].concat();
        let frames = decode_all(&stream);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].seq, 0x1234);
        assert!(frames[1].payload.is_empty());
    }

    #[test]
    fn encode_is_the_inverse_of_decode() {
        let frame = decode_all(REQ_MEMORY_CHECK).remove(0);
        let mut out = BytesMut::new();
        FrameCodec.encode(frame, &mut out).unwrap();
        assert_eq!(&out[..], REQ_MEMORY_CHECK);
    }

    #[test]
    fn rejects_oversize_payload() {
        let frame = Frame {
            seq: 0,
            id: PacketId(0x6001_0100),
            payload: Bytes::from(vec![0; 0x1_0000]),
        };
        let err = FrameCodec.encode(frame, &mut BytesMut::new()).unwrap_err();
        assert!(matches!(err, FrameError::PayloadTooLarge(0x1_0000)));
    }
}
