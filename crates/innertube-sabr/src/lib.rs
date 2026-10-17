//! SABR/UMP streaming engine.
//!
//! `application/vnd.yt-ump` responses are a flat sequence of parts:
//! `[varint part_type][varint part_size][part_size bytes]`, repeated to EOF.
//! See `docs/protocol/ump.md` and `docs/protocol/sabr-rounds.md` for the wire facts.

/// Generated protobuf types for the SABR wire messages (request + part
/// payloads), compiled from the vendored googlevideo schemas in `proto/`.
pub mod proto {
    pub mod misc {
        include!(concat!(env!("OUT_DIR"), "/misc.rs"));
    }
    pub mod video_streaming {
        include!(concat!(env!("OUT_DIR"), "/video_streaming.rs"));
    }
}

/// Re-exported so callers can `encode`/`decode` the generated types without
/// taking their own `prost` dependency.
pub use prost;

mod container;
mod engine;
pub use engine::{
    AdBreak, Discontinuity, DiscontinuityKind, Ingest, MediaSegment, RequestPolicy, SabrClientInfo, SabrEngine,
    SabrErrorInfo, SelectedFormat, VodBufferedRange, DEFAULT_LIVE_CHUNK_READAHEAD,
    LIVE_EDGE_SEEK_MS,
};

mod live_lane;
pub use live_lane::{LiveLane, MergeOut};

mod bandwidth;
pub mod bandwidth_persist {
    /// Re-export of the `yt-player-bandwidth` equivalent store (see `bandwidth::persisted`).
    pub use crate::bandwidth::persisted::*;
}
pub use bandwidth::{BandwidthEstimator, CONSERVATIVE_COLD_START_BPS};

mod stream;
pub use stream::{MediaReader, ReaderTracker, TrackBuffer};


pub use bytes::Bytes;

/// A single UMP part. `part_type` is kept as a raw integer (not an enum) so
/// unknown/newer part types round-trip instead of failing the parse.
///
/// `payload` is a [`Bytes`] slice of the response buffer: [`parse`] returns zero-copy
/// views, so a multi-MB MEDIA part is not copied just to be discarded by dedup. Cloning a
/// part is a refcount bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UmpPart {
    pub part_type: u32,
    pub payload: Bytes,
}

/// Anything convertible into the owned [`Bytes`] buffer the UMP parser slices from. An
/// owned `Bytes` (the HTTP response body) converts with a refcount bump; the slice, `Vec`
/// and array impls copy once into a fresh buffer.
///
/// Only `Bytes` is implemented by value; the rest are by reference so `parse(&buf)` at a
/// call site is not a `needless_borrow`.
pub trait AsUmp {
    fn into_ump(self) -> Bytes;
}
impl AsUmp for Bytes {
    fn into_ump(self) -> Bytes {
        self
    }
}
impl AsUmp for &[u8] {
    fn into_ump(self) -> Bytes {
        Bytes::copy_from_slice(self)
    }
}
impl AsUmp for &Vec<u8> {
    fn into_ump(self) -> Bytes {
        Bytes::copy_from_slice(self)
    }
}
impl<const N: usize> AsUmp for &[u8; N] {
    fn into_ump(self) -> Bytes {
        Bytes::copy_from_slice(self)
    }
}

/// UMP part type IDs we handle (see `docs/protocol/ump.md`).
mod part_type {
    pub const SABR_REDIRECT: u32 = 43;
}

/// A server-side control directive the engine must act on, decoded from a
/// recognized UMP part. (Media/header, live-metadata and protection-status parts
/// are handled inline by [`crate::SabrEngine::ingest`] against the proto types;
/// this stays focused on the redirect the engine routes through `from_part`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// Switch subsequent requests to this GVS URL.
    SabrRedirect { url: String },
}

