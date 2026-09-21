//! Flattening a [`Composition`] into one `.ascii` file (PLAN-M6-M8 §3).
//!
//! Playback never needs this — a composition plays virtually, switching
//! decoders at clip boundaries (§0.4) — but one file is sometimes what you
//! want to hand someone, and `auto-ascii cut` is exactly an export of a
//! one-clip composition.
//!
//! What it does per output frame: locate the clip on top, decode its planes
//! (sequential roll where the walk is sequential, FIDX seek otherwise) and
//! hand them to [`AsciiWriter::write_frame`] unchanged — planes are copied,
//! never re-derived, so the export is exactly what the composition plays.
//! Gaps write black planes. The NORM table is built in one pre-pass over
//! the clips' own shot tables, with no decoding at all: one record per
//! (clip slice ∩ source shot), rebased to output frames and cut-flagged at
//! every clip boundary and gap edge.

use std::io::BufWriter;
use std::path::{Path, PathBuf};

use auto_ascii_format::{
    AsciiError, AsciiReader, AsciiWriter, Meta, PlaneLevels, PlaneRef, ShotRecord, WriterOptions,
    codec, filter, norm_flags, plane_raw_size,
};
use memmap2::Mmap;

use crate::composition::Composition;
use crate::error::Error;

/// Encode knobs for [`export`]. The defaults mirror the factory's
/// committed `params.toml` `[build]` section — a flattened composition is
/// the same kind of asset the factory writes, and an export that quietly
/// used a different cadence or level would be a second profile nobody
/// asked for. (`auto-ascii-format`'s own `WriterOptions::default()`
/// deliberately stays at zstd-19: the format crate's default is not the
/// factory's policy.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Keyframe cadence for the temporal-delta filter (params.toml
    /// `[build] keyframe_ivl`, PLAN §4).
    pub keyframe_ivl: u8,
    /// zstd level for frame payloads (params.toml `[build] zstd_level`:
    /// 15, audited against 19 at +0.91% bytes for 2.84× the build speed).
    pub zstd_level: i32,
}

impl Default for ExportOptions {
    fn default() -> ExportOptions {
        ExportOptions { keyframe_ivl: 60, zstd_level: 15 }
    }
}

/// What [`export`] wrote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportReport {
    /// Frames written (the composition's frame count).
    pub frames: u32,
    /// Frame rate of the written asset (the composition fps).
    pub fps: f64,
    /// Size of the file on disk.
    pub bytes: u64,
    /// NORM records written: one per (clip slice ∩ source shot), plus one
    /// per gap.
    pub shots: u32,
    /// How many of those carry the CUT flag (PLAN §3.5: the player resets
    /// hysteresis there).
    pub cuts: u32,
}

