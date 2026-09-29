//! `dev inspect`: walk an asset's chunks, sample per-plane value stats, verify
//! every CRC, and optionally dump decoded planes as PGM/PPM images. The
//! report is collected first, then printed as text or as one JSON value.

use std::path::Path;

use auto_ascii_format::{
    AsciiHeader, AsciiReader, CHUNK_HEADER_SIZE, ChunkHeader, TAG_FRAM, frame_flags, header_flags,
    plane_id,
};
use serde::Serialize;

use crate::BoxErr;

#[derive(Serialize)]
pub struct Report {
    pub path: String,
    pub version: String,
    pub file_bytes: u64,
    pub flags: u32,
    pub index_present: bool,
    pub crcs_present: bool,
    pub fps_num: u16,
    pub fps_den: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    pub frames: u32,
    pub duration_secs: f64,
    pub planes: Vec<&'static str>,
    pub codec: u8,
    pub filter: u8,
    pub keyframe_ivl: u8,
    pub index_offset: u64,
    pub meta_offset: u64,
    pub meta: auto_ascii_format::Meta,
    pub shots: Vec<u32>,
    pub cuts: usize,
    pub keyframes: u64,
    pub plane_bytes: Vec<PlaneBytes>,
    pub sampled_frames: Vec<u32>,
    pub plane_stats: Vec<PlaneStats>,
    pub dumped_to: Option<String>,
    pub integrity: &'static str,
}

#[derive(Serialize)]
pub struct PlaneBytes {
    pub plane: &'static str,
    pub compressed: u64,
    pub raw: u64,
}

#[derive(Serialize)]
pub struct PlaneStats {
    pub plane: &'static str,
    pub min: u8,
    pub max: u8,
    pub mean: f64,
    pub nonzero_pct: f64,
    pub highlight_pct: f64,
    pub deep_shadow_pct: f64,
    pub dev_mean: f64,
    pub dev_max: u8,
}

pub fn collect(
    asset: &Path,
    dump_dir: Option<&Path>,
    frames: &[u32],
) -> Result<Report, BoxErr> {
    let file = std::fs::File::open(asset).map_err(|e| format!("open {}: {e}", asset.display()))?;
    let bytes = unsafe { memmap2::Mmap::map(&file) }
        .map_err(|e| format!("mmap {}: {e}", asset.display()))?;
    let reader = AsciiReader::open(&bytes)?;
    let h = reader.header().clone();
    let duration_secs = if h.fps_num > 0 {
        f64::from(h.frame_count) * f64::from(h.fps_den) / f64::from(h.fps_num)
    } else {
        0.0
    };
    let meta = reader.meta()?;
    let shots = reader.shots();
    let walk = walk_frames(&bytes, &h)?;
    if walk.frames != u64::from(h.frame_count) {
        return Err(format!(
            "FRAM walk found {} frames but the header says {}",
            walk.frames, h.frame_count
        )
        .into());
    }

    let mut report = Report {
        path: asset.display().to_string(),
        version: format!("{}.{}", h.version_major, h.version_minor),
        file_bytes: bytes.len() as u64,
        flags: h.flags,
        index_present: h.flags & header_flags::INDEX_PRESENT != 0,
        crcs_present: h.flags & header_flags::CRCS_PRESENT != 0,
        fps_num: h.fps_num,
        fps_den: h.fps_den,
        base_w: h.base_w,
        base_h: h.base_h,
        aspect_num: h.aspect_num,
        aspect_den: h.aspect_den,
        frames: h.frame_count,
        duration_secs,
        planes: h.plane_ids[..h.plane_count as usize].iter().map(|&id| plane_name(id)).collect(),
        codec: h.codec,
        filter: h.filter,
        keyframe_ivl: h.keyframe_ivl,
        index_offset: h.index_offset,
        meta_offset: h.meta_offset,
        meta,
        shots: shots.iter().map(|s| s.first_frame).collect(),
        cuts: shots.iter().filter(|s| s.is_cut()).count(),
        keyframes: walk.keyframes,
        plane_bytes: walk
            .per_plane
            .iter()
            .map(|&(id, compressed, raw)| PlaneBytes { plane: plane_name(id), compressed, raw })
            .collect(),
        sampled_frames: Vec::new(),
        plane_stats: Vec::new(),
        dumped_to: None,
        integrity: "ok",
    };

    if h.frame_count > 0 {
        let sampled = if frames.is_empty() {
            sample_frames(h.frame_count)
        } else {
            if let Some(&f) = frames.iter().find(|&&f| f >= h.frame_count) {
                return Err(format!(
                    "--frame {f} out of range (asset has {} frames)",
                    h.frame_count
                )
                .into());
            }
            frames.to_vec()
        };
        let mut reader = AsciiReader::open(&bytes)?;
        report.plane_stats = plane_stats(&mut reader, &sampled)?;
        if let Some(dir) = dump_dir {
            dump_planes(&mut reader, dir, &sampled)?;
            report.dumped_to = Some(dir.display().to_string());
        }
        report.sampled_frames = sampled;
    }

    reader.verify()?;
    Ok(report)
}

