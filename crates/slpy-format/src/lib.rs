//! `slpy-format` — the SLPY v1 chunked container (PLAN §4): 64-byte fixed
//! header, RIFF-style chunks (META / NORM / FRAM / FIDX / TRLR), per-frame
//! zstd-compressed feature planes. Container only — no I/O policy (PLAN §2):
//! the writer takes `Write + Seek`, the reader takes `&[u8]` (mmap-friendly).
//!
//! Determinism is a contract (PLAN §4): hand-rolled fixed layout everywhere,
//! CBOR only inside META, per-chunk CRC32, byte-identical writer output for
//! identical input — enforced by the M0 byte-golden tests.
//!
//! M0 subset (PLAN §7 + approved simplification): luma-only "SLPY-lite" —
//! FRAM carries the Y plane only, `codec = zstd`, `filter = intra` (every
//! frame is a keyframe); global p2/p98 level normalization is baked into the
//! plane at encode time. NORM, temporal delta, and the remaining planes land
//! at M1/M3 — the header layout below is the full §4 layout so M1 does not
//! break the format.

pub mod chunk;
pub mod error;
pub mod header;
pub mod meta;
pub mod read;
pub mod write;

pub use chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_NORM, TAG_TRLR, TRLR_PAYLOAD, chunk_flags, frame_flags,
};
pub use error::{Result, SlpyError};
pub use header::{
    BASE_H, BASE_W, HEADER_SIZE, MAGIC, SlpyHeader, VERSION_MAJOR, VERSION_MINOR, codec, filter,
    header_flags, plane_id,
};
pub use meta::Meta;
pub use read::SlpyReader;
pub use write::{PlaneRef, SlpyWriter, WriterOptions};
