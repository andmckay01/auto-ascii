//! Deterministic SLPY writer (PLAN §4, M0 subset).
//!
//! Stream shape at M0 (SLPY-lite): `HEADER | META | FRAM × frame_count |
//! FIDX | TRLR`. `codec = zstd`, `filter = intra` (every frame keyframe-
//! flagged), Y plane only, per-plane subblocks padded to 64-byte alignment.
//! `index_offset` is patched into the header at close (writer streams frames
//! first) — the one `Seek` use.
//!
//! Byte-determinism is an acceptance gate (M0 (4)): fixed zstd level, no
//! timestamps, no map-ordering ambiguity (META is a struct), identical input
//! ⇒ byte-identical file, CRC goldens committed.
//!
//! ## M0 wire freeze (details PLAN §4 leaves open — M1 must not change these)
//!
//! - **Chunk flags as written:** META = 0 (skippable), FRAM / FIDX / TRLR =
//!   `chunk_flags::REQUIRED`.
//! - **Subblock 64-B alignment:** each `[plane_id u8 | comp_size u32 |
//!   raw_size u32 | zstd bytes]` subblock occupies `align64(9 + comp_size)`
//!   bytes inside the FRAM payload; the zero pad bytes are part of the
//!   payload, so they are counted by the chunk `size` field and covered by
//!   the chunk CRC32.
//! - **FIDX `comp_size`:** the FRAM chunk's payload size (its `size` field),
//!   i.e. everything between the chunk header and the trailing CRC.
//! - **`meta_offset`:** always 64 (META is written immediately after the
//!   header).
//! - **Plane raw sizes:** `base_w × base_h` bytes for Y/E/Ex/Ey/H;
//!   `(base_w/2) × (base_h/2) × 2` bytes for C (RGB565, PLAN §4).

use std::io::{Seek, SeekFrom, Write};

use crate::chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_TRLR, TRLR_PAYLOAD, align64, chunk_flags, frame_flags,
};
use crate::error::{Result, SlpyError};
use crate::header::{
    BASE_H, BASE_W, HEADER_SIZE, SlpyHeader, VERSION_MAJOR, VERSION_MINOR, codec, filter,
    header_flags, plane_id,
};
use crate::meta::Meta;

/// Writer configuration (header fields + encode knobs). Defaults are the M0
/// SLPY-lite profile (PLAN §7 + approved simplification).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterOptions {
    pub fps_num: u16,
    pub fps_den: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    /// Plane IDs in subblock order (PLAN §4 registry). M0: `[Y]`.
    pub plane_ids: Vec<u8>,
    /// `codec::*` — M0 writes ZSTD.
    pub codec: u8,
    /// `filter::*` — M0 writes INTRA (temporal delta at M1).
    pub filter: u8,
    pub keyframe_ivl: u8,
    /// zstd level; PLAN §4 factory default is 19 (decode speed unaffected).
    pub zstd_level: i32,
    /// Emit per-chunk CRC32s and set `header_flags::CRCS_PRESENT` (PLAN §4).
    pub with_crc: bool,
}

impl Default for WriterOptions {
    fn default() -> WriterOptions {
        WriterOptions {
            fps_num: 30,
            fps_den: 1,
            base_w: BASE_W,
            base_h: BASE_H,
            aspect_num: 16,
            aspect_den: 9,
            plane_ids: vec![plane_id::Y],
            codec: codec::ZSTD,
            filter: filter::INTRA,
            keyframe_ivl: 60,
            zstd_level: 19,
            with_crc: true,
        }
    }
}

/// One uncompressed plane handed to [`SlpyWriter::write_frame`].
/// `data.len()` must equal the plane's raw size (`base_w × base_h` for Y).
#[derive(Clone, Copy, Debug)]
pub struct PlaneRef<'a> {
    /// `plane_id::*` — must match `WriterOptions::plane_ids` order.
    pub id: u8,
    pub data: &'a [u8],
}

/// Streaming SLPY writer over any `Write + Seek` (no I/O policy in this
/// crate, PLAN §2 — the factory hands it a `BufWriter<File>`).
pub struct SlpyWriter<W: Write + Seek> {
    w: W,
    opts: WriterOptions,
    /// FIDX rows accumulated as frames stream (PLAN §4: FIDX written last).
    index: Vec<FrameIndexEntry>,
    frames_written: u32,
    /// Current absolute file offset (tracked, not queried — deterministic
    /// and seek-free on the streaming path).
    pos: u64,
    /// Reused zstd context — one compressor for the whole file (fixed level,
    /// no dictionary: byte-determinism).
    cctx: zstd::bulk::Compressor<'static>,
    /// Reused compression scratch buffer.
    comp_buf: Vec<u8>,
    /// Reused FRAM payload assembly buffer.
    payload_buf: Vec<u8>,
}