pub fn print(r: &Report) {
    outln!("{}: ASCI v{}", r.path, r.version);
    outln!("  file size:    {} bytes", r.file_bytes);
    outln!(
        "  flags:        {:#06x} (index: {}, crcs: {})",
        r.flags, r.index_present, r.crcs_present
    );
    outln!("  fps:          {}/{}", r.fps_num, r.fps_den);
    outln!("  base res:     {}x{}", r.base_w, r.base_h);
    outln!("  aspect:       {}:{}", r.aspect_num, r.aspect_den);
    outln!("  frames:       {} ({:.2}s)", r.frames, r.duration_secs);
    outln!("  planes:       {} [{}]", r.planes.len(), r.planes.join(", "));
    outln!("  codec/filter: {}/{} (0=raw 1=lz4 2=zstd / 0=intra 1=delta)", r.codec, r.filter);
    outln!("  keyframe ivl: {}", r.keyframe_ivl);
    outln!("  index_offset: {}", r.index_offset);
    outln!("  meta_offset:  {}", r.meta_offset);
    if r.frames > 0 {
        let payload = r.index_offset.saturating_sub(u64::from(auto_ascii_format::HEADER_SIZE));
        outln!(
            "  avg frame:    {:.1} KiB (chunked payload before FIDX)",
            payload as f64 / 1024.0 / f64::from(r.frames)
        );
    }
    outln!(
        "  meta:         factory {} | source {:?} | palette hints {:?}",
        r.meta.factory_version, r.meta.source, r.meta.palette_hints
    );
    if r.shots.is_empty() {
        outln!("  shots:        none (no NORM chunk — pre-M1 asset)");
    } else {
        let starts: Vec<String> = r.shots.iter().take(8).map(u32::to_string).collect();
        outln!(
            "  shots:        {} ({} cut-flagged) starting at [{}{}]",
            r.shots.len(),
            r.cuts,
            starts.join(", "),
            if r.shots.len() > 8 { ", ..." } else { "" }
        );
    }
    outln!(
        "  keyframes:    {} of {} frames (interval {})",
        r.keyframes, r.frames, r.keyframe_ivl
    );
    let mut raw_total = 0u64;
    for p in &r.plane_bytes {
        raw_total += p.raw;
        outln!(
            "  plane {:<2}      {:.2} MiB compressed / {:.2} MiB raw ({:.1}x)",
            p.plane,
            mib(p.compressed),
            mib(p.raw),
            p.raw as f64 / p.compressed.max(1) as f64
        );
    }
    outln!(
        "  compression:  {:.2} MiB raw planes -> {:.2} MiB file ({:.1}x vs raw)",
        mib(raw_total),
        mib(r.file_bytes),
        raw_total as f64 / r.file_bytes.max(1) as f64
    );
    if !r.sampled_frames.is_empty() {
        let n = r.sampled_frames.len();
        outln!(
            "  stats:        over {n} sampled frame{} {:?}",
            if n == 1 { "" } else { "s" },
            r.sampled_frames
        );
    }
    for s in &r.plane_stats {
        match s.plane {
            "E" => outln!(
                "    E   min {} mean {:.1} max {}, nonzero {:.2}% (unthinned edge mass)",
                s.min, s.mean, s.max, s.nonzero_pct
            ),
            "Ex" | "Ey" => outln!(
                "    {:<3} |v-128| mean {:.2} max {} (doubled-angle, bias 128)",
                s.plane, s.dev_mean, s.dev_max
            ),
            "H" => outln!(
                "    H   highlight {:.2}% deep-shadow {:.2}%",
                s.highlight_pct, s.deep_shadow_pct
            ),
            _ => outln!("    {:<3} min {} mean {:.1} max {}", s.plane, s.min, s.mean, s.max),
        }
    }
    if let Some(dir) = &r.dumped_to {
        crate::output::emit_err(&format!(
            "dumped {} frame(s) x {} plane(s) to {dir}\n",
            r.sampled_frames.len(),
            r.planes.len()
        ));
    }
    outln!("  integrity:    OK (all chunk CRCs verified, TRLR present)");
}

