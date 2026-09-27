//! Fixed ASCI header representation and wire serialization.

use crate::error::{Result, AsciiError};

pub const MAGIC: [u8; 4] = *b"ASCI";
pub const HEADER_SIZE: u32 = 64;
pub const VERSION_MAJOR: u16 = 1;
pub const VERSION_MINOR: u16 = 1;
pub const BASE_W: u16 = 480;
pub const BASE_H: u16 = 270;

pub mod plane_id {
    pub const Y: u8 = 1;
    pub const E: u8 = 2;
    pub const EX: u8 = 3;
    pub const EY: u8 = 4;
    pub const H: u8 = 5;
    pub const C: u8 = 6;

    pub const fn is_known(id: u8) -> bool {
        matches!(id, Y..=C)
    }
}

pub fn plane_raw_size(base_w: u16, base_h: u16, id: u8) -> Option<usize> {
    let (w, h) = (base_w as usize, base_h as usize);
    match id {
        plane_id::C => Some((w / 2) * (h / 2) * 2),
        id if plane_id::is_known(id) => Some(w * h),
        _ => None,
    }
}

pub mod codec {
    pub const RAW: u8 = 0;
    pub const LZ4: u8 = 1;
    pub const ZSTD: u8 = 2;
}

pub mod filter {
    pub const INTRA: u8 = 0;
    pub const TEMPORAL_DELTA: u8 = 1;
}

pub mod header_flags {
    pub const INDEX_PRESENT: u32 = 1;
    pub const CRCS_PRESENT: u32 = 1 << 1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsciiHeader {
    pub version_major: u16,
    pub version_minor: u16,
    pub flags: u32,
    pub fps_num: u16,
    pub fps_den: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    pub frame_count: u32,
    pub plane_count: u8,
    pub codec: u8,
    pub filter: u8,
    pub keyframe_ivl: u8,
    pub plane_ids: [u8; 8],
    pub index_offset: u64,
    pub meta_offset: u64,
}

impl AsciiHeader {
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&self.version_major.to_le_bytes());
        b[6..8].copy_from_slice(&self.version_minor.to_le_bytes());
        b[8..12].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..18].copy_from_slice(&self.fps_num.to_le_bytes());
        b[18..20].copy_from_slice(&self.fps_den.to_le_bytes());
        b[20..22].copy_from_slice(&self.base_w.to_le_bytes());
        b[22..24].copy_from_slice(&self.base_h.to_le_bytes());
        b[24..26].copy_from_slice(&self.aspect_num.to_le_bytes());
        b[26..28].copy_from_slice(&self.aspect_den.to_le_bytes());
        b[28..32].copy_from_slice(&self.frame_count.to_le_bytes());
        b[32] = self.plane_count;
        b[33] = self.codec;
        b[34] = self.filter;
        b[35] = self.keyframe_ivl;
        b[36..44].copy_from_slice(&self.plane_ids);
        b[44..52].copy_from_slice(&self.index_offset.to_le_bytes());
        b[52..60].copy_from_slice(&self.meta_offset.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; 64]) -> Result<AsciiHeader> {
        let le16 = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let le32 = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let le64 = |o: usize| {
            u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
        };

        if b[0..4] != MAGIC {
            return Err(AsciiError::BadMagic);
        }
        let version_major = le16(4);
        if version_major > VERSION_MAJOR {
            return Err(AsciiError::UnsupportedVersion { found: version_major, supported: VERSION_MAJOR });
        }
        if le32(8) != HEADER_SIZE {
            return Err(AsciiError::Corrupt("header_size != 64"));
        }
        let mut plane_ids = [0u8; 8];
        plane_ids.copy_from_slice(&b[36..44]);
        Ok(AsciiHeader {
            version_major,
            version_minor: le16(6),
            flags: le32(12),
            fps_num: le16(16),
            fps_den: le16(18),
            base_w: le16(20),
            base_h: le16(22),
            aspect_num: le16(24),
            aspect_den: le16(26),
            frame_count: le32(28),
            plane_count: b[32],
            codec: b[33],
            filter: b[34],
            keyframe_ivl: b[35],
            plane_ids,
            index_offset: le64(44),
            meta_offset: le64(52),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AsciiHeader {
        AsciiHeader {
            version_major: 1,
            version_minor: 0,
            flags: header_flags::INDEX_PRESENT | header_flags::CRCS_PRESENT,
            fps_num: 30,
            fps_den: 1,
            base_w: BASE_W,
            base_h: BASE_H,
            aspect_num: 16,
            aspect_den: 9,
            frame_count: 900,
            plane_count: 1,
            codec: codec::ZSTD,
            filter: filter::INTRA,
            keyframe_ivl: 60,
            plane_ids: [plane_id::Y, 0, 0, 0, 0, 0, 0, 0],
            index_offset: 0xDEAD_BEEF,
            meta_offset: 64,
        }
    }

    #[test]
    fn layout_offsets_frozen() {
        let b = sample().to_bytes();
        assert_eq!(&b[0..4], b"ASCI");
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 64);
        assert_eq!(u16::from_le_bytes(b[16..18].try_into().unwrap()), 30);
        assert_eq!(u16::from_le_bytes(b[20..22].try_into().unwrap()), 480);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 900);
        assert_eq!(b[32], 1);
        assert_eq!(b[33], codec::ZSTD);
        assert_eq!(b[36], plane_id::Y);
        assert_eq!(u64::from_le_bytes(b[44..52].try_into().unwrap()), 0xDEAD_BEEF);
        assert_eq!(&b[60..64], &[0, 0, 0, 0]);
    }

    #[test]
    fn roundtrip() {
        let h = sample();
        assert_eq!(AsciiHeader::from_bytes(&h.to_bytes()).unwrap(), h);
    }

    #[test]
    fn plane_raw_sizes() {
        assert_eq!(plane_raw_size(480, 270, plane_id::Y), Some(480 * 270));
        assert_eq!(plane_raw_size(480, 270, plane_id::H), Some(480 * 270));
        assert_eq!(plane_raw_size(480, 270, plane_id::C), Some(240 * 135 * 2));
        assert_eq!(plane_raw_size(480, 270, 0), None);
        assert_eq!(plane_raw_size(480, 270, 7), None);
        assert!(plane_id::is_known(plane_id::Y) && plane_id::is_known(plane_id::C));
        assert!(!plane_id::is_known(0) && !plane_id::is_known(7));
    }

    #[test]
    fn rejects_bad_magic_and_future_major() {
        let mut b = sample().to_bytes();
        b[0] = b'X';
        assert!(matches!(AsciiHeader::from_bytes(&b), Err(AsciiError::BadMagic)));

        let mut b = sample().to_bytes();
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            AsciiHeader::from_bytes(&b),
            Err(AsciiError::UnsupportedVersion { found: 2, .. })
        ));
    }
}