/// Emit one chunk: 16-B header | payload | crc32(payload) when enabled.
/// Free function over disjoint fields so callers can keep field borrows.
fn emit_chunk<W: Write>(
    w: &mut W,
    pos: &mut u64,
    with_crc: bool,
    tag: [u8; 4],
    flags: u8,
    payload: &[u8],
) -> Result<()> {
    let header = ChunkHeader { tag, flags, size: payload.len() as u64 };
    w.write_all(&header.to_bytes())?;
    w.write_all(payload)?;
    *pos += (CHUNK_HEADER_SIZE + payload.len()) as u64;
    if with_crc {
        w.write_all(&crc32fast::hash(payload).to_le_bytes())?;
        *pos += 4;
    }
    Ok(())
}

/// Raw (uncompressed) byte size of a plane (see module docs): full-res u8
/// planes, except C = half-res RGB565 (2 B/px, PLAN §4).
fn plane_raw_size(base_w: u16, base_h: u16, id: u8) -> usize {
    let (w, h) = (base_w as usize, base_h as usize);
    if id == plane_id::C { (w / 2) * (h / 2) * 2 } else { w * h }
}

impl<W: Write + Seek> SlpyWriter<W> {
    /// Write the 64-byte header (with `frame_count`/`index_offset` = 0,
    /// patched in [`finish`](SlpyWriter::finish)) followed by the META chunk.
    /// `meta` must obey the determinism rules on [`Meta`].
    pub fn new(w: W, opts: WriterOptions, meta: &Meta) -> Result<SlpyWriter<W>> {
        let n = opts.plane_ids.len();
        if n == 0 || n > 8 {
            return Err(SlpyError::Corrupt("writer: plane_ids must have 1..=8 entries"));
        }
        if opts.plane_ids.contains(&0) {
            return Err(SlpyError::Corrupt("writer: plane id 0 is reserved"));
        }
        for (i, id) in opts.plane_ids.iter().enumerate() {
            if opts.plane_ids[..i].contains(id) {
                return Err(SlpyError::Corrupt("writer: duplicate plane id"));
            }
        }
        if opts.codec != codec::ZSTD {
            return Err(SlpyError::Corrupt("writer: M0 supports codec=zstd only"));
        }
        if opts.filter != filter::INTRA {
            return Err(SlpyError::Corrupt("writer: M0 supports filter=intra only"));
        }
        if opts.keyframe_ivl == 0 {
            return Err(SlpyError::Corrupt("writer: keyframe_ivl must be >= 1"));
        }
        if opts.base_w == 0 || opts.base_h == 0 {
            return Err(SlpyError::Corrupt("writer: base dimensions must be nonzero"));
        }

        let cctx = zstd::bulk::Compressor::new(opts.zstd_level)?;

        let mut meta_buf = Vec::new();
        ciborium::ser::into_writer(meta, &mut meta_buf).map_err(|_| SlpyError::BadMeta)?;

        let mut this = SlpyWriter {
            w,
            opts,
            index: Vec::new(),
            frames_written: 0,
            pos: 0,
            cctx,
            comp_buf: Vec::new(),
            payload_buf: Vec::new(),
        };

        // Provisional header: frame_count = 0, index_offset = 0 (patched at
        // close, PLAN §4). meta_offset is final: META follows immediately.
        let header = this.make_header(0, 0);
        this.w.write_all(&header.to_bytes())?;
        this.pos = u64::from(HEADER_SIZE);

        emit_chunk(&mut this.w, &mut this.pos, this.opts.with_crc, TAG_META, 0, &meta_buf)?;
        Ok(this)
    }

