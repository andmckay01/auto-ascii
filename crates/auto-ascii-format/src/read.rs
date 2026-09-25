//! ASCI reader over a byte slice — mmap-friendly: map the file (e.g. with
//! `memmap2`) and hand the `&[u8]` here; this crate never opens files.
//!
//! Playback contract: decoding a frame is one zstd block decode plus (for
//! delta frames) one memadd into the caller's standing double buffer — zero
//! allocation per frame after `open`.
//!
//! Validation split: [`AsciiReader::open`] validates the header, the TRLR
//! tail anchor, the pre-frame chunk roll (META/NORM) and the whole FIDX — it
//! never touches frame payloads, so cold open + seek stay fast on multi-GB
//! files. Per-frame structure is (re)validated on every decode;
//! [`AsciiReader::verify`] is the full chunk-walk + CRC32 pass.
//!
//! Seek: binary-search the keyframe roster to the nearest keyframe at or
//! before the target, then ≤ `keyframe_ivl − 1` delta rolls —
//! [`AsciiReader::seek_plane_into`].

use crate::chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_NORM, TAG_TRLR, TRLR_PAYLOAD, align64, frame_flags,
};
use crate::error::{Result, AsciiError};
use crate::header::{HEADER_SIZE, AsciiHeader, codec, filter, header_flags, plane_raw_size};
use crate::meta::Meta;
use crate::norm::{NORM_RECORD_SIZE, PlaneLevels, ShotRecord};

/// Random-access reader over a complete ASCI byte image.
pub struct AsciiReader<'a> {
    bytes: &'a [u8],
    header: AsciiHeader,
    index: Vec<FrameIndexEntry>,
    keyframes: Vec<u32>,
    shots: Vec<ShotRecord>,
    scratch: Vec<u8>,
    dctx: zstd::bulk::Decompressor<'static>,
}

fn walk_chunks(
    bytes: &[u8],
    with_crc: bool,
    mut f: impl FnMut(u64, &ChunkHeader, &[u8], Option<u32>) -> Result<()>,
) -> Result<()> {
    let len = bytes.len();
    let mut pos = HEADER_SIZE as usize;
    while pos < len {
        if len - pos < CHUNK_HEADER_SIZE {
            return Err(AsciiError::Truncated);
        }
        let ch = ChunkHeader::from_bytes(bytes[pos..pos + CHUNK_HEADER_SIZE].try_into().unwrap());
        let size = usize::try_from(ch.size).map_err(|_| AsciiError::Corrupt("chunk size overflow"))?;
        let crc_len = if with_crc { 4 } else { 0 };
        let payload_start = pos + CHUNK_HEADER_SIZE;
        let end = payload_start
            .checked_add(size)
            .and_then(|e| e.checked_add(crc_len))
            .ok_or(AsciiError::Corrupt("chunk size overflow"))?;
        if end > len {
            return Err(AsciiError::Truncated);
        }
        let payload = &bytes[payload_start..payload_start + size];
        let stored_crc = with_crc.then(|| {
            u32::from_le_bytes(bytes[payload_start + size..end].try_into().unwrap())
        });
        f(pos as u64, &ch, payload, stored_crc)?;
        if ch.tag == TAG_TRLR && end != len {
            return Err(AsciiError::Corrupt("data after TRLR"));
        }
        pos = end;
    }
    Ok(())
}

