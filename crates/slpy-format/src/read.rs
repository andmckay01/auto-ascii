//! SLPY reader over a byte slice (PLAN §4) — mmap-friendly: the player maps
//! the file with `memmap2` and hands `&[u8]` here; this crate never opens
//! files (no I/O policy, PLAN §2).
//!
//! Playback contract (PLAN §3.6 step 3 / §4): decoding a frame is one zstd
//! block decode (+ one memadd once temporal delta lands at M1) into caller
//! buffers — zero allocation per frame after `open`.
//!
//! Validation split: [`SlpyReader::open`] does the cheap structural walk
//! (header, chunk framing, FIDX↔FRAM cross-check, TRLR presence) without
//! touching payload bytes; [`SlpyReader::verify`] is the full CRC32 walk for
//! `sleepy-factory inspect`.

use crate::chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_NORM, TAG_TRLR, TRLR_PAYLOAD, align64,
};
use crate::error::{Result, SlpyError};
use crate::header::{HEADER_SIZE, SlpyHeader, codec, filter, header_flags};
use crate::meta::Meta;

/// Random-access reader over a complete SLPY byte image.
pub struct SlpyReader<'a> {
    bytes: &'a [u8],
    header: SlpyHeader,
    /// Decoded FIDX (16 B/frame — kept owned; ~14 KB for a 30 s asset).
    index: Vec<FrameIndexEntry>,
    /// Reusable zstd decode context (zero alloc per frame).
    dctx: zstd::bulk::Decompressor<'static>,
}

/// Walk all chunks from the end of the header to EOF. Calls `f(offset,
/// header, payload, stored_crc)` per chunk; `stored_crc` is `Some` when the
/// file carries CRCs. Enforces framing only (bounds, TRLR-is-last) — tag
/// semantics and CRC checking are the callers' business.
fn walk_chunks(
    bytes: &[u8],
    with_crc: bool,
    mut f: impl FnMut(u64, &ChunkHeader, &[u8], Option<u32>) -> Result<()>,
) -> Result<()> {
    let len = bytes.len();
    let mut pos = HEADER_SIZE as usize;
    while pos < len {
        if len - pos < CHUNK_HEADER_SIZE {
            return Err(SlpyError::Truncated);
        }
        let ch = ChunkHeader::from_bytes(bytes[pos..pos + CHUNK_HEADER_SIZE].try_into().unwrap());
        let size = usize::try_from(ch.size).map_err(|_| SlpyError::Corrupt("chunk size overflow"))?;
        let crc_len = if with_crc { 4 } else { 0 };
        let payload_start = pos + CHUNK_HEADER_SIZE;
        let end = payload_start
            .checked_add(size)
            .and_then(|e| e.checked_add(crc_len))
            .ok_or(SlpyError::Corrupt("chunk size overflow"))?;
        if end > len {
            return Err(SlpyError::Truncated);
        }
        let payload = &bytes[payload_start..payload_start + size];
        let stored_crc = with_crc.then(|| {
            u32::from_le_bytes(bytes[payload_start + size..end].try_into().unwrap())
        });
        f(pos as u64, &ch, payload, stored_crc)?;
        if ch.tag == TAG_TRLR && end != len {
            return Err(SlpyError::Corrupt("data after TRLR"));
        }
        pos = end;
    }
    Ok(())
}