fn plane_name(id: u8) -> &'static str {
    match id {
        plane_id::Y => "Y",
        plane_id::E => "E",
        plane_id::EX => "Ex",
        plane_id::EY => "Ey",
        plane_id::H => "H",
        plane_id::C => "C",
        _ => "?",
    }
}

struct FramStats {
    frames: u64,
    keyframes: u64,
    per_plane: Vec<(u8, u64, u64)>,
}

fn walk_frames(bytes: &[u8], h: &AsciiHeader) -> Result<FramStats, BoxErr> {
    let crc_len: usize = if h.flags & header_flags::CRCS_PRESENT != 0 { 4 } else { 0 };
    let end = usize::try_from(h.index_offset).map_err(|_| "index_offset exceeds file")?;
    if end > bytes.len() {
        return Err("index_offset exceeds file".into());
    }
    let mut stats = FramStats { frames: 0, keyframes: 0, per_plane: Vec::new() };
    let mut pos = auto_ascii_format::HEADER_SIZE as usize;
    while pos < end {
        let hdr_bytes: &[u8; CHUNK_HEADER_SIZE] = bytes
            .get(pos..pos + CHUNK_HEADER_SIZE)
            .and_then(|s| s.try_into().ok())
            .ok_or("truncated chunk header before FIDX")?;
        let ch = ChunkHeader::from_bytes(hdr_bytes);
        let size = usize::try_from(ch.size).map_err(|_| "chunk size exceeds file")?;
        let payload = bytes
            .get(pos + CHUNK_HEADER_SIZE..pos + CHUNK_HEADER_SIZE + size)
            .ok_or("truncated chunk payload")?;
        if ch.tag == TAG_FRAM {
            if payload.len() < 5 {
                return Err("FRAM payload shorter than its fixed header".into());
            }
            stats.frames += 1;
            if payload[4] & frame_flags::KEYFRAME != 0 {
                stats.keyframes += 1;
            }
            let mut off = 5usize;
            while off + 9 <= payload.len() {
                let id = payload[off];
                let comp =
                    u32::from_le_bytes(payload[off + 1..off + 5].try_into().unwrap()) as u64;
                let raw =
                    u32::from_le_bytes(payload[off + 5..off + 9].try_into().unwrap()) as u64;
                match stats.per_plane.iter_mut().find(|(pid, _, _)| *pid == id) {
                    Some((_, c, r)) => {
                        *c += comp;
                        *r += raw;
                    }
                    None => stats.per_plane.push((id, comp, raw)),
                }
                off += usize::try_from((9 + comp).div_ceil(64) * 64)
                    .map_err(|_| "subblock size overflow")?;
            }
        }
        pos += CHUNK_HEADER_SIZE + size + crc_len;
    }
    Ok(stats)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn sample_frames(frame_count: u32) -> Vec<u32> {
    let n = frame_count;
    let want = 4u32.min(n);
    let mut out = Vec::new();
    for k in 0..want {
        let f = if want <= 1 { 0 } else { k * (n - 1) / (want - 1) };
        if out.last() != Some(&f) {
            out.push(f);
        }
    }
    out
}

fn plane_stats(reader: &mut AsciiReader<'_>, frames: &[u32]) -> Result<Vec<PlaneStats>, BoxErr> {
    let h = reader.header().clone();
    let mut out = Vec::new();
    for &id in &h.plane_ids[..h.plane_count as usize] {
        let Some((pw, ph)) = reader.plane_dims(id) else { continue };
        let mut buf = vec![0u8; auto_ascii_format::plane_raw_size(h.base_w, h.base_h, id).unwrap()];
        let n = pw as usize * ph as usize;
        let (mut min, mut max, mut sum, mut nonzero) = (255u8, 0u8, 0u64, 0u64);
        let (mut bit0, mut bit1, mut dev_sum, mut dev_max) = (0u64, 0u64, 0u64, 0u8);
        for &f in frames {
            reader.seek_plane_into(f, id, &mut buf)?;
            for &v in &buf[..if id == plane_id::C { buf.len() } else { n }] {
                min = min.min(v);
                max = max.max(v);
                sum += u64::from(v);
                nonzero += u64::from(v != 0);
                bit0 += u64::from(v & 1 != 0);
                bit1 += u64::from(v & 2 != 0);
                let d = v.abs_diff(128);
                dev_sum += u64::from(d);
                dev_max = dev_max.max(d);
            }
        }
        let total = (frames.len() * if id == plane_id::C { buf.len() } else { n }) as u64;
        let pct = |c: u64| 100.0 * c as f64 / total.max(1) as f64;
        out.push(PlaneStats {
            plane: plane_name(id),
            min,
            max,
            mean: sum as f64 / total as f64,
            nonzero_pct: pct(nonzero),
            highlight_pct: pct(bit0),
            deep_shadow_pct: pct(bit1),
            dev_mean: dev_sum as f64 / total as f64,
            dev_max,
        });
    }
    Ok(out)
}

fn dump_planes(reader: &mut AsciiReader<'_>, dir: &Path, frames: &[u32]) -> Result<(), BoxErr> {
    std::fs::create_dir_all(dir)?;
    let h = reader.header().clone();
    for &f in frames {
        for &id in &h.plane_ids[..h.plane_count as usize] {
            let Some((pw, ph)) = reader.plane_dims(id) else { continue };
            let mut buf =
                vec![0u8; auto_ascii_format::plane_raw_size(h.base_w, h.base_h, id).unwrap()];
            reader.seek_plane_into(f, id, &mut buf)?;
            let name = plane_name(id).to_ascii_lowercase();
            if id == plane_id::C {
                let mut rgb = Vec::with_capacity(pw as usize * ph as usize * 3);
                for px in buf.chunks_exact(2) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    let (r, g, b) = ((v >> 11) as u8, ((v >> 5) & 0x3F) as u8, (v & 0x1F) as u8);
                    rgb.push((r << 3) | (r >> 2));
                    rgb.push((g << 2) | (g >> 4));
                    rgb.push((b << 3) | (b >> 2));
                }
                let path = dir.join(format!("f{f:05}-{name}.ppm"));
                std::fs::write(&path, [format!("P6\n{pw} {ph}\n255\n").as_bytes(), &rgb].concat())?;
            } else {
                if id == plane_id::H {
                    for v in &mut buf {
                        *v = match *v & 3 {
                            1 | 3 => 255,
                            2 => 90,
                            _ => 0,
                        };
                    }
                }
                let path = dir.join(format!("f{f:05}-{name}.pgm"));
                std::fs::write(&path, [format!("P5\n{pw} {ph}\n255\n").as_bytes(), &buf].concat())?;
            }
        }
    }
    Ok(())
}
