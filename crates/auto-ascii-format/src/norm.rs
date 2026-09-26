//! ASCI NORM shot-level records and cut flags.

pub mod norm_flags {
    pub const CUT: u8 = 1;
}

pub const NORM_RECORD_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaneLevels {
    pub p2: u8,
    pub p98: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShotRecord {
    pub first_frame: u32,
    pub flags: u8,
    pub levels: [PlaneLevels; 8],
}

impl ShotRecord {
    pub fn to_bytes(&self) -> [u8; NORM_RECORD_SIZE] {
        let mut b = [0u8; NORM_RECORD_SIZE];
        b[0..4].copy_from_slice(&self.first_frame.to_le_bytes());
        b[4] = self.flags;
        for (i, l) in self.levels.iter().enumerate() {
            b[8 + 2 * i] = l.p2;
            b[9 + 2 * i] = l.p98;
        }
        b
    }

    pub fn from_bytes(b: &[u8; NORM_RECORD_SIZE]) -> ShotRecord {
        let mut levels = [PlaneLevels::default(); 8];
        for (i, l) in levels.iter_mut().enumerate() {
            l.p2 = b[8 + 2 * i];
            l.p98 = b[9 + 2 * i];
        }
        ShotRecord {
            first_frame: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            flags: b[4],
            levels,
        }
    }

    #[inline]
    pub fn is_cut(&self) -> bool {
        self.flags & norm_flags::CUT != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_pad() {
        let mut levels = [PlaneLevels::default(); 8];
        levels[0] = PlaneLevels { p2: 12, p98: 240 };
        levels[1] = PlaneLevels { p2: 3, p98: 200 };
        let r = ShotRecord { first_frame: 0x0102_0304, flags: norm_flags::CUT, levels };
        let b = r.to_bytes();
        assert_eq!(&b[5..8], &[0, 0, 0]);
        assert_eq!(b[8], 12);
        assert_eq!(b[9], 240);
        assert_eq!(ShotRecord::from_bytes(&b), r);
        assert!(r.is_cut());
    }
}