impl<'a> SlpyReader<'a> {
    /// Parse header, verify the TRLR is present (absence ⇒ [`crate::SlpyError::Truncated`]
    /// ⇒ factory rerun, PLAN §4), and load FIDX via `index_offset`. Skips
    /// unknown non-required chunks by size; unknown required chunks are a
    /// hard error (PLAN §4 forward compat).
    pub fn open(bytes: &'a [u8]) -> Result<SlpyReader<'a>> {
        if bytes.len() < HEADER_SIZE as usize {
            return Err(SlpyError::Truncated);
        }
        let header = SlpyHeader::from_bytes(bytes[..HEADER_SIZE as usize].try_into().unwrap())?;

        // M0 reader supports the M0 writer profile; delta/lz4 land at M1.
        if header.codec != codec::ZSTD {
            return Err(SlpyError::Corrupt("unsupported codec (M0 reader: zstd only)"));
        }
        if header.filter != filter::INTRA {
            return Err(SlpyError::Corrupt("unsupported filter (M0 reader: intra only)"));
        }
        let plane_count = header.plane_count as usize;
        if plane_count == 0 || plane_count > 8 {
            return Err(SlpyError::Corrupt("plane_count out of range"));
        }
        if header.plane_ids[..plane_count].contains(&0) {
            return Err(SlpyError::Corrupt("plane id 0 in registry"));
        }
        if header.flags & header_flags::INDEX_PRESENT == 0 {
            return Err(SlpyError::Corrupt("M0 reader requires a frame index (flags bit0)"));
        }

        let with_crc = header.flags & header_flags::CRCS_PRESENT != 0;
        let mut fram_offsets: Vec<u64> = Vec::new();
        let mut fidx: Option<(u64, Vec<FrameIndexEntry>)> = None;
        let mut meta_offset_seen: Option<u64> = None;
        let mut trlr_seen = false;

        walk_chunks(bytes, with_crc, |offset, ch, payload, _crc| {
            match ch.tag {
                TAG_META => {
                    if meta_offset_seen.is_some() {
                        return Err(SlpyError::Corrupt("duplicate META chunk"));
                    }
                    meta_offset_seen = Some(offset);
                }
                TAG_NORM => {} // known; consumed at M1+
                TAG_FRAM => fram_offsets.push(offset),
                TAG_FIDX => {
                    if fidx.is_some() {
                        return Err(SlpyError::Corrupt("duplicate FIDX chunk"));
                    }
                    if payload.len() % FIDX_ENTRY_SIZE != 0 {
                        return Err(SlpyError::Corrupt("FIDX size not a multiple of 16"));
                    }
                    let mut entries = Vec::with_capacity(payload.len() / FIDX_ENTRY_SIZE);
                    for row in payload.chunks_exact(FIDX_ENTRY_SIZE) {
                        entries.push(FrameIndexEntry::from_bytes(row.try_into().unwrap()));
                    }
                    fidx = Some((offset, entries));
                }
                TAG_TRLR => {
                    if payload != TRLR_PAYLOAD {
                        return Err(SlpyError::Corrupt("bad TRLR payload"));
                    }
                    trlr_seen = true;
                }
                tag => {
                    if ch.is_required() {
                        return Err(SlpyError::UnknownRequiredChunk(tag));
                    }
                    // unknown non-required: skipped by size (PLAN §4)
                }
            }
            Ok(())
        })?;

        if !trlr_seen {
            return Err(SlpyError::Truncated);
        }
        let (fidx_offset, index) = fidx.ok_or(SlpyError::Corrupt("missing FIDX chunk"))?;
        if header.index_offset != fidx_offset {
            return Err(SlpyError::Corrupt("header index_offset does not match FIDX chunk"));
        }
        if index.len() != header.frame_count as usize {
            return Err(SlpyError::Corrupt("FIDX entry count != header frame_count"));
        }
        if fram_offsets.len() != index.len() {
            return Err(SlpyError::Corrupt("FRAM chunk count != header frame_count"));
        }
        if index.iter().zip(&fram_offsets).any(|(e, &o)| e.offset != o) {
            return Err(SlpyError::Corrupt("FIDX offset does not match FRAM chunk"));
        }
        if let Some(m) = meta_offset_seen
            && header.meta_offset != m
        {
            return Err(SlpyError::Corrupt("header meta_offset does not match META chunk"));
        }

        let dctx = zstd::bulk::Decompressor::new()?;
        Ok(SlpyReader { bytes, header, index, dctx })
    }

    #[inline]
    pub fn header(&self) -> &SlpyHeader {
        &self.header
    }

    #[inline]
    pub fn frame_count(&self) -> u32 {
        self.header.frame_count
    }

    /// Decode the META chunk (CBOR; unknown keys ignored, PLAN §4).
    pub fn meta(&self) -> Result<Meta> {
        let offset = usize::try_from(self.header.meta_offset)
            .map_err(|_| SlpyError::Corrupt("meta_offset overflow"))?;
        if offset < HEADER_SIZE as usize || offset + CHUNK_HEADER_SIZE > self.bytes.len() {
            return Err(SlpyError::Corrupt("meta_offset out of range"));
        }
        let ch = ChunkHeader::from_bytes(
            self.bytes[offset..offset + CHUNK_HEADER_SIZE].try_into().unwrap(),
        );
        if ch.tag != TAG_META {
            return Err(SlpyError::Corrupt("meta_offset does not point at a META chunk"));
        }
        let size = usize::try_from(ch.size).map_err(|_| SlpyError::Corrupt("chunk size overflow"))?;
        let start = offset + CHUNK_HEADER_SIZE;
        let payload = self
            .bytes
            .get(start..start + size)
            .ok_or(SlpyError::Truncated)?;
        ciborium::de::from_reader(payload).map_err(|_| SlpyError::BadMeta)
    }

    /// Stored dimensions of a plane in this asset, or `None` if the plane id
    /// is not in the header registry. Y/E/Ex/Ey/H are `base_w × base_h`;
    /// C is half res (PLAN §4 planes).
    pub fn plane_dims(&self, plane_id: u8) -> Option<(u16, u16)> {
        let n = (self.header.plane_count as usize).min(8);
        if !self.header.plane_ids[..n].contains(&plane_id) {
            return None;
        }
        let (w, h) = (self.header.base_w, self.header.base_h);
        if plane_id == crate::header::plane_id::C {
            Some((w / 2, h / 2))
        } else {
            Some((w, h))
        }
    }

    /// Decode one plane of one frame into `dst` (len ≥ the plane's raw size;
    /// `&mut self` for the reused zstd context). M0 assets are intra-only, so
    /// this is exactly one zstd block decode — no keyframe rolling (that
    /// arrives with temporal delta at M1). Per-plane subblocks let low tiers
    /// skip planes they don't need (PLAN §4). Returns the raw byte count.
    pub fn decode_plane_into(&mut self, frame_idx: u32, plane_id: u8, dst: &mut [u8]) -> Result<usize> {
        let n = (self.header.plane_count as usize).min(8);
        if !self.header.plane_ids[..n].contains(&plane_id) {
            return Err(SlpyError::BadPlaneId(plane_id));
        }
        let entry = *self
            .index
            .get(frame_idx as usize)
            .ok_or(SlpyError::BadFrameIndex(frame_idx))?;

        // Bounds were validated by the open() walk; re-check defensively.
        let offset = usize::try_from(entry.offset)
            .map_err(|_| SlpyError::Corrupt("FRAM offset overflow"))?;
        if offset + CHUNK_HEADER_SIZE > self.bytes.len() {
            return Err(SlpyError::Corrupt("FRAM offset out of range"));
        }
        let ch = ChunkHeader::from_bytes(
            self.bytes[offset..offset + CHUNK_HEADER_SIZE].try_into().unwrap(),
        );
        if ch.tag != TAG_FRAM {
            return Err(SlpyError::Corrupt("FIDX entry does not point at a FRAM chunk"));
        }
        let size = usize::try_from(ch.size).map_err(|_| SlpyError::Corrupt("chunk size overflow"))?;
        let start = offset + CHUNK_HEADER_SIZE;
        let payload = self
            .bytes
            .get(start..start + size)
            .ok_or(SlpyError::Truncated)?;

        if payload.len() < 5 {
            return Err(SlpyError::Corrupt("FRAM payload too short"));
        }
        let stored_idx = u32::from_le_bytes(payload[..4].try_into().unwrap());
        if stored_idx != frame_idx {
            return Err(SlpyError::Corrupt("FRAM frame_idx does not match FIDX position"));
        }

        // Scan plane subblocks: plane_id u8 | comp_size u32 | raw_size u32 |
        // zstd bytes, each subblock occupying align64(9 + comp_size) bytes.
        let mut p = 5usize;
        for _ in 0..n {
            if payload.len() - p < 9 {
                return Err(SlpyError::Corrupt("truncated plane subblock"));
            }
            let id = payload[p];
            let comp_size = u32::from_le_bytes(payload[p + 1..p + 5].try_into().unwrap()) as usize;
            let raw_size = u32::from_le_bytes(payload[p + 5..p + 9].try_into().unwrap()) as usize;
            let data_start = p + 9;
            let data_end = data_start
                .checked_add(comp_size)
                .ok_or(SlpyError::Corrupt("subblock size overflow"))?;
            if data_end > payload.len() {
                return Err(SlpyError::Corrupt("plane subblock overruns FRAM payload"));
            }
            if id == plane_id {
                if dst.len() < raw_size {
                    return Err(SlpyError::Corrupt("dst buffer smaller than plane raw size"));
                }
                let written = self
                    .dctx
                    .decompress_to_buffer(&payload[data_start..data_end], &mut dst[..raw_size])?;
                if written != raw_size {
                    return Err(SlpyError::Corrupt("decoded size != raw_size"));
                }
                return Ok(raw_size);
            }
            let sub = align64(9 + comp_size as u64) as usize;
            p = p
                .checked_add(sub)
                .filter(|&q| q <= payload.len())
                .ok_or(SlpyError::Corrupt("subblock padding overruns FRAM payload"))?;
        }
        Err(SlpyError::BadPlaneId(plane_id))
    }

    /// Full-file integrity walk for `sleepy-factory inspect` (PLAN §5 CLI):
    /// re-walk all chunks, verify every CRC32 and the TRLR.
    pub fn verify(&self) -> Result<()> {
        let with_crc = self.header.flags & header_flags::CRCS_PRESENT != 0;
        let mut trlr_ok = false;
        walk_chunks(self.bytes, with_crc, |_, ch, payload, stored_crc| {
            if let Some(stored) = stored_crc
                && crc32fast::hash(payload) != stored
            {
                return Err(SlpyError::CrcMismatch { tag: ch.tag });
            }
            if ch.tag == TAG_TRLR {
                trlr_ok = payload == TRLR_PAYLOAD;
            }
            Ok(())
        })?;
        if !trlr_ok {
            return Err(SlpyError::Truncated);
        }
        Ok(())
    }
}
