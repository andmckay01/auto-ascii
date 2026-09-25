//! `auto-ascii-format` — the ASCI v1 chunked container: 64-byte fixed header,
//! RIFF-style chunks (META / NORM / FRAM / FIDX / TRLR), per-frame
//! zstd-compressed feature planes. Container only — no I/O policy: the writer
//! takes `Write + Seek`, the reader takes `&[u8]` (mmap-friendly).
//!
//! Determinism is a contract: hand-rolled fixed layout everywhere, CBOR only
//! inside META, per-chunk CRC32, byte-identical writer output for identical
//! input — enforced by byte-golden tests.
//!
//! Format features: temporal byte-delta filter with keyframes every
//! `keyframe_ivl` frames (default 60, FRAM/FIDX flags bit0), 64-B-aligned
//! plane subblocks, FIDX seek (keyframe binary search + delta rolls,
//! [`AsciiReader::seek_plane_into`]), NORM per-shot runtime levels + cut flags
//! ([`ShotRecord`]), chroma plane C (RGB565, half res). Minor versions are
//! additive: minor-0 (intra-only) files remain readable.

pub mod chunk;
pub mod error;
pub mod header;
pub mod meta;
pub mod norm;
pub mod read;
pub mod write;

pub use chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_NORM, TAG_TRLR, TRLR_PAYLOAD, chunk_flags, frame_flags,
};
pub use error::{Result, AsciiError};
pub use header::{
    BASE_H, BASE_W, HEADER_SIZE, MAGIC, AsciiHeader, VERSION_MAJOR, VERSION_MINOR, codec, filter,
    header_flags, plane_id, plane_raw_size,
};
pub use meta::Meta;
pub use norm::{NORM_RECORD_SIZE, PlaneLevels, ShotRecord, norm_flags};
pub use read::AsciiReader;
pub use write::{PlaneRef, AsciiWriter, WriterOptions};