impl<'a> AsciiReader<'a> {
    /// Parse + validate the header, check the TRLR tail anchor (absence ⇒
    /// [`crate::AsciiError::Truncated`]), roll the pre-frame chunks (META /
    /// NORM / unknowns — skipped by size unless required), then load FIDX via
    /// `index_offset` and build the keyframe roster. Frame payloads are never
    /// touched here.
    pub fn open(bytes: &'a [u8]) -> Result<AsciiReader<'a>> {
        if bytes.len() < HEADER_SIZE as usize {
            return Err(AsciiError::Truncated);
        }
        let header = AsciiHeader::from_bytes(bytes[..HEADER_SIZE as usize].try_into().unwrap())?;

        if header.codec != codec::ZSTD {
            return Err(AsciiError::Corrupt("unsupported codec (reader: zstd only)"));
        }
        if header.filter != filter::INTRA && header.filter != filter::TEMPORAL_DELTA {
            return Err(AsciiError::Corrupt("unsupported filter"));
        }
        if header.base_w < 2
            || header.base_h < 2
            || !header.base_w.is_multiple_of(2)
            || !header.base_h.is_multiple_of(2)
        {
            return Err(AsciiError::Corrupt("base dimensions must be even and >= 2"));
        }
        if header.fps_num == 0 || header.fps_den == 0 {
            return Err(AsciiError::Corrupt("fps_num and fps_den must be nonzero"));
        }
        if header.keyframe_ivl == 0 {
            return Err(AsciiError::Corrupt("keyframe_ivl must be >= 1"));
        }
        let plane_count = header.plane_count as usize;
        if plane_count == 0 || plane_count > 8 {
            return Err(AsciiError::Corrupt("plane_count out of range"));
        }
        let registry = &header.plane_ids[..plane_count];
        if registry.contains(&0) {
            return Err(AsciiError::Corrupt("plane id 0 in registry"));
        }
        for (i, id) in registry.iter().enumerate() {
            if registry[..i].contains(id) {
                return Err(AsciiError::Corrupt("duplicate plane id in registry"));
            }
        }
        if header.flags & header_flags::INDEX_PRESENT == 0 {
            return Err(AsciiError::Corrupt("reader requires a frame index (flags bit0)"));
        }

        let with_crc = header.flags & header_flags::CRCS_PRESENT != 0;
        let crc_len = if with_crc { 4usize } else { 0 };

        let tail = CHUNK_HEADER_SIZE + TRLR_PAYLOAD.len() + crc_len;
        let Some(trlr_off) = bytes.len().checked_sub(tail) else {
            return Err(AsciiError::Truncated);
        };
        if trlr_off < HEADER_SIZE as usize {
            return Err(AsciiError::Truncated);
        }
        let tch = ChunkHeader::from_bytes(
            bytes[trlr_off..trlr_off + CHUNK_HEADER_SIZE].try_into().unwrap(),
        );
        if tch.tag != TAG_TRLR
            || tch.size != TRLR_PAYLOAD.len() as u64
            || &bytes[trlr_off + CHUNK_HEADER_SIZE..trlr_off + CHUNK_HEADER_SIZE + TRLR_PAYLOAD.len()]
                != TRLR_PAYLOAD
        {
            return Err(AsciiError::Truncated);
        }

        let mut shots: Vec<ShotRecord> = Vec::new();
        let mut norm_seen = false;
        let mut meta_offset_seen: Option<u64> = None;
        let mut pos = HEADER_SIZE as usize;
        while pos < trlr_off {
            if trlr_off - pos < CHUNK_HEADER_SIZE {
                return Err(AsciiError::Truncated);
            }
            let ch =
                ChunkHeader::from_bytes(bytes[pos..pos + CHUNK_HEADER_SIZE].try_into().unwrap());
            if ch.tag == TAG_FRAM || ch.tag == TAG_FIDX {
                break;
            }
            let size =
                usize::try_from(ch.size).map_err(|_| AsciiError::Corrupt("chunk size overflow"))?;
            let payload_start = pos + CHUNK_HEADER_SIZE;
            let end = payload_start
                .checked_add(size)
                .and_then(|e| e.checked_add(crc_len))
                .ok_or(AsciiError::Corrupt("chunk size overflow"))?;
            if end > trlr_off {
                return Err(AsciiError::Truncated);
            }
            let payload = &bytes[payload_start..payload_start + size];
            match ch.tag {
                TAG_META => {
                    if meta_offset_seen.is_some() {
                        return Err(AsciiError::Corrupt("duplicate META chunk"));
                    }
                    meta_offset_seen = Some(pos as u64);
                }
                TAG_NORM => {
                    if norm_seen {
                        return Err(AsciiError::Corrupt("duplicate NORM chunk"));
                    }
                    if !payload.len().is_multiple_of(NORM_RECORD_SIZE) {
                        return Err(AsciiError::Corrupt("NORM size not a multiple of 24"));
                    }
                    shots = payload
                        .chunks_exact(NORM_RECORD_SIZE)
                        .map(|row| ShotRecord::from_bytes(row.try_into().unwrap()))
                        .collect();
                    norm_seen = true;
                }
                tag => {
                    if ch.is_required() {
                        return Err(AsciiError::UnknownRequiredChunk(tag));
                    }
                }
            }
            pos = end;
        }
        if let Some(m) = meta_offset_seen
            && header.meta_offset != m
        {
            return Err(AsciiError::Corrupt("header meta_offset does not match META chunk"));
        }

        if let Some(first) = shots.first()
            && first.first_frame != 0
        {
            return Err(AsciiError::Corrupt("NORM: first shot must start at frame 0"));
        }
        if shots.windows(2).any(|w| w[1].first_frame <= w[0].first_frame) {
            return Err(AsciiError::Corrupt("NORM: shot first_frame not strictly increasing"));
        }
        if shots.last().is_some_and(|s| s.first_frame >= header.frame_count) {
            return Err(AsciiError::Corrupt("NORM: shot first_frame out of range"));
        }

        let io = usize::try_from(header.index_offset)
            .map_err(|_| AsciiError::Corrupt("index_offset overflow"))?;
        if io < HEADER_SIZE as usize
            || io.checked_add(CHUNK_HEADER_SIZE).is_none_or(|end| end > trlr_off)
        {
            return Err(AsciiError::Corrupt("index_offset out of range"));
        }
        let fch = ChunkHeader::from_bytes(bytes[io..io + CHUNK_HEADER_SIZE].try_into().unwrap());
        if fch.tag != TAG_FIDX {
            return Err(AsciiError::Corrupt("index_offset does not point at a FIDX chunk"));
        }
        let fsize =
            usize::try_from(fch.size).map_err(|_| AsciiError::Corrupt("chunk size overflow"))?;
        if !fsize.is_multiple_of(FIDX_ENTRY_SIZE) {
            return Err(AsciiError::Corrupt("FIDX size not a multiple of 16"));
        }
        let fidx_start = io + CHUNK_HEADER_SIZE;
        let fidx_end = fidx_start
            .checked_add(fsize)
            .ok_or(AsciiError::Corrupt("chunk size overflow"))?;
        if fidx_end > trlr_off {
            return Err(AsciiError::Truncated);
        }
        if fsize / FIDX_ENTRY_SIZE != header.frame_count as usize {
            return Err(AsciiError::Corrupt("FIDX entry count != header frame_count"));
        }
        let mut index = Vec::with_capacity(header.frame_count as usize);
        let mut keyframes: Vec<u32> = Vec::new();
        let mut prev_offset = 0u64;
        for (i, row) in bytes[fidx_start..fidx_end].chunks_exact(FIDX_ENTRY_SIZE).enumerate() {
            let e = FrameIndexEntry::from_bytes(row.try_into().unwrap());
            if e.offset < u64::from(HEADER_SIZE)
                || e.offset
                    .checked_add(CHUNK_HEADER_SIZE as u64)
                    .is_none_or(|end| end > trlr_off as u64)
            {
                return Err(AsciiError::Corrupt("FIDX offset out of range"));
            }
            if i > 0 && e.offset <= prev_offset {
                return Err(AsciiError::Corrupt("FIDX offsets not strictly increasing"));
            }
            prev_offset = e.offset;
            if e.flags & frame_flags::KEYFRAME != 0 {
                keyframes.push(i as u32);
            }
            index.push(e);
        }
        if header.filter == filter::TEMPORAL_DELTA
            && header.frame_count > 0
            && keyframes.first() != Some(&0)
        {
            return Err(AsciiError::Corrupt("first frame of a delta asset must be a keyframe"));
        }

        let scratch = if header.filter == filter::TEMPORAL_DELTA {
            let max_raw = registry
                .iter()
                .filter_map(|&id| plane_raw_size(header.base_w, header.base_h, id))
                .max()
                .unwrap_or(0);
            vec![0u8; max_raw]
        } else {
            Vec::new()
        };

        let dctx = zstd::bulk::Decompressor::new()?;
        Ok(AsciiReader { bytes, header, index, keyframes, shots, scratch, dctx })
    }

    #[inline]
    pub fn header(&self) -> &AsciiHeader {
        &self.header
    }

    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.header.frame_count
    }