    /// Append one FRAM chunk: `frame_idx u32 | flags u8 | [plane_id u8 |
    /// comp_size u32 | raw_size u32 | zstd bytes] × plane_count`, each plane
    /// subblock padded to 64-byte alignment (PLAN §4). Planes must match
    /// `opts.plane_ids` in id and order; M0 = the Y plane, keyframe-flagged
    /// (intra). Records the FIDX row.
    pub fn write_frame(&mut self, planes: &[PlaneRef<'_>]) -> Result<()> {
        if planes.len() != self.opts.plane_ids.len() {
            return Err(SlpyError::Corrupt("writer: plane count does not match plane_ids"));
        }
        if self.frames_written == u32::MAX {
            return Err(SlpyError::Corrupt("writer: frame count overflow"));
        }
        let frame_idx = self.frames_written;
        // filter = intra (enforced in new): every frame stands alone.
        let flags = frame_flags::KEYFRAME;

        self.payload_buf.clear();
        self.payload_buf.extend_from_slice(&frame_idx.to_le_bytes());
        self.payload_buf.push(flags);

        for (plane, &want) in planes.iter().zip(&self.opts.plane_ids) {
            if plane.id != want {
                return Err(SlpyError::Corrupt("writer: plane id/order mismatch"));
            }
            let raw_size = plane_raw_size(self.opts.base_w, self.opts.base_h, plane.id);
            if plane.data.len() != raw_size {
                return Err(SlpyError::Corrupt("writer: plane data length != raw size"));
            }

            let bound = zstd::zstd_safe::compress_bound(plane.data.len());
            self.comp_buf.resize(bound, 0);
            let comp_size = self.cctx.compress_to_buffer(plane.data, &mut self.comp_buf[..])?;
            if comp_size > u32::MAX as usize || raw_size > u32::MAX as usize {
                return Err(SlpyError::Corrupt("writer: plane subblock exceeds u32"));
            }

            self.payload_buf.push(plane.id);
            self.payload_buf.extend_from_slice(&(comp_size as u32).to_le_bytes());
            self.payload_buf.extend_from_slice(&(raw_size as u32).to_le_bytes());
            self.payload_buf.extend_from_slice(&self.comp_buf[..comp_size]);
            // Pad the subblock to 64-B alignment (PLAN §4); pads are payload
            // bytes → counted by the chunk `size`, covered by the CRC.
            let sub_len = 9 + comp_size;
            let padded = align64(sub_len as u64) as usize;
            let new_len = self.payload_buf.len() + (padded - sub_len);
            self.payload_buf.resize(new_len, 0);
        }
        if self.payload_buf.len() > u32::MAX as usize {
            return Err(SlpyError::Corrupt("writer: FRAM payload exceeds u32"));
        }

        let offset = self.pos;
        let comp_size = self.payload_buf.len() as u32;
        emit_chunk(
            &mut self.w,
            &mut self.pos,
            self.opts.with_crc,
            TAG_FRAM,
            chunk_flags::REQUIRED,
            &self.payload_buf,
        )?;

        self.index.push(FrameIndexEntry { offset, comp_size, flags });
        self.frames_written += 1;
        Ok(())
    }

    /// Write FIDX (frame_count × 16 B) and TRLR (`"SLPY_END"`), then seek
    /// back and patch `frame_count` + `index_offset` in the header (PLAN §4
    /// "patched at close"). Returns the inner writer (flushed, not synced).
    pub fn finish(mut self) -> Result<W> {
        let index_offset = self.pos;

        let mut fidx = Vec::with_capacity(self.index.len() * FIDX_ENTRY_SIZE);
        for entry in &self.index {
            fidx.extend_from_slice(&entry.to_bytes());
        }
        emit_chunk(
            &mut self.w,
            &mut self.pos,
            self.opts.with_crc,
            TAG_FIDX,
            chunk_flags::REQUIRED,
            &fidx,
        )?;
        emit_chunk(
            &mut self.w,
            &mut self.pos,
            self.opts.with_crc,
            TAG_TRLR,
            chunk_flags::REQUIRED,
            TRLR_PAYLOAD,
        )?;

        let header = self.make_header(self.frames_written, index_offset);
        self.w.flush()?;
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&header.to_bytes())?;
        self.w.flush()?;
        Ok(self.w)
    }

    fn make_header(&self, frame_count: u32, index_offset: u64) -> SlpyHeader {
        let mut plane_ids = [0u8; 8];
        plane_ids[..self.opts.plane_ids.len()].copy_from_slice(&self.opts.plane_ids);
        let mut flags = header_flags::INDEX_PRESENT;
        if self.opts.with_crc {
            flags |= header_flags::CRCS_PRESENT;
        }
        SlpyHeader {
            version_major: VERSION_MAJOR,
            version_minor: VERSION_MINOR,
            flags,
            fps_num: self.opts.fps_num,
            fps_den: self.opts.fps_den,
            base_w: self.opts.base_w,
            base_h: self.opts.base_h,
            aspect_num: self.opts.aspect_num,
            aspect_den: self.opts.aspect_den,
            frame_count,
            plane_count: self.opts.plane_ids.len() as u8,
            codec: self.opts.codec,
            filter: self.opts.filter,
            keyframe_ivl: self.opts.keyframe_ivl,
            plane_ids,
            index_offset,
            meta_offset: u64::from(HEADER_SIZE),
        }
    }
}
