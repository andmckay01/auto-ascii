//! Deterministic SLPY writer (PLAN §4, full M1 profile).
//!
//! Stream shape: `HEADER | META | [NORM] | FRAM × frame_count | FIDX | TRLR`.
//! Default profile: `codec = zstd`, `filter = temporal-delta` with a keyframe
//! every `keyframe_ivl` frames (flags bit0), per-plane subblocks padded to
//! 64-byte alignment. `frame_count` + `index_offset` are patched into the
//! header at close (writer streams frames first) — the one `Seek` use.
//!
//! Byte-determinism is an acceptance gate (PLAN §4/§6): fixed zstd level, no
//! timestamps, no map-ordering ambiguity (META is a struct), identical input
//! ⇒ byte-identical file, CRC goldens committed.
//!
//! ## Wire freeze (details PLAN §4 leaves open — frozen at M0, kept at M1)
//!
//! - **Chunk flags as written:** META = 0 and NORM = 0 (skippable),
//!   FRAM / FIDX / TRLR = `chunk_flags::REQUIRED`.
//! - **Subblock 64-B alignment:** each `[plane_id u8 | comp_size u32 |
//!   raw_size u32 | zstd bytes]` subblock occupies `align64(9 + comp_size)`
//!   bytes inside the FRAM payload; the zero pad bytes are part of the
//!   payload, so they are counted by the chunk `size` field and covered by
//!   the chunk CRC32.
//! - **FIDX `comp_size`:** the FRAM chunk's payload size (its `size` field),
//!   i.e. everything between the chunk header and the trailing CRC.
//! - **`meta_offset`:** always 64 (META is written immediately after the
//!   header). NORM, when present, immediately follows META.
//! - **Plane raw sizes:** [`crate::header::plane_raw_size`] — `base_w ×
//!   base_h` bytes for Y/E/Ex/Ey/H; `(base_w/2) × (base_h/2) × 2` bytes for
//!   C (RGB565, PLAN §4).
//! - **Temporal delta (M1):** for non-keyframes, each plane stores
//!   `cur − prev (mod 256)` per byte against the *previous frame as handed
//!   to the writer* (== previous decoded frame); keyframes store the plane
//!   intra. Keyframes fall on `frame_idx % keyframe_ivl == 0`.
//! - **NORM records:** [`crate::norm::ShotRecord`] wire layout, 24 B each,
//!   `first_frame` strictly increasing from 0.

use std::io::{Seek, SeekFrom, Write};

use crate::chunk::{
    CHUNK_HEADER_SIZE, ChunkHeader, FIDX_ENTRY_SIZE, FrameIndexEntry, TAG_FIDX, TAG_FRAM,
    TAG_META, TAG_NORM, TAG_TRLR, TRLR_PAYLOAD, align64, chunk_flags, frame_flags,
};
use crate::error::{Result, SlpyError};
use crate::header::{
    BASE_H, BASE_W, HEADER_SIZE, SlpyHeader, VERSION_MAJOR, VERSION_MINOR, codec, filter,
    header_flags, plane_id, plane_raw_size,
};
use crate::meta::Meta;
use crate::norm::ShotRecord;

/// Writer configuration (header fields + encode knobs). Defaults are the M1
/// SLPY v1 profile (PLAN §4): temporal delta + zstd-19, keyframe every 60
/// frames, Y plane, CRCs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterOptions {
    pub fps_num: u16,
    pub fps_den: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    /// Plane IDs in subblock order (PLAN §4 registry). Must be known IDs
    /// (the writer needs their geometry, [`plane_raw_size`]).
    pub plane_ids: Vec<u8>,
    /// `codec::*` — ZSTD only.
    pub codec: u8,
    /// `filter::*` — TEMPORAL_DELTA (M1 default) or INTRA (M0 profile).
    pub filter: u8,
    /// Keyframe cadence for TEMPORAL_DELTA (PLAN §4: 60). Ignored by INTRA
    /// (every frame is keyframe-flagged) but must still be ≥ 1.
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
            filter: filter::TEMPORAL_DELTA,
            keyframe_ivl: 60,
            zstd_level: 19,
            with_crc: true,
        }
    }
}