/// Flatten `comp` into one ASCI asset at `out` (PLAN-M6-M8 §3).
///
/// `comp` must be resolved ([`Composition::resolve`]) and every clip must
/// share one base resolution and one plane set — mixed shapes play fine but
/// cannot be written into a single header, and the error says which clip to
/// re-import.
///
/// Atomic: the frames go to `<out>.part` and are renamed over `out` only
/// once the container is complete (FIDX, TRLR and the patched header), so a
/// failure leaves an existing `out` exactly as it was and no debris behind.
/// An existing `out` is replaced on success.
///
/// # Errors
/// [`Error::Config`] for an unresolved composition or a clip whose shape
/// does not match the first one; [`Error::Io`]/[`Error::Format`] when a
/// clip cannot be read or the output cannot be written;
/// [`Error::Decode`] on a corrupt frame payload.
pub fn export(
    comp: &Composition,
    out: &Path,
    opts: &ExportOptions,
) -> Result<ExportReport, Error> {
    if !comp.is_resolved() {
        return Err(Error::Config(
            "composition is not resolved (call Composition::resolve first)".into(),
        ));
    }
    let spans = comp.timeline();
    let names: Vec<&str> = comp.clips().iter().map(|c| c.name.as_str()).collect();
    let first = spans[0];
    for (idx, span) in spans.iter().enumerate().skip(1) {
        if (span.base_w, span.base_h) != (first.base_w, first.base_h) {
            return Err(Error::Config(format!(
                "clip {idx} ({}) stores {}x{} but clip 0 ({}) stores {}x{} — an export \
                 is one file with one base resolution; re-import it with \
                 `auto-ascii import --res {}x{}`",
                names[idx],
                span.base_w,
                span.base_h,
                names[0],
                first.base_w,
                first.base_h,
                first.base_w,
                first.base_h,
            )));
        }
        if span.planes() != first.planes() {
            return Err(Error::Config(format!(
                "clip {idx} ({}) carries planes {:?} but clip 0 ({}) carries {:?} — an \
                 export is one file with one plane registry; re-import it with the same \
                 factory build",
                names[idx],
                span.planes(),
                names[0],
                first.planes(),
            )));
        }
    }
    let plane_ids: Vec<u8> = first.planes().to_vec();
    let raw_sizes: Vec<usize> = plane_ids
        .iter()
        .map(|&id| {
            plane_raw_size(first.base_w, first.base_h, id).ok_or_else(|| {
                Error::Config(format!(
                    "clip 0 ({}) carries plane id {id}, whose geometry this build does \
                     not know — it cannot be re-written",
                    names[0]
                ))
            })
        })
        .collect::<Result<Vec<usize>, Error>>()?;

    // One mapping per clip — address space, not memory, and it proves
    // every clip is there before a byte is written. The decoders behind
    // them are built on demand (see `SourcePool`), because a decoder is
    // where the megabytes are.
    let mut maps: Vec<Mmap> = Vec::with_capacity(spans.len());
    for clip in comp.clips() {
        let file = std::fs::File::open(&clip.path)
            .map_err(|source| Error::Io { path: clip.path.clone(), source })?;
        // SAFETY: read-only private map of a file we never mutate through
        // this mapping; the standard not-truncated-mid-use contract.
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| Error::Io { path: clip.path.clone(), source })?;
        maps.push(map);
    }

    let frames = comp.frame_count();
    // The NORM pre-pass reads shot tables and nothing else, so it opens
    // each clip, copies its handful of 24-byte records and lets the reader
    // (and its frame index) go again.
    let mut clip_shots: Vec<Vec<ShotRecord>> = Vec::with_capacity(maps.len());
    for (idx, map) in maps.iter().enumerate() {
        let reader = AsciiReader::open(map).map_err(|source| Error::Format {
            path: comp.clips()[idx].path.clone(),
            source,
        })?;
        clip_shots.push(reader.shots().to_vec());
    }
    let shots = norm_records(comp, &clip_shots, frames);
    let cuts = shots.iter().filter(|s| s.is_cut()).count() as u32;

    let (fps_num, fps_den) = comp.fps_ratio();
    let wopts = WriterOptions {
        fps_num,
        fps_den,
        base_w: first.base_w,
        base_h: first.base_h,
        aspect_num: first.aspect_num,
        aspect_den: first.aspect_den,
        plane_ids: plane_ids.clone(),
        codec: codec::ZSTD,
        filter: filter::TEMPORAL_DELTA,
        keyframe_ivl: opts.keyframe_ivl.max(1),
        zstd_level: opts.zstd_level,
        with_crc: true,
    };
    let meta = Meta {
        factory_version: concat!("auto-ascii ", env!("CARGO_PKG_VERSION"), " compose").to_owned(),
        // A name, never a path: META must stay byte-deterministic (PLAN §4).
        source: comp.name().to_owned(),
        palette_hints: Vec::new(),
    };
    // Write to `<out>.part` and rename on success, exactly as the factory's
    // build pass does: a half-written asset (no FIDX, no TRLR, frame_count
    // 0) must never replace a good one, and a failed export must leave
    // nothing behind to be mistaken for one.
    let part = part_path(out);
    let mut pool = SourcePool::new(maps.len(), raw_sizes);
    let written = write_asset(&part, wopts, &meta, &shots, comp, &mut pool, &maps, &plane_ids);
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, out).map_err(|source| {
        let _ = std::fs::remove_file(&part);
        // Error::Io names the path and carries the OS reason; the failure
        // is the rename, which is why the part file goes with it.
        Error::Io { path: out.into(), source }
    })?;
    Ok(ExportReport { frames, fps: comp.fps(), bytes, shots: shots.len() as u32, cuts })
}

