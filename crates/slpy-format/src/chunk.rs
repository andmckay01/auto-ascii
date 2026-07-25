//! Chunk framing, FIDX entries and FRAM flags (PLAN §4).
//!
//! Chunk wire format: `tag FourCC u32 | flags u8 (bit0=required) | pad u24 |
//! size u64 | payload | [crc32 u32 when header CRCS_PRESENT]`. The CRC32
//! (IEEE, via `crc32fast`) covers the payload bytes only.

pub const TAG_META: [u8; 4] = *b"META";
pub const TAG_NORM: [u8; 4] = *b"NORM";
pub const TAG_FRAM: [u8; 4] = *b"FRAM";
pub const TAG_FIDX: [u8; 4] = *b"FIDX";
pub const TAG_TRLR: [u8; 4] = *b"TRLR";

/// TRLR payload (PLAN §4): absence ⇒ truncated ⇒ factory rerun.
pub const TRLR_PAYLOAD: &[u8] = b"SLPY_END";

/// Chunk flag bits (PLAN §4): unknown non-required chunks are skipped by
/// size; unknown *required* chunks are a hard error.
pub mod chunk_flags {
    pub const REQUIRED: u8 = 1;
}

/// FRAM per-frame flag bits (PLAN §4). M0 (`filter = intra`) sets KEYFRAME on
/// every frame.
pub mod frame_flags {
    pub const KEYFRAME: u8 = 1;
}

/// Encoded size of a chunk header on the wire.
pub const CHUNK_HEADER_SIZE: usize = 16;

/// Round `n` up to the next multiple of 64 (PLAN §4: plane subblocks inside
/// FRAM payloads are padded to 64-B alignment).
#[inline]
pub(crate) fn align64(n: u64) -> u64 {
    (n + 63) & !63
}

/// Parsed chunk header (PLAN §4). `size` counts payload bytes only — the
/// trailing CRC32 (if the file has CRCs) is not included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkHeader {
    pub tag: [u8; 4],
    pub flags: u8,
    pub size: u64,
}

impl ChunkHeader {
    pub fn to_bytes(&self) -> [u8; CHUNK_HEADER_SIZE] {
        let mut b = [0u8; CHUNK_HEADER_SIZE];
        b[0..4].copy_from_slice(&self.tag);
        b[4] = self.flags;
        // 5..8 pad u24 = 0
        b[8..16].copy_from_slice(&self.size.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; CHUNK_HEADER_SIZE]) -> ChunkHeader {
        ChunkHeader {
            tag: [b[0], b[1], b[2], b[3]],
            flags: b[4],
            size: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        }
    }

    #[inline]
    pub fn is_required(&self) -> bool {
        self.flags & chunk_flags::REQUIRED != 0
    }
}

/// Encoded size of one FIDX entry (PLAN §4: frame_count × 16 B).
pub const FIDX_ENTRY_SIZE: usize = 16;

/// One FIDX row (PLAN §4): `{offset u64, comp_size u32, flags u8, pad u24}`.
/// `offset` is the absolute file offset of the frame's FRAM chunk header;
/// `flags` mirrors the FRAM flags (bit0 = keyframe) so seek can binary-search
/// backward to a keyframe without touching FRAM data (PLAN §4 compression).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameIndexEntry {
    pub offset: u64,
    pub comp_size: u32,
    pub flags: u8,
}

impl FrameIndexEntry {
    pub fn to_bytes(&self) -> [u8; FIDX_ENTRY_SIZE] {
        let mut b = [0u8; FIDX_ENTRY_SIZE];
        b[0..8].copy_from_slice(&self.offset.to_le_bytes());
        b[8..12].copy_from_slice(&self.comp_size.to_le_bytes());
        b[12] = self.flags;
        // 13..16 pad u24 = 0
        b
    }

    pub fn from_bytes(b: &[u8; FIDX_ENTRY_SIZE]) -> FrameIndexEntry {
        FrameIndexEntry {
            offset: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            comp_size: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            flags: b[12],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_header_roundtrip_and_pad() {
        let h = ChunkHeader { tag: TAG_FRAM, flags: chunk_flags::REQUIRED, size: 12345 };
        let b = h.to_bytes();
        assert_eq!(&b[5..8], &[0, 0, 0]); // pad u24
        assert_eq!(ChunkHeader::from_bytes(&b), h);
        assert!(h.is_required());
    }

    #[test]
    fn fidx_entry_roundtrip_and_pad() {
        let e = FrameIndexEntry { offset: 1 << 40, comp_size: 999, flags: frame_flags::KEYFRAME };
        let b = e.to_bytes();
        assert_eq!(&b[13..16], &[0, 0, 0]); // pad u24
        assert_eq!(FrameIndexEntry::from_bytes(&b), e);
    }
}