    /// Decode the META chunk (CBOR; unknown keys ignored).
    pub fn meta(&self) -> Result<Meta> {
        let offset = usize::try_from(self.header.meta_offset)
            .map_err(|_| AsciiError::Corrupt("meta_offset overflow"))?;
        if offset < HEADER_SIZE as usize
            || offset
                .checked_add(CHUNK_HEADER_SIZE)
                .is_none_or(|end| end > self.bytes.len())
        {
            return Err(AsciiError::Corrupt("meta_offset out of range"));
        }
        let ch = ChunkHeader::from_bytes(
            self.bytes[offset..offset + CHUNK_HEADER_SIZE].try_into().unwrap(),
        );
        if ch.tag != TAG_META {
            return Err(AsciiError::Corrupt("meta_offset does not point at a META chunk"));
        }
        let size = usize::try_from(ch.size).map_err(|_| AsciiError::Corrupt("chunk size overflow"))?;
        let start = offset + CHUNK_HEADER_SIZE;
        let payload = start
            .checked_add(size)
            .and_then(|end| self.bytes.get(start..end))
            .ok_or(AsciiError::Truncated)?;
        ciborium::de::from_reader(payload).map_err(|_| AsciiError::BadMeta)
    }

    /// NORM shot records in `first_frame` order (empty slice when the asset
    /// has no NORM chunk).
    #[inline]
    pub fn shots(&self) -> &[ShotRecord] {
        &self.shots
    }