/// `<out>.part` — the scratch name [`export`] writes through.
fn part_path(out: &Path) -> PathBuf {
    let mut os = out.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

/// Write the whole flattened asset to `path` and return its size. Every
/// failure leaves the caller to remove the file — nothing here is
/// recoverable in place.
#[allow(clippy::too_many_arguments)] // one call site; the alternative is a
// struct that exists only to be destructured back into these eight
fn write_asset<'a>(
    path: &Path,
    wopts: WriterOptions,
    meta: &Meta,
    shots: &[ShotRecord],
    comp: &Composition,
    pool: &mut SourcePool<'a>,
    maps: &'a [Mmap],
    plane_ids: &[u8],
) -> Result<u64, Error> {
    let file = std::fs::File::create(path)
        .map_err(|source| Error::Io { path: path.into(), source })?;
    // Writer failures are an OS problem (a full disk) or a broken invariant
    // in what we handed it; report the first as what it is and let the
    // second surface as the container error it carries.
    let fmt = |source| match source {
        AsciiError::Io(source) => Error::Io { path: path.into(), source },
        source => Error::Format { path: path.into(), source },
    };
    let mut writer = AsciiWriter::new(BufWriter::new(file), wopts, meta).map_err(fmt)?;
    writer.write_norm(shots).map_err(fmt)?;

    // Black planes for gap frames: zero is black luma, and zero RGB565 is
    // black chroma, so one zeroed buffer per plane serves.
    let black: Vec<Vec<u8>> = pool.raw_sizes.iter().map(|&n| vec![0u8; n]).collect();
    for frame in 0..comp.frame_count() {
        match comp.locate_frame(frame) {
            Some(loc) => {
                let source = pool.front(loc.clip_idx, maps, comp)?;
                source.load(loc.local_frame, plane_ids)?;
                let refs: Vec<PlaneRef<'_>> = plane_ids
                    .iter()
                    .zip(&source.planes)
                    .map(|(&id, data)| PlaneRef { id, data })
                    .collect();
                writer.write_frame(&refs).map_err(fmt)?;
            }
            None => {
                let refs: Vec<PlaneRef<'_>> = plane_ids
                    .iter()
                    .zip(&black)
                    .map(|(&id, data)| PlaneRef { id, data })
                    .collect();
                writer.write_frame(&refs).map_err(fmt)?;
            }
        }
    }
    let out_file = writer
        .finish()
        .map_err(fmt)?
        .into_inner()
        .map_err(|e| Error::Io { path: path.into(), source: e.into_error() })?;
    Ok(out_file
        .metadata()
        .map_err(|source| Error::Io { path: path.into(), source })?
        .len())
}

/// How many clip decoders an export keeps live at once. Each one holds a
/// full set of plane buffers (roughly a megabyte for a 480×270 six-plane
/// clip) plus that asset's frame index, and a composition may stitch an
/// unbounded number of clips — so, as the player's deck does, the export
/// keeps the most recently used few. Four is enough that no realistic cut
/// pattern thrashes: an export walks the timeline in order, so at any
/// moment it needs the clip on top and, at an overlap, the ones around it.
/// Re-opening costs one FIDX parse and one keyframe seek.
const MAX_LIVE_SOURCES: usize = 4;

/// The export's clip decoders, opened on first use and capped at
/// [`MAX_LIVE_SOURCES`] (LRU). Memory is bounded by how many clips are
/// live, not by how many the composition names.
struct SourcePool<'a> {
    sources: Vec<Option<ClipSource<'a>>>,
    /// `clock` when each clip was last used — the LRU key.
    used: Vec<u64>,
    clock: u64,
    /// Raw plane sizes, shared by every decoder (one shape per export).
    raw_sizes: Vec<usize>,
}

impl<'a> SourcePool<'a> {
    fn new(clips: usize, raw_sizes: Vec<usize>) -> SourcePool<'a> {
        SourcePool {
            sources: (0..clips).map(|_| None).collect(),
            used: vec![0; clips],
            clock: 0,
            raw_sizes,
        }
    }

    /// The decoder for clip `idx`, opened over `maps[idx]` if it is not
    /// live, evicting the least recently used one to make room.
    fn front(
        &mut self,
        idx: usize,
        maps: &'a [Mmap],
        comp: &Composition,
    ) -> Result<&mut ClipSource<'a>, Error> {
        if self.sources[idx].is_none() {
            while self.live() >= MAX_LIVE_SOURCES && self.evict(idx) {}
            let reader = AsciiReader::open(&maps[idx]).map_err(|source| Error::Format {
                path: comp.clips()[idx].path.clone(),
                source,
            })?;
            let planes = self.raw_sizes.iter().map(|&n| vec![0u8; n]).collect();
            self.sources[idx] = Some(ClipSource { reader, planes, loaded: None });
        }
        self.clock += 1;
        self.used[idx] = self.clock;
        Ok(self.sources[idx].as_mut().expect("just opened"))
    }

    fn live(&self) -> usize {
        self.sources.iter().filter(|s| s.is_some()).count()
    }

    /// Drop the least recently used decoder (never `keep`); `false` when
    /// there was nothing to drop.
    fn evict(&mut self, keep: usize) -> bool {
        let victim = self
            .sources
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != keep && s.is_some())
            .min_by_key(|(i, _)| self.used[*i])
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                self.sources[i] = None; // reader + plane buffers go together
                true
            }
            None => false,
        }
    }
}

