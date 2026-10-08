//! The PAT packet layer of *Monster Hunter Tri*'s online protocol.
//!
//! Byte-level only: framing, packet ids, items and records. It knows what the
//! game sends and accepts, nothing about what the server does with it.

mod catalogue;
mod frame;
mod id;
mod pat;

pub use frame::{Frame, FrameCodec, FrameError, HEADER_LEN};
pub use id::{Category, Entry, Kind, PacketId};
pub use pat::{DecodeError, EncodeError, Item, OptionalFields, PatData, PatWrite, Reader};
