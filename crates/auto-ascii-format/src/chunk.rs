//! ASCI chunk framing, frame-index entries and frame flags.

pub const TAG_META: [u8; 4] = *b"META";
pub const TAG_NORM: [u8; 4] = *b"NORM";
pub const TAG_FRAM: [u8; 4] = *b"FRAM";
pub const TAG_FIDX: [u8; 4] = *b"FIDX";
pub const TAG_TRLR: [u8; 4] = *b"TRLR";

pub const TRLR_PAYLOAD: &[u8] = b"ASCI_END";

pub mod chunk_flags {
    pub const REQUIRED: u8 = 1;
}

pub mod frame_flags {
    pub const KEYFRAME: u8 = 1;
}

pub const CHUNK_HEADER_SIZE: usize = 16;

#[inline]
pub(crate) fn align64(n: u64) -> u64 {
    (n + 63) & !63
}

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

pub const FIDX_ENTRY_SIZE: usize = 16;

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
        assert_eq!(&b[5..8], &[0, 0, 0]);
        assert_eq!(ChunkHeader::from_bytes(&b), h);
        assert!(h.is_required());
    }

    #[test]
    fn fidx_entry_roundtrip_and_pad() {
        let e = FrameIndexEntry { offset: 1 << 40, comp_size: 999, flags: frame_flags::KEYFRAME };
        let b = e.to_bytes();
        assert_eq!(&b[13..16], &[0, 0, 0]);
        assert_eq!(FrameIndexEntry::from_bytes(&b), e);
    }
}
