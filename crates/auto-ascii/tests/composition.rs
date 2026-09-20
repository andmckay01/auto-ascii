//! M8 (PLAN-M6-M8 §3): compositions end to end — the timeline a
//! [`RenderSession`] plays, the gaps between clips, the flattened export,
//! and the `.toml` the whole thing is written in.
//!
//! Two deterministic fixture assets stand in for library clips
//! (GradientMotion and HardCut are visually distinct, and HardCut carries a
//! NORM cut in the middle), stitched with a trim and an explicit `at` that
//! leaves a gap. The proofs are all "same bytes as the obvious single-asset
//! answer": a frame from a clip must equal what a fresh session on that
//! clip alone renders for the corresponding local frame.

// Every test here starts from a composition FILE, so the whole file needs
// the `compose` feature — the same whole-file gate tests/sim_e2e.rs uses
// for `bin`. Without it a `--no-default-features` build would fail to
// compile a test binary rather than simply having nothing to run.
#![cfg(feature = "compose")]

use std::fs;
use std::path::{Path, PathBuf};

use auto_ascii::compose::{ExportOptions, export};
use auto_ascii::{Cell, Composition, Grid, RenderSession, Rgb};
use auto_ascii_eval::fixtures::{FIXTURE_FRAMES, Fixture, build_fixture};
use auto_ascii_format::{AsciiReader, norm_flags};

