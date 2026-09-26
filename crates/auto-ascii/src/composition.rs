use std::path::{Path, PathBuf};

use auto_ascii_format::AsciiHeader;
use memmap2::Mmap;

use crate::error::Error;

pub const SCHEMA_VERSION: i64 = 1;

const FRAME_EPS: f64 = 1e-6;

fn snap(raw: f64) -> f64 {
    let n = raw.round();
    if (raw - n).abs() <= FRAME_EPS { n } else { raw }
}

fn frame_ceil(raw: f64) -> Option<u32> {
    let n = raw.ceil();
    (n.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&n)).then_some(n as u32)
}

struct Part {
    header: AsciiHeader,
    in_secs: f64,
    out_secs: f64,
    at_secs: Option<f64>,
}

impl Part {
    fn fps(&self) -> f64 {
        f64::from(self.header.fps_num) / f64::from(self.header.fps_den)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: String,
    pub path: PathBuf,
    pub in_secs: f64,
    pub out_secs: Option<f64>,
    pub at_secs: Option<f64>,
}

impl Clip {
    pub fn new(path: impl Into<PathBuf>) -> Clip {
        let path = path.into();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Clip { name, path, in_secs: 0.0, out_secs: None, at_secs: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipSpan {
    pub start_frame: u32,
    pub end_frame: u32,
    pub start_secs: f64,
    pub end_secs: f64,
    pub in_secs: f64,
    pub out_secs: f64,
    pub fps_num: u16,
    pub fps_den: u16,
    pub frame_count: u32,
    pub base_w: u16,
    pub base_h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    in_frame: u32,
    rate: Option<f64>,
    plane_ids: [u8; 8],
    plane_count: u8,
}

impl ClipSpan {
    pub fn fps(&self) -> f64 {
        f64::from(self.fps_num) / f64::from(self.fps_den)
    }

    pub fn len_frames(&self) -> u32 {
        self.end_frame - self.start_frame
    }

    pub fn len_secs(&self) -> f64 {
        self.end_secs - self.start_secs
    }

    pub fn source_secs(&self) -> f64 {
        f64::from(self.frame_count) / self.fps()
    }

    pub fn planes(&self) -> &[u8] {
        &self.plane_ids[..usize::from(self.plane_count)]
    }

    pub fn contains_frame(&self, frame: u32) -> bool {
        frame >= self.start_frame && frame < self.end_frame
    }

    fn local_frame(&self, frame: u32) -> u32 {
        let offset = frame - self.start_frame;
        let offset = match self.rate {
            None => offset,
            Some(rate) => snap(f64::from(offset) * rate).floor().max(0.0) as u32,
        };
        self.in_frame.saturating_add(offset).min(self.frame_count.saturating_sub(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start_frame: u32,
    pub end_frame: u32,
    pub start_secs: f64,
    pub end_secs: f64,
}

impl Span {
    pub fn len_frames(&self) -> u32 {
        self.end_frame - self.start_frame
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlap {
    pub span: Span,
    pub under: usize,
    pub over: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMark {
    Clear,
    Partial,
    Hidden,
}

fn merge_spans(ranges: impl Iterator<Item = (u32, u32)>) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = ranges.filter(|(s, e)| s < e).collect();
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
    pub clip_idx: usize,
    pub local_frame: u32,
}

#[derive(Clone, Debug)]
pub struct Composition {
    name: String,
    clips: Vec<Clip>,
    spans: Vec<ClipSpan>,
    resolved: bool,
    fps_num: u16,
    fps_den: u16,
    fps: f64,
    duration_secs: f64,
    frame_count: u32,
    aspect: f64,
    stitch: bool,
}

impl Composition {
    pub fn single(path: impl Into<PathBuf>) -> Composition {
        let clip = Clip::new(path);
        let mut comp = Composition::from_clips(clip.name.clone(), vec![clip]);
        comp.stitch = false;
        comp
    }

    pub fn from_clips(name: impl Into<String>, clips: Vec<Clip>) -> Composition {
        Composition {
            name: name.into(),
            clips,
            spans: Vec::new(),
            resolved: false,
            fps_num: 0,
            fps_den: 1,
            fps: 0.0,
            duration_secs: 0.0,
            frame_count: 0,
            aspect: 16.0 / 9.0,
            stitch: true,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn is_resolved(&self) -> bool {
        self.resolved
    }

    pub fn resolve(&mut self) -> Result<(), Error> {
        self.resolved = false;
        self.spans.clear();
        if self.clips.is_empty() {
            return Err(Error::Config(format!(
                "composition {:?} has no clips",
                self.name
            )));
        }

        let mut parts: Vec<Part> = Vec::with_capacity(self.clips.len());
        for (idx, clip) in self.clips.iter().enumerate() {
            let bad = |what: String| Error::Config(format!("clip {idx} ({}): {what}", clip.name));
            let header = read_clip(&clip.path).map_err(|e| match e {
                Error::Asset(msg) => bad(msg.to_owned()),
                other => other,
            })?;
            let fps = f64::from(header.fps_num) / f64::from(header.fps_den);
            let source_secs = f64::from(header.frame_count) / fps;
            let in_secs = clip.in_secs;
            let out_secs = clip.out_secs.unwrap_or(source_secs);
            if !in_secs.is_finite() || in_secs < 0.0 {
                return Err(bad(format!("in {in_secs} must be finite and >= 0")));
            }
            if !out_secs.is_finite() || out_secs <= in_secs {
                return Err(bad(format!("out {out_secs} must be greater than in {in_secs}")));
            }
            if out_secs * fps > f64::from(header.frame_count) + 0.5 {
                return Err(bad(format!(
                    "out {out_secs:.2}s is past the end of the asset ({source_secs:.2}s, \
                     {} frames @ {fps} fps)",
                    header.frame_count
                )));
            }
            let out_secs = out_secs.min(source_secs);
            if let Some(at) = clip.at_secs
                && !(at.is_finite() && at >= 0.0)
            {
                return Err(bad(format!("at {at} must be finite and >= 0")));
            }
            parts.push(Part { header, in_secs, out_secs, at_secs: clip.at_secs });
        }

        let fastest = parts
            .iter()
            .max_by(|a, b| a.fps().total_cmp(&b.fps()))
            .expect("clips are non-empty");
        self.fps_num = fastest.header.fps_num;
        self.fps_den = fastest.header.fps_den;
        self.fps = fastest.fps();
        let comp_fps = self.fps;

        let mut cursor: u32 = 0;
        for (idx, part) in parts.iter().enumerate() {
            let name = &self.clips[idx].name;
            let bad = |what: String| Error::Config(format!("clip {idx} ({name}): {what}"));
            let len_frames = frame_ceil(snap((part.out_secs - part.in_secs) * comp_fps))
                .ok_or_else(|| bad("is longer than a composition can hold".into()))?;
            let start_frame = match part.at_secs {
                Some(at) => frame_ceil(snap(at * comp_fps))
                    .ok_or_else(|| bad(format!("at {at}s is past what a timeline can hold")))?,
                None => cursor,
            };
            let end_frame = start_frame
                .checked_add(len_frames.max(1))
                .ok_or_else(|| bad("ends past what a timeline can hold".into()))?;
            cursor = end_frame;
            let clip_fps = part.fps();
            let header = &part.header;
            let (aspect_num, aspect_den) = if header.aspect_num == 0 || header.aspect_den == 0 {
                (16, 9)
            } else {
                (header.aspect_num, header.aspect_den)
            };
            self.spans.push(ClipSpan {
                start_frame,
                end_frame,
                start_secs: f64::from(start_frame) / comp_fps,
                end_secs: f64::from(end_frame) / comp_fps,
                in_secs: part.in_secs,
                out_secs: part.out_secs,
                fps_num: header.fps_num,
                fps_den: header.fps_den,
                frame_count: header.frame_count,
                base_w: header.base_w,
                base_h: header.base_h,
                aspect_num,
                aspect_den,
                in_frame: snap(part.in_secs * clip_fps).floor().max(0.0) as u32,
                rate: (u32::from(header.fps_num) * u32::from(self.fps_den)
                    != u32::from(self.fps_num) * u32::from(header.fps_den))
                .then(|| clip_fps / comp_fps),
                plane_ids: header.plane_ids,
                plane_count: header.plane_count.min(8),
            });
        }

        self.frame_count = self.spans.iter().map(|s| s.end_frame).max().unwrap_or(0);
        self.duration_secs = f64::from(self.frame_count) / comp_fps;
        let first = self.spans[0];
        self.aspect = f64::from(first.aspect_num) / f64::from(first.aspect_den);
        self.resolved = true;
        Ok(())
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    pub fn fps_ratio(&self) -> (u16, u16) {
        (self.fps_num, self.fps_den)
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_secs
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn aspect(&self) -> f64 {
        self.aspect
    }

    pub fn timeline(&self) -> &[ClipSpan] {
        &self.spans
    }

    pub fn locate(&self, t_secs: f64) -> Option<Located> {
        if !t_secs.is_finite() || t_secs < 0.0 || self.fps <= 0.0 {
            return None;
        }
        let frame = snap(t_secs * self.fps).floor();
        if !(0.0..=f64::from(u32::MAX)).contains(&frame) {
            return None;
        }
        self.locate_frame(frame as u32)
    }

    pub fn locate_frame(&self, frame_idx: u32) -> Option<Located> {
        self.spans
            .iter()
            .enumerate()
            .rev()
            .find(|(_, s)| s.contains_frame(frame_idx))
            .map(|(clip_idx, s)| Located { clip_idx, local_frame: s.local_frame(frame_idx) })
    }

    pub fn is_stitch(&self) -> bool {
        self.stitch
    }

    pub fn frame_at_secs(&self, secs: f64) -> Result<u32, Error> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(Error::Config(format!("seek must be finite and >= 0 (got {secs}s)")));
        }
        let frame = snap(secs * self.fps).floor();
        if !(0.0..f64::from(self.frame_count)).contains(&frame) {
            let what = if self.stitch { "composition" } else { "asset" };
            return Err(Error::Config(format!(
                "seek {secs}s is past the end of the {what} ({} frames @ {} fps)",
                self.frame_count, self.fps
            )));
        }
        Ok(frame as u32)
    }

    pub fn gaps(&self) -> Vec<Span> {
        let mut gaps = Vec::new();
        let mut cursor = 0u32;
        for span in merge_spans(self.spans.iter().map(|s| (s.start_frame, s.end_frame))) {
            if span.0 > cursor {
                gaps.push(self.span(cursor, span.0));
            }
            cursor = cursor.max(span.1);
        }
        if cursor < self.frame_count {
            gaps.push(self.span(cursor, self.frame_count));
        }
        gaps
    }

    pub fn overlaps(&self) -> Vec<Overlap> {
        let mut out = Vec::new();
        for (over, top) in self.spans.iter().enumerate() {
            for (under, below) in self.spans.iter().enumerate().take(over) {
                let start = top.start_frame.max(below.start_frame);
                let end = top.end_frame.min(below.end_frame);
                if start < end {
                    out.push(Overlap { span: self.span(start, end), under, over });
                }
            }
        }
        out.sort_by_key(|o| (o.span.start_frame, o.under, o.over));
        out
    }

    pub fn mark_for(&self, clip_idx: usize) -> Option<ClipMark> {
        let span = self.spans.get(clip_idx)?;
        let covered: u32 = merge_spans(
            self.spans
                .iter()
                .skip(clip_idx + 1)
                .filter_map(|later| {
                    let start = span.start_frame.max(later.start_frame);
                    let end = span.end_frame.min(later.end_frame);
                    (start < end).then_some((start, end))
                })
                .collect::<Vec<_>>()
                .into_iter(),
        )
        .iter()
        .map(|(start, end)| end - start)
        .sum();
        Some(match covered {
            0 => ClipMark::Clear,
            n if n >= span.len_frames() => ClipMark::Hidden,
            _ => ClipMark::Partial,
        })
    }

    fn span(&self, start_frame: u32, end_frame: u32) -> Span {
        Span {
            start_frame,
            end_frame,
            start_secs: f64::from(start_frame) / self.fps,
            end_secs: f64::from(end_frame) / self.fps,
        }
    }

    pub fn frame_after(&self, base_frame: u64, elapsed_secs: f64) -> u64 {
        base_frame + (elapsed_secs * self.fps) as u64
    }

    pub fn is_toml_path(path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    }

    pub fn default_library_dir() -> Option<PathBuf> {
        let root = match std::env::var_os("AUTO_ASCII_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => {
                let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
                PathBuf::from(home).join("auto-ascii")
            }
        };
        let library = root.join("library");
        library.is_dir().then_some(library)
    }
}

pub(crate) fn read_clip(path: &Path) -> Result<AsciiHeader, Error> {
    let file = std::fs::File::open(path)
        .map_err(|source| Error::Io { path: path.into(), source })?;
    let map = unsafe { Mmap::map(&file) }
        .map_err(|source| Error::Io { path: path.into(), source })?;
    let header = auto_ascii_format::AsciiReader::open(&map)
        .map_err(|source| Error::Format { path: path.into(), source })?
        .header()
        .clone();
    if header.fps_num == 0 || header.fps_den == 0 {
        return Err(Error::Asset("corrupt header: fps_num or fps_den == 0"));
    }
    if header.frame_count == 0 {
        return Err(Error::Asset("asset has zero frames"));
    }
    let planes = &header.plane_ids[..usize::from(header.plane_count).min(8)];
    if !planes.contains(&auto_ascii_format::header::plane_id::Y) {
        return Err(Error::Asset("asset has no Y (luma) plane"));
    }
    Ok(header)
}

#[cfg(feature = "compose")]
impl Composition {
    pub fn from_toml_str(
        text: &str,
        base_dir: &Path,
        library_dir: Option<&Path>,
    ) -> Result<Composition, Error> {
        let cfg = |msg: String| Error::Config(msg);
        let table: toml::Table =
            toml::from_str(text).map_err(|e| cfg(format!("not valid TOML: {e}")))?;
        for key in table.keys() {
            if !matches!(key.as_str(), "schema" | "name" | "clip") {
                return Err(cfg(format!(
                    "unknown key {key:?} (expected schema, name, clip)"
                )));
            }
        }
        match table.get("schema").and_then(toml::Value::as_integer) {
            Some(SCHEMA_VERSION) => {}
            Some(other) => {
                return Err(cfg(format!(
                    "schema {other} is not supported (this build reads schema \
                     {SCHEMA_VERSION})"
                )));
            }
            None => {
                return Err(cfg(format!(
                    "missing `schema = {SCHEMA_VERSION}` (every composition declares \
                     its schema)"
                )));
            }
        }
        let name = match table.get("name") {
            None => String::new(),
            Some(v) => v
                .as_str()
                .ok_or_else(|| cfg("name must be a string".into()))?
                .to_owned(),
        };
        let raw_clips: &[toml::Value] = match table.get("clip") {
            None => &[],
            Some(v) => v
                .as_array()
                .ok_or_else(|| cfg("clip must be an array of tables ([[clip]])".into()))?
                .as_slice(),
        };
        let mut clips = Vec::with_capacity(raw_clips.len());
        for (idx, raw) in raw_clips.iter().enumerate() {
            let bad = |msg: String| cfg(format!("clip {idx}: {msg}"));
            let entry = raw
                .as_table()
                .ok_or_else(|| bad("must be a table ([[clip]])".into()))?;
            for key in entry.keys() {
                if !matches!(key.as_str(), "asset" | "in" | "out" | "at") {
                    return Err(bad(format!(
                        "unknown key {key:?} (expected asset, in, out, at)"
                    )));
                }
            }
            let asset = entry
                .get("asset")
                .ok_or_else(|| bad("missing `asset`".into()))?
                .as_str()
                .ok_or_else(|| bad("asset must be a string".into()))?;
            let path = resolve_asset(asset, base_dir, library_dir).map_err(bad)?;
            clips.push(Clip {
                name: asset.to_owned(),
                path,
                in_secs: time_field(entry.get("in"), "in", idx)?.unwrap_or(0.0),
                out_secs: time_field(entry.get("out"), "out", idx)?,
                at_secs: time_field(entry.get("at"), "at", idx)?,
            });
        }
        Ok(Composition::from_clips(name, clips))
    }

    pub fn from_toml_file(
        path: impl AsRef<Path>,
        library_dir: Option<&Path>,
    ) -> Result<Composition, Error> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|source| Error::Io { path: path.into(), source })?;
        let base_dir = path.parent().unwrap_or(Path::new("."));
        let mut comp = Composition::from_toml_str(&text, base_dir, library_dir).map_err(|e| {
            match e {
                Error::Config(msg) => Error::Config(format!("{}: {msg}", path.display())),
                other => other,
            }
        })?;
        if comp.name.is_empty() {
            comp.name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        Ok(comp)
    }
}

#[cfg(feature = "compose")]
fn time_field(value: Option<&toml::Value>, field: &str, idx: usize) -> Result<Option<f64>, Error> {
    let bad = |msg: String| Error::Config(format!("clip {idx}: {field} {msg}"));
    match value {
        None => Ok(None),
        Some(toml::Value::String(s)) => crate::timecode::parse(s)
            .map(Some)
            .map_err(|e| bad(format!("{s:?}: {e}"))),
        Some(toml::Value::Integer(n)) => Ok(Some(*n as f64)),
        Some(toml::Value::Float(f)) => Ok(Some(*f)),
        Some(other) => Err(bad(format!(
            "must be a timestamp string or a number of seconds (got a {})",
            other.type_str()
        ))),
    }
}

#[cfg(feature = "compose")]
fn resolve_asset(
    asset: &str,
    base_dir: &Path,
    library_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let direct = base_dir.join(asset);
    if direct.is_file() {
        return Ok(direct);
    }
    if let Some(lib) = library_dir {
        let in_library = lib.join(format!("{asset}.ascii"));
        if in_library.is_file() {
            return Ok(in_library);
        }
        return Err(format!(
            "asset {asset:?} is neither {} nor {}",
            direct.display(),
            in_library.display()
        ));
    }
    Err(format!(
        "asset {asset:?} is not {} (and there is no library folder to look in — \
         `auto-ascii home` creates one)",
        direct.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_eval::fixtures::{FIXTURE_FRAMES, Fixture, build_fixture};

    struct Assets(PathBuf);

    impl Assets {
        fn new(tag: &str) -> Assets {
            let dir = std::env::temp_dir()
                .join(format!("auto-ascii-comp-{}-{tag}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            for f in [Fixture::GradientMotion, Fixture::HardCut] {
                std::fs::write(dir.join(format!("{}.ascii", f.name())), build_fixture(f))
                    .expect("write fixture");
            }
            Assets(dir)
        }

        fn path(&self, fixture: Fixture) -> PathBuf {
            self.0.join(format!("{}.ascii", fixture.name()))
        }

        fn clip(&self, fixture: Fixture) -> Clip {
            Clip::new(self.path(fixture))
        }
    }

    impl Drop for Assets {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const FIXTURE_SECS: f64 = 2.4;

    fn resolved(name: &str, clips: Vec<Clip>) -> Composition {
        let mut comp = Composition::from_clips(name, clips);
        comp.resolve().expect("fixtures resolve");
        comp
    }

    #[test]
    fn single_asset_is_a_one_clip_composition() {
        let a = Assets::new("single");
        let mut comp = Composition::single(a.path(Fixture::GradientMotion));
        assert!(!comp.is_resolved());
        assert_eq!(comp.locate(0.0), None, "an unresolved composition locates nothing");
        comp.resolve().unwrap();
        assert_eq!(comp.frame_count(), FIXTURE_FRAMES, "no frames gained or lost");
        assert!((comp.fps() - 30.0).abs() < 1e-9);
        assert!((comp.duration_secs() - FIXTURE_SECS).abs() < 1e-9);
        assert!((comp.aspect() - 16.0 / 9.0).abs() < 1e-9);
        for f in 0..FIXTURE_FRAMES {
            assert_eq!(
                comp.locate_frame(f),
                Some(Located { clip_idx: 0, local_frame: f }),
                "frame {f} must map to itself"
            );
        }
        assert_eq!(comp.locate_frame(FIXTURE_FRAMES), None, "the end is exclusive");
    }

    #[test]
    fn clips_default_to_sequential_placement() {
        let a = Assets::new("sequential");
        let comp = resolved(
            "seq",
            vec![a.clip(Fixture::GradientMotion), a.clip(Fixture::HardCut)],
        );
        let spans = comp.timeline();
        assert_eq!(spans.len(), 2);
        assert!((spans[0].start_secs - 0.0).abs() < 1e-9);
        assert!((spans[0].end_secs - FIXTURE_SECS).abs() < 1e-9);
        assert!((spans[1].start_secs - FIXTURE_SECS).abs() < 1e-9);
        assert!((comp.duration_secs() - 2.0 * FIXTURE_SECS).abs() < 1e-9);
        assert_eq!(comp.frame_count(), 2 * FIXTURE_FRAMES);
        assert_eq!(
            comp.locate_frame(FIXTURE_FRAMES - 1),
            Some(Located { clip_idx: 0, local_frame: FIXTURE_FRAMES - 1 })
        );
        assert_eq!(
            comp.locate_frame(FIXTURE_FRAMES),
            Some(Located { clip_idx: 1, local_frame: 0 })
        );
    }

    #[test]
    fn at_places_and_leaves_a_gap() {
        let a = Assets::new("gap");
        let mut second = a.clip(Fixture::HardCut);
        second.at_secs = Some(4.0);
        let comp = resolved("gap", vec![a.clip(Fixture::GradientMotion), second]);
        assert!((comp.duration_secs() - (4.0 + FIXTURE_SECS)).abs() < 1e-9);
        assert_eq!(comp.locate(2.39).map(|l| l.clip_idx), Some(0));
        assert_eq!(comp.locate(2.4), None, "the gap starts where clip 0 ends");
        assert_eq!(comp.locate(3.999), None, "still the gap");
        assert_eq!(comp.locate(4.0), Some(Located { clip_idx: 1, local_frame: 0 }));
        assert_eq!(comp.locate(-1.0), None, "before the timeline");
        assert_eq!(comp.locate(6.4), None, "the composition end is exclusive");
    }

    #[test]
    fn a_boundary_frame_belongs_to_exactly_one_clip() {
        let a = Assets::new("boundary");
        let mut first = a.clip(Fixture::GradientMotion);
        first.in_secs = 0.1;
        first.out_secs = Some(0.4);
        let comp = resolved("boundary", vec![first, a.clip(Fixture::HardCut)]);

        assert_eq!(comp.timeline()[0].len_frames(), 9);
        assert_eq!(comp.locate_frame(8), Some(Located { clip_idx: 0, local_frame: 11 }));
        assert_eq!(
            comp.locate_frame(9),
            Some(Located { clip_idx: 1, local_frame: 0 }),
            "the frame at 0.3 s is the SECOND clip's first frame"
        );
        assert_eq!(comp.timeline()[0].end_frame, comp.timeline()[1].start_frame);
        assert_eq!(comp.timeline()[0].end_secs, comp.timeline()[1].start_secs);
    }

    #[test]
    fn one_decimal_placements_never_lose_a_frame() {
        let a = Assets::new("sweep");
        for tenth in 0..10 {
            let in_secs = f64::from(tenth) / 10.0;
            let mut first = a.clip(Fixture::GradientMotion);
            first.in_secs = in_secs;
            first.out_secs = Some(in_secs + 0.3);
            let comp = resolved("sweep", vec![first, a.clip(Fixture::HardCut)]);

            let boundary = comp.timeline()[0].end_frame;
            assert_eq!(boundary, 9, "in {in_secs}: a 0.3 s slice is 9 frames");
            let last_of_first = comp.locate_frame(boundary - 1).expect("owned");
            assert_eq!(last_of_first.clip_idx, 0, "in {in_secs}");
            assert!(
                f64::from(last_of_first.local_frame) < (in_secs + 0.3) * 30.0,
                "in {in_secs}: frame {} is past the trim",
                last_of_first.local_frame
            );
            assert_eq!(
                comp.locate_frame(boundary),
                Some(Located { clip_idx: 1, local_frame: 0 }),
                "in {in_secs}: the second clip must show its first frame"
            );
            assert!(
                (0..comp.frame_count()).all(|f| comp.locate_frame(f).is_some()),
                "in {in_secs}: a sequential composition has no gaps"
            );
        }
    }

    #[test]
    fn abutting_clips_have_no_phantom_gap_or_overlap() {
        let a = Assets::new("abut");
        for (in_secs, out_secs, at) in [(0.7, 1.0, 0.3), (0.1, 0.3, 0.2)] {
            let mut first = a.clip(Fixture::GradientMotion);
            first.in_secs = in_secs;
            first.out_secs = Some(out_secs);
            let mut second = a.clip(Fixture::HardCut);
            second.at_secs = Some(at);
            let comp = resolved("abut", vec![first, second]);

            let label = format!("in {in_secs} out {out_secs} at {at}");
            assert_eq!(comp.timeline()[0].end_frame, comp.timeline()[1].start_frame, "{label}");
            assert!(comp.gaps().is_empty(), "{label}: phantom gap {:?}", comp.gaps());
            assert!(comp.overlaps().is_empty(), "{label}: phantom overlap");
            assert_eq!(comp.mark_for(0), Some(ClipMark::Clear), "{label}");
            assert_eq!(comp.mark_for(1), Some(ClipMark::Clear), "{label}");
        }
    }

    #[test]
    fn gaps_and_overlaps_are_reported_on_the_frame_grid() {
        let a = Assets::new("marks");
        let mut hidden = a.clip(Fixture::HardCut);
        hidden.at_secs = Some(1.0);
        hidden.out_secs = Some(0.5);
        let mut covering = a.clip(Fixture::GradientMotion);
        covering.at_secs = Some(0.5);
        let mut after = a.clip(Fixture::HardCut);
        after.at_secs = Some(3.9);
        let comp =
            resolved("marks", vec![a.clip(Fixture::GradientMotion), hidden, covering, after]);

        let gaps = comp.gaps();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!((gaps[0].start_frame, gaps[0].end_frame), (87, 117));
        assert!((gaps[0].start_secs - 2.9).abs() < 1e-9);
        assert_eq!(gaps[0].len_frames(), 30);

        let overlaps = comp.overlaps();
        assert_eq!(
            overlaps.iter().map(|o| (o.under, o.over)).collect::<Vec<_>>(),
            vec![(0, 2), (0, 1), (1, 2)]
        );
        assert_eq!((overlaps[0].span.start_frame, overlaps[0].span.end_frame), (15, 72));
        assert_eq!((overlaps[1].span.start_frame, overlaps[1].span.end_frame), (30, 45));

        assert_eq!(comp.mark_for(0), Some(ClipMark::Partial), "A shows either side");
        assert_eq!(comp.mark_for(1), Some(ClipMark::Hidden), "B never reaches the screen");
        assert_eq!(comp.mark_for(2), Some(ClipMark::Clear));
        assert_eq!(comp.mark_for(3), Some(ClipMark::Clear));
        assert_eq!(comp.mark_for(4), None, "no such clip");
        assert!(
            (0..comp.frame_count()).all(|f| comp.locate_frame(f).map(|l| l.clip_idx) != Some(1)),
            "a HIDDEN clip must never be located"
        );
    }

    #[test]
    fn frame_at_secs_is_the_one_seek_bound() {
        let a = Assets::new("seekbound");
        let mut asset = Composition::single(a.path(Fixture::GradientMotion));
        asset.resolve().unwrap();
        assert_eq!(asset.frame_at_secs(0.0).unwrap(), 0);
        assert_eq!(asset.frame_at_secs(1.0).unwrap(), 30);
        assert_eq!(asset.frame_at_secs(2.4 - 1.0 / 30.0).unwrap(), 71);
        let e = asset.frame_at_secs(2.4).unwrap_err().to_string();
        assert!(e.contains("past the end of the asset"), "{e}");
        assert!(asset.frame_at_secs(-1.0).is_err() && asset.frame_at_secs(f64::NAN).is_err());

        let mut second = a.clip(Fixture::HardCut);
        second.at_secs = Some(4.0);
        let stitch = resolved("stitch", vec![a.clip(Fixture::GradientMotion), second]);
        assert_eq!(stitch.frame_at_secs(4.0).unwrap(), 120);
        let e = stitch.frame_at_secs(99.0).unwrap_err().to_string();
        assert!(e.contains("past the end of the composition"), "{e}");
    }

    #[test]
    fn out_accepts_the_duration_the_tools_print() {
        let a = Assets::new("outslack");
        let odd = a.0.join("odd.ascii");
        let mut bytes = build_fixture(Fixture::GradientMotion);
        bytes[16..18].copy_from_slice(&7u16.to_le_bytes());
        std::fs::write(&odd, bytes).unwrap();

        let mut clip = Clip::new(&odd);
        clip.out_secs = Some(10.29);
        let comp = resolved("slack", vec![clip]);
        assert_eq!(comp.frame_count(), 72, "clamped to the asset end, not a frame past it");

        let mut past = Clip::new(&odd);
        past.out_secs = Some(10.5);
        let mut comp = Composition::from_clips("past", vec![past]);
        let e = comp.resolve().unwrap_err().to_string();
        assert!(e.contains("10.50s is past the end"), "{e}");
        assert!(e.contains("10.29s"), "the message prints what the tools print: {e}");
    }

    #[test]
    fn overlap_puts_the_later_clip_on_top() {
        let a = Assets::new("overlap");
        let mut second = a.clip(Fixture::HardCut);
        second.at_secs = Some(1.0);
        let comp = resolved("overlap", vec![a.clip(Fixture::GradientMotion), second]);
        assert_eq!(comp.locate(0.5).map(|l| l.clip_idx), Some(0));
        let overlapped = comp.locate(1.5).expect("inside both clips");
        assert_eq!(overlapped.clip_idx, 1, "the later-listed clip is on top");
        assert_eq!(overlapped.local_frame, 15, "0.5 s into clip 1 at 30 fps");
        assert!((comp.duration_secs() - (1.0 + FIXTURE_SECS)).abs() < 1e-9);
    }

    #[test]
    fn in_and_out_trim_inside_the_asset() {
        let a = Assets::new("trim");
        let mut clip = a.clip(Fixture::GradientMotion);
        clip.in_secs = 0.5;
        clip.out_secs = Some(1.5);
        let comp = resolved("trim", vec![clip]);
        assert!((comp.duration_secs() - 1.0).abs() < 1e-9, "a 1 s slice");
        assert_eq!(comp.frame_count(), 30);
        assert_eq!(comp.locate_frame(0), Some(Located { clip_idx: 0, local_frame: 15 }));
        assert_eq!(comp.locate_frame(29), Some(Located { clip_idx: 0, local_frame: 44 }));
        assert_eq!(comp.locate_frame(30), None, "trimmed end, exclusive");
    }

    #[test]
    fn mixed_fps_takes_the_highest() {
        let a = Assets::new("mixedfps");
        let slow = a.0.join("slow.ascii");
        let mut bytes = build_fixture(Fixture::HardCut);
        bytes[16..18].copy_from_slice(&15u16.to_le_bytes());
        std::fs::write(&slow, bytes).unwrap();

        let comp = resolved("mixed", vec![Clip::new(&slow), a.clip(Fixture::GradientMotion)]);
        assert!((comp.fps() - 30.0).abs() < 1e-9, "the composition runs at the fastest clip");
        assert_eq!(comp.fps_ratio(), (30, 1));
        assert!((comp.duration_secs() - 7.2).abs() < 1e-9);
        assert_eq!(comp.frame_count(), 216);
        assert_eq!(comp.locate_frame(30), Some(Located { clip_idx: 0, local_frame: 15 }));
    }

    #[test]
    fn empty_and_bad_trims_are_rejected_by_clip_index() {
        let a = Assets::new("reject");
        let mut empty = Composition::from_clips("empty", Vec::new());
        let e = empty.resolve().unwrap_err();
        assert!(e.to_string().contains("no clips"), "{e}");

        let mut past = a.clip(Fixture::GradientMotion);
        past.out_secs = Some(99.0);
        let mut comp = Composition::from_clips("past", vec![a.clip(Fixture::HardCut), past]);
        let e = comp.resolve().unwrap_err();
        assert!(e.to_string().contains("clip 1"), "names the clip: {e}");
        assert!(e.to_string().contains("past the end"), "{e}");

        let mut inverted = a.clip(Fixture::GradientMotion);
        inverted.in_secs = 2.0;
        inverted.out_secs = Some(1.0);
        let mut comp = Composition::from_clips("inverted", vec![inverted]);
        let e = comp.resolve().unwrap_err();
        assert!(e.to_string().contains("clip 0"), "names the clip: {e}");
        assert!(e.to_string().contains("greater than in"), "{e}");
    }

    #[test]
    fn frame_after_is_the_pre_m8_expression() {
        let a = Assets::new("frameafter");
        let comp = resolved("fa", vec![a.clip(Fixture::GradientMotion)]);
        assert_eq!(comp.frame_after(0, 0.0), 0);
        assert_eq!(comp.frame_after(10, 0.5), 25);
        assert_eq!(comp.frame_after(0, 2.4), 72);
    }

    #[cfg(feature = "compose")]
    mod toml {
        use super::*;

        fn write(dir: &Path, text: &str) -> PathBuf {
            let path = dir.join("comp.toml");
            std::fs::write(&path, text).unwrap();
            path
        }

        #[test]
        fn parses_the_schema_and_resolves_paths() {
            let a = Assets::new("toml-ok");
            let path = write(
                &a.0,
                "schema = 1\nname = \"demo\"\n\
                 [[clip]]\nasset = \"gradient-motion.ascii\"\nin = \"0:00.5\"\n\
                 out = 1.5\n\
                 [[clip]]\nasset = \"hard-cut.ascii\"\nat = \"0:04\"\n",
            );
            let mut comp = Composition::from_toml_file(&path, None).expect("parses");
            assert_eq!(comp.name(), "demo");
            assert_eq!(comp.clips().len(), 2);
            assert_eq!(comp.clips()[0].in_secs, 0.5);
            assert_eq!(comp.clips()[0].out_secs, Some(1.5));
            assert_eq!(comp.clips()[1].at_secs, Some(4.0));
            comp.resolve().expect("resolves against the file's folder");
            assert!((comp.duration_secs() - 6.4).abs() < 1e-9);
        }

        #[test]
        fn a_bare_name_resolves_in_the_library() {
            let a = Assets::new("toml-lib");
            let elsewhere = std::env::temp_dir()
                .join(format!("auto-ascii-comp-{}-toml-lib-home", std::process::id()));
            std::fs::create_dir_all(&elsewhere).unwrap();
            let path = write(&elsewhere, "schema = 1\n[[clip]]\nasset = \"hard-cut\"\n");
            let comp = Composition::from_toml_file(&path, Some(&a.0)).expect("library lookup");
            assert_eq!(comp.clips()[0].path, a.path(Fixture::HardCut));
            assert_eq!(comp.name(), "comp", "the file stem is the default name");
            let _ = std::fs::remove_dir_all(&elsewhere);
        }

        #[test]
        fn errors_name_the_clip() {
            let a = Assets::new("toml-bad");
            let cases = [
                ("schema = 2\n[[clip]]\nasset = \"hard-cut.ascii\"\n", "schema 2"),
                ("[[clip]]\nasset = \"hard-cut.ascii\"\n", "missing `schema"),
                (
                    "schema = 1\n[[clip]]\nasset = \"hard-cut.ascii\"\nstart = \"0:01\"\n",
                    "clip 0: unknown key \"start\"",
                ),
                ("schema = 1\nfps = 30\n[[clip]]\nasset = \"x\"\n", "unknown key \"fps\""),
                ("schema = 1\n[[clip]]\nin = \"0:01\"\n", "clip 0: missing `asset`"),
                (
                    "schema = 1\n[[clip]]\nasset = \"nope\"\n",
                    "clip 0: asset \"nope\" is neither",
                ),
                (
                    "schema = 1\n[[clip]]\nasset = \"hard-cut.ascii\"\nin = \"1:2:3:4\"\n",
                    "clip 0: in \"1:2:3:4\"",
                ),
                (
                    "schema = 1\n[[clip]]\nasset = \"hard-cut.ascii\"\nat = true\n",
                    "clip 0: at must be a timestamp string",
                ),
            ];
            for (text, want) in cases {
                let path = write(&a.0, text);
                let e = Composition::from_toml_file(&path, Some(&a.0))
                    .expect_err("must be rejected");
                let msg = e.to_string();
                assert!(msg.contains(want), "{msg:?} should contain {want:?}");
                assert!(msg.contains("comp.toml"), "the file is named: {msg:?}");
            }
        }
    }
}