    /// The shot containing `frame_idx` (binary search over `first_frame`),
    /// or `None` when the asset has no NORM chunk.
    pub fn shot_for_frame(&self, frame_idx: u32) -> Option<&ShotRecord> {
        let p = self.shots.partition_point(|s| s.first_frame <= frame_idx);
        if p == 0 { None } else { Some(&self.shots[p - 1]) }
    }

    /// Position of `plane_id` in the header registry (the index into
    /// [`ShotRecord::levels`]), or `None` if the plane is not in this asset.
    pub fn plane_index(&self, plane_id: u8) -> Option<usize> {
        let n = (self.header.plane_count as usize).min(8);
        self.header.plane_ids[..n].iter().position(|&id| id == plane_id)
    }

    /// Runtime p2/p98 levels for one plane at one frame (the per-shot
    /// auto-levels of the shot containing it). `None` when the asset has no
    /// NORM chunk or the plane is not in this asset.
    pub fn norm_levels(&self, frame_idx: u32, plane_id: u8) -> Option<PlaneLevels> {
        let pi = self.plane_index(plane_id)?;
        self.shot_for_frame(frame_idx).map(|s| s.levels[pi])
    }

    /// Whether `frame_idx` carries the KEYFRAME flag (FIDX bit0).
    pub fn is_keyframe(&self, frame_idx: u32) -> Result<bool> {
        self.index
            .get(frame_idx as usize)
            .map(|e| e.flags & frame_flags::KEYFRAME != 0)
            .ok_or(AsciiError::BadFrameIndex(frame_idx))
    }