/// One uncompressed plane handed to [`SlpyWriter::write_frame`].
/// `data.len()` must equal the plane's raw size ([`plane_raw_size`]).
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
    /// Raw plane sizes, parallel to `opts.plane_ids` (validated at `new`).
    raw_sizes: Vec<usize>,
    /// FIDX rows accumulated as frames stream (PLAN §4: FIDX written last).
    index: Vec<FrameIndexEntry>,
    frames_written: u32,
    norm_written: bool,
    /// Current absolute file offset (tracked, not queried — deterministic
    /// and seek-free on the streaming path).
    pos: u64,
    /// Reused zstd context — one compressor for the whole file (fixed level,
    /// no dictionary: byte-determinism).
    cctx: zstd::bulk::Compressor<'static>,
    /// Previous frame's raw planes (TEMPORAL_DELTA only), parallel to
    /// `opts.plane_ids` — the delta reference (PLAN §4).
    prev: Vec<Vec<u8>>,
    /// Reused delta scratch (`cur − prev mod 256` before compression).
    delta_buf: Vec<u8>,
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
            return Err(SlpyError::Corrupt("writer: codec must be zstd"));
        }
        if opts.filter != filter::INTRA && opts.filter != filter::TEMPORAL_DELTA {
            return Err(SlpyError::Corrupt("writer: unknown filter"));
        }
        if opts.keyframe_ivl == 0 {
            return Err(SlpyError::Corrupt("writer: keyframe_ivl must be >= 1"));
        }
        // §4 geometry term (M1-review fix 1): base dims must be EVEN and
        // >= 2. The C plane lives at (base_w/2, base_h/2) — odd or
        // degenerate dims yield a zero-dimension chroma plane that panicked
        // the player's Resampler::build (base_w == 1 → C width 0). Enforced
        // by writer AND reader so no such asset can exist or be read.
        if opts.base_w < 2
            || opts.base_h < 2
            || !opts.base_w.is_multiple_of(2)
            || !opts.base_h.is_multiple_of(2)
        {
            return Err(SlpyError::Corrupt("writer: base dimensions must be even and >= 2"));
        }
        // M0 adversarial-review fix: fps_num == 0 reached a divide-by-zero
        // Duration panic in the player; reject at the source (both ends).
        if opts.fps_num == 0 || opts.fps_den == 0 {
            return Err(SlpyError::Corrupt("writer: fps_num and fps_den must be nonzero"));
        }
        let raw_sizes = opts
            .plane_ids
            .iter()
            .map(|&id| {
                plane_raw_size(opts.base_w, opts.base_h, id)
                    .ok_or(SlpyError::Corrupt("writer: plane id outside the known registry"))
            })
            .collect::<Result<Vec<usize>>>()?;

        let cctx = zstd::bulk::Compressor::new(opts.zstd_level)?;

        let mut meta_buf = Vec::new();
        ciborium::ser::into_writer(meta, &mut meta_buf).map_err(|_| SlpyError::BadMeta)?;

        let (prev, delta_buf) = if opts.filter == filter::TEMPORAL_DELTA {
            let max_raw = raw_sizes.iter().copied().max().unwrap_or(0);
            (raw_sizes.iter().map(|&s| vec![0u8; s]).collect(), vec![0u8; max_raw])
        } else {
            (Vec::new(), Vec::new())
        };

        let mut this = SlpyWriter {
            w,
            opts,
            raw_sizes,
            index: Vec::new(),
            frames_written: 0,
            norm_written: false,
            pos: 0,
            cctx,
            prev,
            delta_buf,
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

    /// Write the NORM chunk (per-shot runtime levels + cut flags, PLAN §4/§5).
    /// Must be called before the first [`write_frame`](SlpyWriter::write_frame)
    /// and at most once. `shots[0].first_frame` must be 0 and `first_frame`
    /// strictly increasing; upper bounds are checked by the reader against
    /// the final `frame_count` (unknown while streaming).
    pub fn write_norm(&mut self, shots: &[ShotRecord]) -> Result<()> {
        if self.frames_written > 0 {
            return Err(SlpyError::Corrupt("writer: NORM must precede all frames"));
        }
        if self.norm_written {
            return Err(SlpyError::Corrupt("writer: duplicate NORM chunk"));
        }
        if shots.is_empty() {
            return Err(SlpyError::Corrupt("writer: NORM requires at least one shot"));
        }
        if shots[0].first_frame != 0 {
            return Err(SlpyError::Corrupt("writer: first shot must start at frame 0"));
        }
        if shots.windows(2).any(|w| w[1].first_frame <= w[0].first_frame) {
            return Err(SlpyError::Corrupt("writer: shot first_frame must be strictly increasing"));
        }

        let mut payload = Vec::with_capacity(shots.len() * crate::norm::NORM_RECORD_SIZE);
        for shot in shots {
            payload.extend_from_slice(&shot.to_bytes());
        }
        emit_chunk(&mut self.w, &mut self.pos, self.opts.with_crc, TAG_NORM, 0, &payload)?;
        self.norm_written = true;
        Ok(())
    }

    /// Append one FRAM chunk: `frame_idx u32 | flags u8 | [plane_id u8 |
    /// comp_size u32 | raw_size u32 | zstd bytes] × plane_count`, each plane
    /// subblock padded to 64-byte alignment (PLAN §4). Planes must match
    /// `opts.plane_ids` in id and order. Under TEMPORAL_DELTA, frames on the
    /// keyframe cadence are stored intra (flags bit0); all others store the
    /// per-byte delta against the previous frame. Records the FIDX row.
    pub fn write_frame(&mut self, planes: &[PlaneRef<'_>]) -> Result<()> {
        if planes.len() != self.opts.plane_ids.len() {
            return Err(SlpyError::Corrupt("writer: plane count does not match plane_ids"));
        }
        // Validate everything BEFORE mutating delta state: a rejected frame
        // must leave `prev` untouched or later deltas would desync.
        for ((plane, &want), &raw_size) in
            planes.iter().zip(&self.opts.plane_ids).zip(&self.raw_sizes)
        {
            if plane.id != want {
                return Err(SlpyError::Corrupt("writer: plane id/order mismatch"));
            }
            if plane.data.len() != raw_size {
                return Err(SlpyError::Corrupt("writer: plane data length != raw size"));
            }
        }
        if self.frames_written == u32::MAX {
            return Err(SlpyError::Corrupt("writer: frame count overflow"));
        }
        let frame_idx = self.frames_written;
        let delta_filter = self.opts.filter == filter::TEMPORAL_DELTA;
        // Keyframe cadence (PLAN §4): every keyframe_ivl frames from 0.
        // INTRA: every frame stands alone and is keyframe-flagged.
        let keyframe =
            !delta_filter || frame_idx.is_multiple_of(u32::from(self.opts.keyframe_ivl));
        let flags = if keyframe { frame_flags::KEYFRAME } else { 0 };

        self.payload_buf.clear();
        self.payload_buf.extend_from_slice(&frame_idx.to_le_bytes());
        self.payload_buf.push(flags);

        for (i, plane) in planes.iter().enumerate() {
            let raw_size = self.raw_sizes[i];
            let bound = zstd::zstd_safe::compress_bound(raw_size);
            self.comp_buf.resize(bound, 0);

            let comp_size = if keyframe {
                self.cctx.compress_to_buffer(plane.data, &mut self.comp_buf[..])?
            } else {
                // Temporal byte-delta pre-pass (PLAN §4): cur − prev mod 256.
                for ((d, &c), &p) in
                    self.delta_buf[..raw_size].iter_mut().zip(plane.data).zip(&self.prev[i])
                {
                    *d = c.wrapping_sub(p);
                }
                self.cctx.compress_to_buffer(&self.delta_buf[..raw_size], &mut self.comp_buf[..])?
            };
            if delta_filter {
                self.prev[i].copy_from_slice(plane.data);
            }
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