/// Self-cleaning temp folder holding the clips and the composition file (no
/// tempfile dep — pinned workspace dep set).
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> TmpDir {
        let dir = std::env::temp_dir()
            .join(format!("auto-ascii-composition-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        for fixture in [Fixture::GradientMotion, Fixture::HardCut] {
            fs::write(dir.join(format!("{}.ascii", fixture.name())), build_fixture(fixture))
                .expect("write fixture clip");
        }
        TmpDir(dir)
    }

    fn clip(&self, fixture: Fixture) -> PathBuf {
        self.0.join(format!("{}.ascii", fixture.name()))
    }

    /// The §3 example schema over the two fixtures: clip A whole from 0,
    /// then a 1 s slice of clip B placed at 4 s — which leaves a 1.6 s gap.
    fn composition_file(&self) -> PathBuf {
        let path = self.0.join("demo.toml");
        fs::write(
            &path,
            "schema = 1\n\
             name = \"demo\"\n\
             \n\
             [[clip]]\n\
             asset = \"gradient-motion.ascii\"\n\
             \n\
             [[clip]]\n\
             asset = \"hard-cut.ascii\"\n\
             in = \"0:00.5\"\n\
             out = \"0:01.5\"\n\
             at = \"0:04\"\n",
        )
        .expect("write composition");
        path
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// The demo timeline at 30 fps: clip A owns frames 0..71, the gap 72..119,
// the 1 s slice of clip B 120..149 (its own frames 15..44).
const GRID: (u16, u16) = (120, 40);
const GAP_START: u32 = FIXTURE_FRAMES; // 72
const B_START: u32 = 120;
const B_IN_FRAME: u32 = 15; // in = 0:00.5 at 30 fps
const TOTAL: u32 = 150;

fn cells(grid: &Grid<Cell>) -> Vec<Cell> {
    grid.as_slice().to_vec()
}

fn open_demo(dir: &TmpDir) -> RenderSession {
    RenderSession::open_composition(dir.composition_file(), None).expect("open composition")
}

#[test]
fn the_timeline_is_the_files_timeline() {
    let dir = TmpDir::new("timeline");
    let session = open_demo(&dir);
    assert_eq!(session.frame_count(), TOTAL, "2.4 s + 1.6 s gap + 1 s slice at 30 fps");
    assert!((session.fps() - 30.0).abs() < 1e-9);
    assert!((session.aspect() - 16.0 / 9.0).abs() < 1e-9);
}

/// Every frame that belongs to a clip is that clip's own frame, byte for
/// byte — including the first frame after the gap, which the composition
/// renders on a decoder it has only just built.
#[test]
fn clip_frames_match_the_clip_played_alone() {
    let dir = TmpDir::new("boundary");
    let mut comp = open_demo(&dir);

    // Walk the whole timeline once, keeping the frames on either side of
    // both boundaries.
    let mut last_of_a = Vec::new();
    let (mut first_of_b, mut second_of_b) = (Vec::new(), Vec::new());
    for frame in 0..TOTAL {
        let grid = comp.render(frame, GRID.0, GRID.1).expect("render");
        match frame {
            f if f == GAP_START - 1 => last_of_a = cells(grid),
            f if f == B_START => first_of_b = cells(grid),
            f if f == B_START + 1 => second_of_b = cells(grid),
            _ => {}
        }
    }

    // Clip A alone, rolled the same way: same warm temporal state, so the
    // last frame before the gap must be identical.
    let mut solo_a = RenderSession::open(dir.clip(Fixture::GradientMotion)).unwrap();
    let mut a_last = Vec::new();
    for frame in 0..FIXTURE_FRAMES {
        a_last = cells(solo_a.render(frame, GRID.0, GRID.1).unwrap());
    }
    assert_eq!(last_of_a, a_last, "the frame before the gap is clip A's last frame");

    // Clip B alone, started cold at the trimmed-in frame — which is exactly
    // what the composition does when the gap ends.
    let mut solo_b = RenderSession::open(dir.clip(Fixture::HardCut)).unwrap();
    let b_first = cells(solo_b.render(B_IN_FRAME, GRID.0, GRID.1).unwrap());
    let b_second = cells(solo_b.render(B_IN_FRAME + 1, GRID.0, GRID.1).unwrap());
    assert_eq!(first_of_b, b_first, "the first frame after the gap is clip B's `in` frame");
    assert_eq!(second_of_b, b_second, "and it rolls on sequentially from there");
    assert_ne!(first_of_b, last_of_a, "the two clips are visually distinct");
}

#[test]
fn gap_frames_are_blank() {
    let dir = TmpDir::new("gap");
    let mut comp = open_demo(&dir);
    // Straddle the gap so the blank frames are reached from a live clip.
    for frame in [GAP_START - 1, GAP_START, GAP_START + 20, B_START - 1, B_START] {
        let grid = comp.render(frame, GRID.0, GRID.1).expect("render");
        let blank = grid.as_slice().iter().all(|c| *c == Cell::BLANK);
        let in_gap = (GAP_START..B_START).contains(&frame);
        assert_eq!(blank, in_gap, "frame {frame}: blank should be {in_gap}");
    }
}

/// A jump backwards across a clip boundary lands cold, exactly like the
/// single-asset contract: the clip is re-fronted and its temporal state
/// reset, so the frame is what a fresh session renders.
#[test]
fn backward_jump_across_clips_lands_cold() {
    let dir = TmpDir::new("backward");
    let mut comp = open_demo(&dir);
    for frame in [0u32, 30, B_START, B_START + 5] {
        comp.render(frame, GRID.0, GRID.1).unwrap();
    }
    let jumped = cells(comp.render(10, GRID.0, GRID.1).unwrap());

    let mut cold = open_demo(&dir);
    let cold_cells = cells(cold.render(10, GRID.0, GRID.1).unwrap());
    assert_eq!(jumped, cold_cells, "post-jump frame == cold-start frame, cell for cell");
}

/// The palette/font/cell-aspect knobs reach every clip — including one
/// whose decode pipeline does not exist yet when the knob is turned.
#[test]
fn palette_knobs_reach_every_clip() {
    let dir = TmpDir::new("palette");
    let mut comp = open_demo(&dir);
    let unicode = cells(comp.render(0, GRID.0, GRID.1).unwrap());
    assert!(
        unicode.iter().any(|c| !c.glyph().is_ascii()),
        "the default session is the unicode-blocks tier"
    );
    // Clip B has never been fronted at this point.
    comp.set_palette(auto_ascii::PaletteChoice::Ascii);
    for frame in [0u32, B_START] {
        let grid = comp.render(frame, GRID.0, GRID.1).unwrap();
        assert!(
            grid.as_slice().iter().all(|c| c.glyph().is_ascii()),
            "frame {frame} must honour the ascii palette"
        );
    }
    // And a cell-aspect change lands on both too (square cells letterbox
    // 16:9 content to a narrower viewport, so the left column goes blank).
    comp.set_cell_aspect(1.0).unwrap();
    for frame in [0u32, B_START] {
        let grid = comp.render(frame, GRID.0, GRID.1).unwrap();
        assert_eq!(grid.get(0, GRID.1 / 2), Cell::BLANK, "frame {frame} reflowed");
    }
}

/// Export flattens the timeline into one asset: same frame count, same
/// pictures, and a NORM table that describes what actually happened —
/// one record per (clip slice ∩ source shot) plus the gap, cut-flagged at
/// every boundary.
#[test]
fn export_flattens_the_timeline() {
    let dir = TmpDir::new("export");
    let mut comp = Composition::from_toml_file(dir.composition_file(), None).unwrap();
    comp.resolve().unwrap();
    let out = dir.0.join("flat.ascii");
    let report = export(&comp, &out, &ExportOptions::default()).expect("export");

    assert_eq!(report.frames, TOTAL);
    assert!((report.fps - 30.0).abs() < 1e-9);
    assert!(report.bytes > 0);
    // Clip A (one shot) | the gap | clip B's first shot | clip B's cut.
    assert_eq!(report.shots, 4, "one record per clip slice ∩ source shot, plus the gap");
    assert_eq!(report.cuts, 3, "the gap edge, the clip edge and clip B's own cut");

    let bytes = fs::read(&out).unwrap();
    let reader = AsciiReader::open(&bytes).expect("the export reopens");
    reader.verify().expect("every chunk CRC checks out");
    let shots = reader.shots();
    assert_eq!(shots.len() as u32, report.shots, "the report counts what was written");
    assert_eq!(
        shots.iter().filter(|s| s.is_cut()).count() as u32,
        report.cuts,
        "and how many of them cut"
    );
    let firsts: Vec<u32> = shots.iter().map(|s| s.first_frame).collect();
    // Clip B's own cut is at its frame 36 — 21 frames past the `in` frame.
    assert_eq!(firsts, vec![0, GAP_START, B_START, B_START + (36 - B_IN_FRAME)]);
    let flags: Vec<bool> = shots.iter().map(|s| s.flags & norm_flags::CUT != 0).collect();
    assert_eq!(flags, vec![false, true, true, true], "record 0 has nothing to cut from");

    // The pictures survive the round trip: the flattened asset renders each
    // clip frame exactly as the composition does.
    let mut flat = RenderSession::open(&out).unwrap();
    assert_eq!(flat.frame_count(), TOTAL);
    let mut comp_session = open_demo(&dir);
    for frame in [0u32, 10, GAP_START - 1, B_START, TOTAL - 1] {
        let want = cells(comp_session.render(frame, GRID.0, GRID.1).unwrap());
        let got = cells(flat.render(frame, GRID.0, GRID.1).unwrap());
        assert_eq!(got, want, "flattened frame {frame} must match the composition");
    }
    // Gap frames were written as black planes, so they render with no ink
    // (the composition's own gap grid is literally blank cells).
    let gap = flat.render(GAP_START + 5, GRID.0, GRID.1).unwrap();
    // Every cell is blank: no glyph anywhere, and nothing lit — a picture
    // cell composed from black planes is black on black, and the letterbox
    // pads are `Cell::BLANK` itself.
    let inked = gap
        .as_slice()
        .iter()
        .find(|c| c.glyph() != ' ' || (c.fg != Rgb::BLACK && **c != Cell::BLANK));
    assert!(inked.is_none(), "a flattened gap frame carries no picture: {inked:?}");
}

/// An overlap puts the later-listed clip on top — and when it ends, the
/// first clip is on top again. That return is the interesting case: the
/// deck re-fronts a clip it left (reset + repaint) and the export seeks its
/// reader forward over the frames the window covered.
#[test]
fn overlapping_clips_put_the_later_one_on_top() {
    let dir = TmpDir::new("overlap");
    let mut window = auto_ascii::Clip::new(dir.clip(Fixture::HardCut));
    window.out_secs = Some(1.0);
    window.at_secs = Some(0.7); // a 1 s window inside clip A's 2.4 s
    let mut comp = Composition::from_clips(
        "overlap",
        vec![auto_ascii::Clip::new(dir.clip(Fixture::GradientMotion)), window],
    );
    comp.resolve().unwrap();
    // A owns [0, 0.7) and [1.7, 2.4); B is on top in between.
    assert_eq!(comp.frame_count(), FIXTURE_FRAMES);
    let on_top = |f: u32| comp.locate_frame(f).map(|l| (l.clip_idx, l.local_frame));
    assert_eq!(on_top(20), Some((0, 20)));
    assert_eq!(on_top(21), Some((1, 0)), "the later clip takes over at 0.7 s");
    assert_eq!(on_top(50), Some((1, 29)));
    assert_eq!(on_top(51), Some((0, 51)), "and hands back when its window ends");

    let mut session = RenderSession::from_composition(comp.clone()).unwrap();
    let mut played = Vec::new();
    for frame in 0..FIXTURE_FRAMES {
        played.push(cells(session.render(frame, GRID.0, GRID.1).unwrap()));
    }
    // Clip A after the window: re-fronted cold, so it is what a fresh
    // session renders at that frame — not what warm state would have shown.
    let mut solo_a = RenderSession::open(dir.clip(Fixture::GradientMotion)).unwrap();
    assert_eq!(played[51], cells(solo_a.render(51, GRID.0, GRID.1).unwrap()));
    let mut solo_b = RenderSession::open(dir.clip(Fixture::HardCut)).unwrap();
    assert_eq!(played[21], cells(solo_b.render(0, GRID.0, GRID.1).unwrap()));
    assert_ne!(played[50], played[51], "the handover is visible");

    // The export walks the same three segments — clip A's reader has to
    // seek forward over the window when it comes back.
    let out = dir.0.join("overlap.ascii");
    let report = export(&comp, &out, &ExportOptions::default()).expect("export");
    assert_eq!(report.frames, FIXTURE_FRAMES);
    assert_eq!(report.shots, 3, "A | B | A");
    assert_eq!(report.cuts, 2, "both handovers cut");
    // No gaps here, so the flattened asset must reproduce the composition
    // frame for frame over the whole walk — including the two handovers,
    // where its NORM cut records reset hysteresis exactly where the
    // composition switched clips.
    let mut flat = RenderSession::open(&out).unwrap();
    for frame in 0..FIXTURE_FRAMES {
        assert_eq!(
            cells(flat.render(frame, GRID.0, GRID.1).unwrap()),
            played[frame as usize],
            "flattened frame {frame} must match the composition"
        );
    }
}

/// A failed export leaves nothing behind: no half-written `out` where a
/// good asset used to be, and no `.part` debris either.
#[test]
fn a_failed_export_leaves_no_debris() {
    let dir = TmpDir::new("atomic");
    let clip = dir.0.join("doomed.ascii");
    fs::copy(dir.clip(Fixture::GradientMotion), &clip).unwrap();
    let mut comp = Composition::from_clips("doomed", vec![auto_ascii::Clip::new(&clip)]);
    comp.resolve().unwrap();

    let out = dir.0.join("out.ascii");
    fs::write(&out, b"an earlier export that must survive").unwrap();

    // Corrupt a frame payload AFTER resolve: the header and FIDX still
    // open, so this fails in the middle of the frame walk — with the
    // writer already created and the part file on disk.
    let mut bytes = fs::read(&clip).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle..middle + 256].fill(0xA5);
    fs::write(&clip, &bytes).unwrap();

    let err = export(&comp, &out, &ExportOptions::default()).expect_err("must fail");
    // Decode, not Format: the header and FIDX still parsed, so the failure
    // came from the frame walk — the part file existed and was cleaned up.
    assert!(matches!(err, auto_ascii::Error::Decode { .. }), "unexpected error: {err}");
    assert_eq!(
        fs::read(&out).unwrap(),
        b"an earlier export that must survive",
        "the previous export is untouched"
    );
    assert!(!dir.0.join("out.ascii.part").exists(), "no .part debris");

    // And the same when the clip is gone entirely (it is opened before
    // anything is created, so there is nothing to clean up).
    fs::remove_file(&clip).unwrap();
    let gone = dir.0.join("gone.ascii");
    let err = export(&comp, &gone, &ExportOptions::default()).expect_err("must fail");
    assert!(matches!(err, auto_ascii::Error::Io { .. }), "unexpected error: {err}");
    assert!(!gone.exists() && !dir.0.join("gone.ascii.part").exists(), "nothing written");
}

/// A one-clip composition with a trim is `auto-ascii cut`: the slice's
/// frames are the source's frames at the offset.
#[test]
fn a_trimmed_single_clip_exports_the_slice() {
    let dir = TmpDir::new("cut");
    let mut clip = auto_ascii::Clip::new(dir.clip(Fixture::HardCut));
    clip.in_secs = 1.0;
    clip.out_secs = Some(2.0);
    let mut comp = Composition::from_clips("slice", vec![clip]);
    comp.resolve().unwrap();
    let out = dir.0.join("slice.ascii");
    let report = export(&comp, &out, &ExportOptions::default()).expect("export");
    assert_eq!(report.frames, 30);

    let mut slice = RenderSession::open(&out).unwrap();
    let mut source = RenderSession::open(dir.clip(Fixture::HardCut)).unwrap();
    assert_eq!(slice.frame_count(), 30);
    // Frame f of the slice is frame 30+f of the source (1.0 s at 30 fps).
    for f in [0u32, 1, 2] {
        let want = cells(source.render(30 + f, GRID.0, GRID.1).unwrap());
        let got = cells(slice.render(f, GRID.0, GRID.1).unwrap());
        assert_eq!(got, want, "slice frame {f} is source frame {}", 30 + f);
    }
}

#[test]
fn toml_errors_name_the_clip() {
    let dir = TmpDir::new("errors");
    let path = dir.0.join("bad.toml");
    let cases = [
        (
            "schema = 1\n[[clip]]\nasset = \"gradient-motion.ascii\"\nfrom = \"0:01\"\n",
            "clip 0: unknown key \"from\"",
        ),
        ("schema = 7\n[[clip]]\nasset = \"gradient-motion.ascii\"\n", "schema 7"),
        ("schema = 1\n[[clip]]\nasset = \"missing-clip\"\n", "clip 0: asset \"missing-clip\""),
    ];
    for (text, want) in cases {
        fs::write(&path, text).unwrap();
        let err = RenderSession::open_composition(&path, None).expect_err("must be rejected");
        let msg = err.to_string();
        assert!(msg.contains(want), "{msg:?} should contain {want:?}");
        assert!(msg.contains("bad.toml"), "the file is named: {msg:?}");
    }
}

// ---------------------------------------------------------------------------
// The player binary treats a `.toml` argument as a composition (PLAN-M6-M8
// §3). Gated like sim_e2e.rs: CARGO_BIN_EXE_* only exists with the `bin`
// feature, and without the gate the harness would reuse a stale binary.
// ---------------------------------------------------------------------------

#[cfg(feature = "bin")]
fn run_player(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_auto-ascii-player"))
        .args(args)
        .output()
        .expect("spawn auto-ascii-player");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Pull a bare (unquoted) JSON scalar out of the one-line stats report.
#[cfg(feature = "bin")]
fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let pat = format!("\"{key}\":");
    let start = json.find(&pat).unwrap_or_else(|| panic!("no {key} in {json}")) + pat.len();
    let rest = &json[start..];
    let end = rest.find([',', '}']).unwrap_or_else(|| panic!("unterminated {key} in {json}"));
    rest[..end].trim_matches('"')
}

#[cfg(feature = "bin")]
#[test]
fn sim_plays_a_composition_and_prints_the_stats_line() {
    let dir = TmpDir::new("sim");
    let comp = dir.composition_file();
    let (ok, stdout, stderr) =
        run_player(&[comp.to_str().unwrap(), "--sim", "120x40:60"]);
    assert!(ok, "player failed: {stderr}");
    let line = stdout.lines().last().expect("one JSON line");
    assert_eq!(json_field(line, "frames"), "60");
    assert_eq!(json_field(line, "grid_after"), "120x40");
    assert_eq!(json_field(line, "tier"), "truecolor");
    assert!(json_field(line, "bytes_total").parse::<u64>().unwrap() > 0);
}

/// `--seek` on a composition is composition time: 4.5 s lands 0.5 s into
/// the second clip's slice, which starts 0.5 s into that clip — so the
/// frame is byte-identical to seeking the clip alone to 1.0 s.
#[cfg(feature = "bin")]
#[test]
fn seek_lands_on_the_composition_timeline() {
    let dir = TmpDir::new("seek");
    let comp = dir.composition_file();
    let clip = dir.clip(Fixture::HardCut);
    let from_comp = dir.0.join("comp.dump");
    let from_clip = dir.0.join("clip.dump");

    let sim = |asset: &Path, seek: &str, dump: &Path| {
        let (ok, _, stderr) = run_player(&[
            asset.to_str().unwrap(),
            "--sim",
            "120x40:1",
            "--seek",
            seek,
            "--sim-dump",
            dump.to_str().unwrap(),
        ]);
        assert!(ok, "player failed: {stderr}");
        fs::read(dump).expect("dump written")
    };
    assert_eq!(
        sim(&comp, "0:04.5", &from_comp),
        sim(&clip, "0:01", &from_clip),
        "composition frame 135 must render clip B's frame 30"
    );

    // And a seek into the gap plays the gap, not a picture.
    let in_gap = sim(&comp, "0:03", &dir.0.join("gap.dump"));
    let on_clip = sim(&comp, "0:01", &dir.0.join("clipa.dump"));
    assert_ne!(in_gap, on_clip, "3 s is inside the gap");
}