    /// Nearest keyframe at or before `frame_idx` (binary search over the
    /// keyframe roster built from FIDX flags). For INTRA assets every frame
    /// stands alone, so this is `frame_idx` itself.
    pub fn nearest_keyframe_at_or_before(&self, frame_idx: u32) -> Result<u32> {
        if frame_idx >= self.header.frame_count {
            return Err(AsciiError::BadFrameIndex(frame_idx));
        }
        if self.header.filter == filter::INTRA {
            return Ok(frame_idx);
        }
        let p = self.keyframes.partition_point(|&k| k <= frame_idx);
        if p == 0 {
            return Err(AsciiError::Corrupt("no keyframe at or before frame"));
        }
        Ok(self.keyframes[p - 1])
    }

    /// Stored dimensions of a plane in this asset: `base_w × base_h` for
    /// Y/E/Ex/Ey/H, half res for C. `None` if the plane is not in the header
    /// registry — or is an unknown (future) ID whose geometry this reader
    /// cannot claim (its raw size travels in the FRAM subblock header
    /// instead).
    pub fn plane_dims(&self, plane_id: u8) -> Option<(u16, u16)> {
        self.plane_index(plane_id)?;
        let (w, h) = (self.header.base_w, self.header.base_h);
        if plane_id == crate::header::plane_id::C {
            Some((w / 2, h / 2))
        } else if crate::header::plane_id::is_known(plane_id) {
            Some((w, h))
        } else {
            None
        }
    }

