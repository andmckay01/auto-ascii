//! Playback never needs an export — a composition plays virtually,
//! switching decoders at clip boundaries — but one file is sometimes what
//! you want to hand someone, and `auto-ascii cut` is exactly an export of a
//! one-clip composition.
//!
//! What it does per output frame: locate the clip on top, decode its planes
//! (sequential roll where the walk is sequential, FIDX seek otherwise) and
//! hand them to [`AsciiWriter::write_frame`](auto_ascii_format::AsciiWriter::write_frame) unchanged — planes are copied,
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
/// committed `params.toml` `[build]` section, so a flattened composition is
/// encoded like any asset the factory writes. (`auto-ascii-format`'s own
/// `WriterOptions::default()` differs: it stays at zstd-19.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Keyframe cadence for the temporal-delta filter (params.toml
    /// `[build] keyframe_ivl`).
    pub keyframe_ivl: u8,
    /// zstd level for frame payloads (params.toml `[build] zstd_level`,
    /// default 15).
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
    /// How many of those carry the CUT flag (the player resets hysteresis
    /// there).
    pub cuts: u32,
}

/// Flatten `comp` into one ASCI asset at `out`.
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
        source: comp.name().to_owned(),
        palette_hints: Vec::new(),
    };
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
        Error::Io { path: out.into(), source }
    })?;
    Ok(ExportReport { frames, fps: comp.fps(), bytes, shots: shots.len() as u32, cuts })
}

fn part_path(out: &Path) -> PathBuf {
    let mut os = out.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

#[allow(clippy::too_many_arguments)]
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
    let fmt = |source| match source {
        AsciiError::Io(source) => Error::Io { path: path.into(), source },
        source => Error::Format { path: path.into(), source },
    };
    let mut writer = AsciiWriter::new(BufWriter::new(file), wopts, meta).map_err(fmt)?;
    writer.write_norm(shots).map_err(fmt)?;

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

const MAX_LIVE_SOURCES: usize = 4;

struct SourcePool<'a> {
    sources: Vec<Option<ClipSource<'a>>>,
    used: Vec<u64>,
    clock: u64,
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
                self.sources[i] = None;
                true
            }
            None => false,
        }
    }
}

struct ClipSource<'a> {
    reader: AsciiReader<'a>,
    planes: Vec<Vec<u8>>,
    loaded: Option<u32>,
}

impl ClipSource<'_> {
    fn load(&mut self, frame: u32, plane_ids: &[u8]) -> Result<(), Error> {
        if self.loaded == Some(frame) {
            return Ok(());
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    Gap,
    Clip { idx: usize, shot: Option<u32> },
}

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
        let boundary = prev.is_some_and(|p| clip_of(p) != clip_of(segment));
        let flags = if boundary || source_cut { norm_flags::CUT } else { 0 };
        records.push(ShotRecord { first_frame: frame, flags, levels });
        prev = Some(segment);
    }
    records
}

fn shot_at(shots: &[ShotRecord], frame: u32) -> Option<&ShotRecord> {
    let p = shots.partition_point(|s| s.first_frame <= frame);
    (p > 0).then(|| &shots[p - 1])
}

fn clip_of(segment: Segment) -> Option<usize> {
    match segment {
        Segment::Gap => None,
        Segment::Clip { idx, .. } => Some(idx),
    }
}
