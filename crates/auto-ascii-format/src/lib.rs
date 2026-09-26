//! ASCI container headers, chunks, feature-plane encoding and decoding.

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