impl Directive {
    /// Decode a directive from a UMP part, or `None` if the part type is not a
    /// directive this engine acts on (including unknown/newer types) or its
    /// payload fails to decode.
    pub fn from_part(part: &UmpPart) -> Option<Directive> {
        use prost::Message;
        use proto::video_streaming as vs;
        match part.part_type {
            part_type::SABR_REDIRECT => Some(Directive::SabrRedirect {
                url: vs::SabrRedirect::decode(&part.payload[..]).ok()?.url?,
            }),
            _ => None,
        }
    }
}

/// Parse a UMP response body into its parts.
///
/// Stops cleanly at the first part whose header or payload is incomplete (a body
/// can end mid-part, since media spans multiple HTTP responses), returning every
/// complete part read so far.
pub fn parse(data: impl AsUmp) -> Vec<UmpPart> {
    let data = data.into_ump();
    let mut parts = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let Some((part_type, o)) = read_varint(&data, off) else { break };
        let Some((size, o)) = read_varint(&data, o) else { break };
        let end = o + size as usize;
        if end > data.len() {
            break; // payload spills past this response; carry over to the next
        }
        // Zero-copy slice of the input buffer.
        parts.push(UmpPart { part_type, payload: data.slice(o..end) });
        off = end;
    }
    parts
}

/// Byte length of the longest prefix of `data` containing only COMPLETE UMP parts.
///
/// [`parse`] stops at the first incomplete part but does not say where it stopped, so a
/// caller feeding arriving chunks cannot tell which trailing bytes it must carry to the
/// next call. This returns exactly that boundary.
pub fn complete_parts_len(data: &[u8]) -> usize {
    let mut off = 0;
    while off < data.len() {
        let Some((_, o)) = read_varint(data, off) else { break };
        let Some((size, o)) = read_varint(data, o) else { break };
        let end = o + size as usize;
        if end > data.len() {
            break;
        }
        off = end;
    }
    off
}

/// Append one UMP part (type, size, payload) to `out`.
pub fn write_part(out: &mut Vec<u8>, part_type: u32, payload: &[u8]) {
    write_varint(out, part_type);
    write_varint(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

/// Append `v` as a UMP varint: the first byte's high bits give the byte length.
pub fn write_varint(out: &mut Vec<u8>, v: u32) {
    match v {
        0..=0x7f => out.push(v as u8),
        0x80..=0x3fff => out.extend([0x80 | (v & 0x3f) as u8, (v >> 6) as u8]),
        0x4000..=0x1f_ffff => out.extend([0xc0 | (v & 0x1f) as u8, (v >> 5) as u8, (v >> 13) as u8]),
        0x20_0000..=0xfff_ffff => {
            out.extend([0xe0 | (v & 0x0f) as u8, (v >> 4) as u8, (v >> 12) as u8, (v >> 20) as u8])
        }
        _ => {
            out.push(0xf0);
            out.extend(v.to_le_bytes());
        }
    }
}

/// Read a UMP varint at `off`, returning the value and the offset past it, or
/// `None` if fewer bytes remain than the first byte's length prefix requires.
/// The byte length is encoded in the high bits of the first byte (see spec note).
fn read_varint(data: &[u8], off: usize) -> Option<(u32, usize)> {
    let b0 = *data.get(off)?;
    let len = match b0 {
        _ if b0 < 0x80 => 1,
        _ if b0 < 0xC0 => 2,
        _ if b0 < 0xE0 => 3,
        _ if b0 < 0xF0 => 4,
        _ => 5,
    };
    if off + len > data.len() {
        return None;
    }
    let b = |i: usize| data[off + i] as u32;
    let v = match len {
        1 => b0 as u32,
        2 => (b0 as u32 & 0x3f) | b(1) << 6,
        3 => (b0 as u32 & 0x1f) | b(1) << 5 | b(2) << 13,
        4 => (b0 as u32 & 0x0f) | b(1) << 4 | b(2) << 12 | b(3) << 20,
        _ => b(1) | b(2) << 8 | b(3) << 16 | b(4) << 24,
    };
    Some((v, off + len))
}