    /// Decode one plane of one frame into `dst` (len ≥ the plane's raw size;
    /// `&mut self` for the reused zstd context). Keyframes (and every frame
    /// of INTRA assets) decode standalone: one zstd block into `dst`. Delta
    /// frames REQUIRE `dst` to already hold the fully decoded previous frame
    /// of the same plane (the standing double buffer): the delta is decoded
    /// to scratch and memadded in place. For random access use
    /// [`seek_plane_into`](AsciiReader::seek_plane_into). Only the requested
    /// plane's subblock is decompressed. Returns the raw byte count.
    pub fn decode_plane_into(&mut self, frame_idx: u32, plane_id: u8, dst: &mut [u8]) -> Result<usize> {
        let n = (self.header.plane_count as usize).min(8);
        if !self.header.plane_ids[..n].contains(&plane_id) {
            return Err(AsciiError::BadPlaneId(plane_id));
        }
        let entry = *self
            .index
            .get(frame_idx as usize)
            .ok_or(AsciiError::BadFrameIndex(frame_idx))?;

        let offset = usize::try_from(entry.offset)
            .map_err(|_| AsciiError::Corrupt("FRAM offset overflow"))?;
        if offset + CHUNK_HEADER_SIZE > self.bytes.len() {
            return Err(AsciiError::Corrupt("FRAM offset out of range"));
        }
        let ch = ChunkHeader::from_bytes(
            self.bytes[offset..offset + CHUNK_HEADER_SIZE].try_into().unwrap(),
        );
        if ch.tag != TAG_FRAM {
            return Err(AsciiError::Corrupt("FIDX entry does not point at a FRAM chunk"));
        }
        let size = usize::try_from(ch.size).map_err(|_| AsciiError::Corrupt("chunk size overflow"))?;
        let start = offset + CHUNK_HEADER_SIZE;
        let payload = start
            .checked_add(size)
            .and_then(|end| self.bytes.get(start..end))
            .ok_or(AsciiError::Truncated)?;

        if payload.len() < 5 {
            return Err(AsciiError::Corrupt("FRAM payload too short"));
        }
        let stored_idx = u32::from_le_bytes(payload[..4].try_into().unwrap());
        if stored_idx != frame_idx {
            return Err(AsciiError::Corrupt("FRAM frame_idx does not match FIDX position"));
        }
        if payload[4] != entry.flags {
            return Err(AsciiError::Corrupt("FRAM flags do not match FIDX entry"));
        }
        let is_delta = self.header.filter == filter::TEMPORAL_DELTA
            && entry.flags & frame_flags::KEYFRAME == 0;

        let mut p = 5usize;
        for _ in 0..n {
            if payload.len() - p < 9 {
                return Err(AsciiError::Corrupt("truncated plane subblock"));
            }
            let id = payload[p];
            let comp_size = u32::from_le_bytes(payload[p + 1..p + 5].try_into().unwrap()) as usize;
            let raw_size = u32::from_le_bytes(payload[p + 5..p + 9].try_into().unwrap()) as usize;
            let data_start = p + 9;
            let data_end = data_start
                .checked_add(comp_size)
                .ok_or(AsciiError::Corrupt("subblock size overflow"))?;
            if data_end > payload.len() {
                return Err(AsciiError::Corrupt("plane subblock overruns FRAM payload"));
            }
            if id == plane_id {
                if dst.len() < raw_size {
                    return Err(AsciiError::Corrupt("dst buffer smaller than plane raw size"));
                }
                let src = &payload[data_start..data_end];
                if is_delta {
                    if self.scratch.len() < raw_size {
                        self.scratch.resize(raw_size, 0);
                    }
                    let written = self
                        .dctx
                        .decompress_to_buffer(src, &mut self.scratch[..raw_size])?;
                    if written != raw_size {
                        return Err(AsciiError::Corrupt("decoded size != raw_size"));
                    }
                    for (d, &s) in dst[..raw_size].iter_mut().zip(&self.scratch[..raw_size]) {
                        *d = d.wrapping_add(s);
                    }
                } else {
                    let written =
                        self.dctx.decompress_to_buffer(src, &mut dst[..raw_size])?;
                    if written != raw_size {
                        return Err(AsciiError::Corrupt("decoded size != raw_size"));
                    }
                }
                return Ok(raw_size);
            }
            let sub = align64(9 + comp_size as u64) as usize;
            p = p
                .checked_add(sub)
                .filter(|&q| q <= payload.len())
                .ok_or(AsciiError::Corrupt("subblock padding overruns FRAM payload"))?;
        }
        Err(AsciiError::BadPlaneId(plane_id))
    }

    /// Random-access decode: binary-search to the nearest keyframe at or
    /// before `frame_idx`, decode it intra into `dst`, then roll
    /// ≤ `keyframe_ivl − 1` deltas forward. `dst` contents on entry are
    /// irrelevant. Returns the raw byte count.
    pub fn seek_plane_into(&mut self, frame_idx: u32, plane_id: u8, dst: &mut [u8]) -> Result<usize> {
        let key = self.nearest_keyframe_at_or_before(frame_idx)?;
        let mut written = self.decode_plane_into(key, plane_id, dst)?;
        for f in key + 1..=frame_idx {
            written = self.decode_plane_into(f, plane_id, dst)?;
        }
        Ok(written)
    }

    /// Full-file integrity walk: walk all chunks, verify framing, every CRC32
    /// and the TRLR.
    pub fn verify(&self) -> Result<()> {
        let with_crc = self.header.flags & header_flags::CRCS_PRESENT != 0;
        let mut trlr_ok = false;
        walk_chunks(self.bytes, with_crc, |_, ch, payload, stored_crc| {
            if let Some(stored) = stored_crc
                && crc32fast::hash(payload) != stored
            {
                return Err(AsciiError::CrcMismatch { tag: ch.tag });
            }
            if ch.tag == TAG_TRLR {
                trlr_ok = payload == TRLR_PAYLOAD;
            }
            Ok(())
        })?;
        if !trlr_ok {
            return Err(AsciiError::Truncated);
        }
        Ok(())
    }
}