/// One clip's reader plus the standing plane buffers the delta filter needs
/// (PLAN §3.6 step 3: a sequential successor is one memadd; anything else
/// is a FIDX keyframe seek).
struct ClipSource<'a> {
    reader: AsciiReader<'a>,
    /// Parallel to the composition's plane registry.
    planes: Vec<Vec<u8>>,
    /// Frame currently in `planes`.
    loaded: Option<u32>,
}

impl ClipSource<'_> {
    fn load(&mut self, frame: u32, plane_ids: &[u8]) -> Result<(), Error> {
        if self.loaded == Some(frame) {
            return Ok(()); // a slowed clip can show the same frame twice
        }
        let sequential = frame > 0 && self.loaded == Some(frame - 1);
        for (&id, dst) in plane_ids.iter().zip(&mut self.planes) {
            if sequential {
                self.reader.decode_plane_into(frame, id, dst)
            } else {
                self.reader.seek_plane_into(frame, id, dst)
            }
            .map_err(|source| Error::Decode { frame, plane: id, source })?;
        }
        self.loaded = Some(frame);
        Ok(())
    }
}

/// Which source shot (if any) an output frame draws from — the identity a
/// NORM record covers a run of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    /// No clip is on top: black frames with identity levels.
    Gap,
    /// Clip `idx`, inside the source shot starting at `shot` (`None` when
    /// the clip carries no NORM table at all).
    Clip { idx: usize, shot: Option<u32> },
}

/// The NORM pre-pass (PLAN-M6-M8 §3): walk the output frames, break them
/// into runs of one (clip, source shot), and emit one record per run —
/// levels copied from the source shot (the plane registries are identical,
/// so the position-indexed levels transfer verbatim), cut-flagged at every
/// clip boundary and gap edge. No frame payload is touched.
fn norm_records(
    comp: &Composition,
    clip_shots: &[Vec<ShotRecord>],
    frames: u32,
) -> Vec<ShotRecord> {
    let mut records: Vec<ShotRecord> = Vec::new();
    let mut prev: Option<Segment> = None;
    for frame in 0..frames {
        let (segment, levels, source_cut) = match comp.locate_frame(frame) {
            None => (Segment::Gap, [PlaneLevels::default(); 8], false),
            Some(loc) => {
                let shot = shot_at(&clip_shots[loc.clip_idx], loc.local_frame);
                (
                    Segment::Clip { idx: loc.clip_idx, shot: shot.map(|s| s.first_frame) },
                    shot.map_or([PlaneLevels::default(); 8], |s| s.levels),
                    shot.is_some_and(ShotRecord::is_cut),
                )
            }
        };
        if prev == Some(segment) {
            continue;
        }
        // A clip boundary or a gap edge is a hard cut by construction: the
        // picture is unrelated to the one before it. Inside one clip slice
        // the source's own flag decides. Record 0 keeps the source's flag —
        // there is nothing before frame 0 to cut away from.
        let boundary = prev.is_some_and(|p| clip_of(p) != clip_of(segment));
        let flags = if boundary || source_cut { norm_flags::CUT } else { 0 };
        records.push(ShotRecord { first_frame: frame, flags, levels });
        prev = Some(segment);
    }
    records
}

/// The shot covering `frame` in a clip's own NORM table (`None` when the
/// clip carries none) — `AsciiReader::shot_for_frame` over the copy the
/// pre-pass kept.
fn shot_at(shots: &[ShotRecord], frame: u32) -> Option<&ShotRecord> {
    let p = shots.partition_point(|s| s.first_frame <= frame);
    (p > 0).then(|| &shots[p - 1])
}

/// The clip a segment belongs to (`None` for a gap) — what a boundary is
/// measured against.
fn clip_of(segment: Segment) -> Option<usize> {
    match segment {
        Segment::Gap => None,
        Segment::Clip { idx, .. } => Some(idx),
    }
}
