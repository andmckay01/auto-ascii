//! The compositions folder's file model (PLAN-M6-M8 §3).
//!
//! A composition is a TOML file and IS the source of truth (§0.3): agents
//! write it directly, and `compose new`/`add` are conveniences that edit
//! the same bytes. So both are TEXT operations — `new` writes a header and
//! a comment block naming the clip keys, `add` APPENDS one `[[clip]]`
//! table — and nothing here ever re-serializes a file, which is what keeps
//! an agent's (or a human's) comments and ordering intact.
//!
//! Reading is the facade's job ([`Composition::from_toml_file`]); what
//! this module adds on top is the `compose show` report: the resolved
//! timeline plus the two things a timeline has that a clip list does not —
//! gaps (nothing plays; the frames are black) and overlaps (two clips own
//! one instant; the later-listed one is on top).

use std::io::Write;
use std::path::Path;

use auto_ascii::Composition;
use serde::Serialize;

use crate::BoxErr;
use crate::library::absolute;

/// The body `compose new` writes: the two required keys, then a commented
/// `[[clip]]` table so the schema is readable in the file itself.
pub fn new_text(name: &str) -> String {
    format!(
        "schema = 1\n\
         name = {}\n\
         \n\
         # Clips play in file order; `at` places one, `in`/`out` trim it, and a gap\n\
         # plays black. This file is the source of truth: append tables by hand, or\n\
         # with `auto-ascii compose add {name} <clip> [--in T] [--out T] [--at T]`.\n\
         #\n\
         # [[clip]]\n\
         # asset = \"apple-1984\"  # a library name, or a path relative to this file\n\
         # in = \"0:05\"           # trim start inside the asset (default: its start)\n\
         # out = \"0:20\"          # trim end inside the asset (default: its end)\n\
         # at = \"0:00\"           # timeline position (default: end of the last clip)\n",
        toml_string(name)
    )
}

/// One `[[clip]]` table, blank-line separated from whatever is above it.
/// `times` are the `(key, spec)` pairs the caller was given, in schema
/// order — written back VERBATIM (the schema takes a timestamp string
/// anywhere it takes seconds), so the file says what the agent meant
/// rather than a float that re-rounds it.
pub fn clip_table(asset: &str, times: &[(&str, &str)]) -> String {
    let mut out = format!("\n[[clip]]\nasset = {}\n", toml_string(asset));
    for (key, spec) in times {
        out.push_str(&format!("{key} = {}\n", toml_string(spec)));
    }
    out
}

/// Write a new composition file, failing if one is already there. The
/// create is atomic (`create_new`), so two agents racing cannot both think
/// they started it.
pub fn create(path: &Path, name: &str) -> Result<(), BoxErr> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!(
                "composition {name:?} already exists at {} — add to it with \
                 `auto-ascii compose add {name} <clip>`",
                path.display()
            ),
            _ => format!("write {}: {e}", path.display()),
        })?;
    file.write_all(new_text(name).as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Append `table` to the composition at `path`, leaving every byte above
/// it exactly as it was. Read-then-write rather than an append handle: a
/// file that does not end in a newline gets one first, so a table can
/// never land glued to the last line someone typed.
pub fn append_clip(path: &Path, table: &str) -> Result<(), BoxErr> {
    let mut text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(table);
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Quote `s` as a TOML basic string — the form the schema is documented
/// in. Only `"`, `\` and control characters need escaping, and a clip name
/// or an absolute path holds none of them in practice; the escapes are
/// here so a path that does still produces a file that parses.
pub fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What `compose show` prints: the resolved timeline of one composition.
/// Build it with [`report`] from a RESOLVED [`Composition`].
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The composition's name (its TOML `name`, else the file stem).
    pub name: String,
    /// Composition frame rate: the highest clip fps.
    pub fps: f64,
    /// Length of the timeline, seconds: the latest clip end.
    pub duration_secs: f64,
    /// Frames on the timeline at `fps`.
    pub frame_count: u32,
    /// Every clip, in FILE order (`index` is its position in the file).
    pub clips: Vec<ClipRow>,
    /// Stretches nothing covers, in time order. These play black.
    pub gaps: Vec<Span>,
    /// Stretches two clips both cover, in time order.
    pub overlaps: Vec<Overlap>,
}

/// One clip's row: what the file says, and where it landed.
#[derive(Clone, Debug, Serialize)]
pub struct ClipRow {
    /// Position in the file, counting from 0.
    pub index: usize,
    /// The `asset` string as written.
    pub asset: String,
    /// The file it resolved to.
    pub path: String,
    /// Trim start inside the asset, seconds.
    pub in_secs: f64,
    /// Trim end inside the asset, seconds — RESOLVED, so a clip with no
    /// `out` reports the asset's own end rather than null.
    pub out_secs: f64,
    /// The `at` as written: `null` when the clip simply follows the one
    /// before it (which is what `start_secs` then reports).
    pub at_secs: Option<f64>,
    /// Start on the composition timeline, seconds (inclusive).
    pub start_secs: f64,
    /// End on the composition timeline, seconds (EXCLUSIVE).
    pub end_secs: f64,
    /// The ASSET's own frame rate, which is not the composition's unless
    /// this is the fastest clip — the column that makes a mixed-fps
    /// composition readable.
    pub fps: f64,
}

/// A `[start, end)` stretch of composition time.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Span {
    /// Start, seconds (inclusive).
    pub start_secs: f64,
    /// End, seconds (exclusive).
    pub end_secs: f64,
}

/// Where two clips both own the timeline. The later-listed clip is on top
/// (PLAN-M6-M8 §3), so `over` is what actually plays there.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Overlap {
    /// Start of the shared stretch, seconds.
    pub start_secs: f64,
    /// End of the shared stretch, seconds.
    pub end_secs: f64,
    /// The covered clip's index.
    pub under: usize,
    /// The covering clip's index — the one that plays.
    pub over: usize,
}

/// What one overlap does to a clip's row in the `compose show` table.
/// Every overlap is TWO facts — the later clip covers, the earlier one is
/// covered — and a reader needs both: a clip nothing ever shows is exactly
/// the mistake a table of start/end numbers hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// This clip covers part of clip `n`, which is listed before it.
    Over(usize),
    /// Clip `n`, listed after this one, covers part of it.
    Under(usize),
    /// Clip `n` covers ALL of it: not one frame of this clip ever plays.
    Hidden(usize),
}

/// One line of the `compose show` table, in timeline order.
pub enum Row<'a> {
    /// A clip and every overlap it takes part in, in time order.
    Clip(&'a ClipRow, Vec<Mark>),
    /// A stretch nothing covers.
    Gap(&'a Span),
}

impl Row<'_> {
    /// Where this row starts on the timeline — what the table sorts by.
    pub fn start_secs(&self) -> f64 {
        match self {
            Row::Clip(clip, _) => clip.start_secs,
            Row::Gap(gap) => gap.start_secs,
        }
    }
}

impl Report {
    /// The table's lines in TIMELINE order, gaps interleaved — file order
    /// is what `index` reports, and a composition whose `at` jumps around
    /// reads as the timeline it plays. Stable, so clips that start
    /// together stay in file order.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let mut rows: Vec<Row<'_>> = self
            .clips
            .iter()
            .map(|clip| Row::Clip(clip, self.marks(clip)))
            .chain(self.gaps.iter().map(Row::Gap))
            .collect();
        rows.sort_by(|a, b| a.start_secs().total_cmp(&b.start_secs()));
        rows
    }

    /// Every overlap `clip` takes part in, from its own side.
    fn marks(&self, clip: &ClipRow) -> Vec<Mark> {
        self.overlaps
            .iter()
            .filter_map(|o| {
                if o.over == clip.index {
                    return Some(Mark::Over(o.under));
                }
                if o.under != clip.index {
                    return None;
                }
                // Exact comparison, deliberately: the intersection's bounds
                // are this clip's own numbers when the cover swallows it
                // whole, so there is nothing for a tolerance to absorb.
                let whole = o.start_secs == clip.start_secs && o.end_secs == clip.end_secs;
                Some(if whole { Mark::Hidden(o.over) } else { Mark::Under(o.over) })
            })
            .collect()
    }
}

/// The `compose show` report for a RESOLVED composition (an unresolved one
/// has no timeline, so its report is empty rather than wrong).
pub fn report(comp: &Composition) -> Report {
    let spans = comp.timeline();
    let clips = comp
        .clips()
        .iter()
        .zip(spans)
        .enumerate()
        .map(|(index, (clip, span))| ClipRow {
            index,
            asset: clip.name.clone(),
            path: absolute(&clip.path),
            in_secs: span.in_secs,
            out_secs: span.out_secs,
            at_secs: clip.at_secs,
            start_secs: span.start_secs,
            end_secs: span.end_secs,
            fps: span.fps(),
        })
        .collect();
    // The two timeline questions are asked of the bounds alone, which is
    // all they are about — and which makes them testable without assets.
    let bounds: Vec<(f64, f64)> = spans.iter().map(|s| (s.start_secs, s.end_secs)).collect();
    Report {
        name: comp.name().to_string(),
        fps: comp.fps(),
        duration_secs: comp.duration_secs(),
        frame_count: comp.frame_count(),
        clips,
        gaps: gaps(&bounds),
        overlaps: overlaps(&bounds),
    }
}

/// The stretches of `[0, duration)` no clip covers — a sweep over the
/// spans sorted by start, which is the only order that survives an `at`
/// placing clip 3 before clip 1. There is never a trailing gap: the
/// composition ends at the latest clip end.
fn gaps(bounds: &[(f64, f64)]) -> Vec<Span> {
    let mut sorted = bounds.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut covered_to = 0.0f64;
    for (start, end) in sorted {
        if start > covered_to {
            out.push(Span { start_secs: covered_to, end_secs: start });
        }
        covered_to = covered_to.max(end);
    }
    out
}

/// Every pair of clips that share an instant, later-listed on top.
/// Quadratic in the clip count and deliberately so: it is exact for the
/// unbounded, out-of-order placements the schema allows, and `show` runs
/// once per invocation on a file a human wrote.
fn overlaps(bounds: &[(f64, f64)]) -> Vec<Overlap> {
    let mut out = Vec::new();
    for (under, a) in bounds.iter().enumerate() {
        for (over, b) in bounds.iter().enumerate().skip(under + 1) {
            let start = a.0.max(b.0);
            let end = a.1.min(b.1);
            if start < end {
                out.push(Overlap { start_secs: start, end_secs: end, under, over });
            }
        }
    }
    out.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs).then(a.over.cmp(&b.over)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_file_declares_its_schema_and_names_the_keys() {
        let text = new_text("demo");
        assert!(text.starts_with("schema = 1\nname = \"demo\"\n\n#"), "{text}");
        assert!(text.contains("auto-ascii compose add demo <clip>"), "{text}");
        for key in ["# [[clip]]", "# asset =", "# in =", "# out =", "# at ="] {
            assert!(text.contains(key), "{key} missing from:\n{text}");
        }
        assert!(text.lines().all(|l| l.len() < 80), "a line ran long:\n{text}");
    }

    #[test]
    fn a_clip_table_is_the_schema_verbatim() {
        assert_eq!(clip_table("apple-1984", &[]), "\n[[clip]]\nasset = \"apple-1984\"\n");
        assert_eq!(
            clip_table("apple-1984", &[("in", "0:05"), ("out", "0:20"), ("at", "1:00")]),
            "\n[[clip]]\nasset = \"apple-1984\"\nin = \"0:05\"\nout = \"0:20\"\nat = \"1:00\"\n"
        );
    }

    #[test]
    fn strings_are_quoted_for_toml() {
        assert_eq!(toml_string("apple-1984"), "\"apple-1984\"");
        assert_eq!(toml_string("/a/b c.ascii"), "\"/a/b c.ascii\"");
        assert_eq!(toml_string("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(toml_string("C:\\clips\\a.ascii"), "\"C:\\\\clips\\\\a.ascii\"");
        assert_eq!(toml_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(toml_string("bell\u{7}"), "\"bell\\u0007\"");
    }

    /// A report over hand-built clips: `rows()` and its marks are table
    /// logic, so they are tested without assets, like the two sweeps are.
    fn report_of(bounds: &[(f64, f64)]) -> Report {
        let clips = bounds
            .iter()
            .enumerate()
            .map(|(index, &(start, end))| ClipRow {
                index,
                asset: format!("clip-{index}"),
                path: format!("/tmp/clip-{index}.ascii"),
                in_secs: 0.0,
                out_secs: end - start,
                at_secs: Some(start),
                start_secs: start,
                end_secs: end,
                fps: 30.0,
            })
            .collect();
        Report {
            name: "t".into(),
            fps: 30.0,
            duration_secs: bounds.iter().fold(0.0f64, |acc, &(_, e)| acc.max(e)),
            frame_count: 1,
            clips,
            gaps: gaps(bounds),
            overlaps: overlaps(bounds),
        }
    }

    fn marks_of(rows: &[Row<'_>], index: usize) -> Vec<Mark> {
        rows.iter()
            .find_map(|row| match row {
                Row::Clip(clip, marks) if clip.index == index => Some(marks.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("clip {index} has no row"))
    }

    /// Both sides of an overlap are marked: the covering clip and the one
    /// being covered. A table that only marked the coverer would leave the
    /// clip you cannot see looking perfectly ordinary.
    #[test]
    fn an_overlap_marks_both_clips() {
        let report = report_of(&[(0.0, 3.0), (2.0, 4.0)]);
        let rows = report.rows();
        assert_eq!(marks_of(&rows, 0), [Mark::Under(1)]);
        assert_eq!(marks_of(&rows, 1), [Mark::Over(0)]);
    }

    /// Clip 0 at 5–6 sits entirely inside clip 1 at 0–20: not one of its
    /// frames ever plays, which is the mistake start/end columns hide.
    #[test]
    fn a_swallowed_clip_is_hidden() {
        let report = report_of(&[(5.0, 6.0), (0.0, 20.0)]);
        let rows = report.rows();
        assert_eq!(marks_of(&rows, 0), [Mark::Hidden(1)]);
        assert_eq!(marks_of(&rows, 1), [Mark::Over(0)]);
        // Timeline order, not file order: clip 1 starts first.
        match (&rows[0], &rows[1]) {
            (Row::Clip(first, _), Row::Clip(second, _)) => {
                assert_eq!((first.index, second.index), (1, 0));
            }
            _ => panic!("two clip rows, no gap"),
        }
        // Abutting ends are exclusive, so touching is not covering.
        assert!(marks_of(&report_of(&[(0.0, 2.0), (2.0, 4.0)]).rows(), 0).is_empty());
    }

    /// One clip can be on both sides of two different overlaps, and the
    /// marks come in the overlaps' own time order.
    #[test]
    fn marks_accumulate_per_clip() {
        let report = report_of(&[(0.0, 3.0), (2.0, 6.0), (1.0, 5.0)]);
        let rows = report.rows();
        assert_eq!(marks_of(&rows, 0), [Mark::Under(2), Mark::Under(1)]);
        assert_eq!(marks_of(&rows, 1), [Mark::Over(0), Mark::Under(2)]);
        assert_eq!(marks_of(&rows, 2), [Mark::Over(0), Mark::Over(1)]);
    }

    #[test]
    fn gaps_are_what_nothing_covers() {
        assert!(gaps(&[(0.0, 2.0)]).is_empty());
        // A leading gap (first clip placed past 0) and one between clips.
        let found = gaps(&[(1.0, 2.0), (4.0, 5.0)]);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start_secs, found[0].end_secs), (0.0, 1.0));
        assert_eq!((found[1].start_secs, found[1].end_secs), (2.0, 4.0));
        // Abutting clips leave nothing, and a clip swallowed by a longer
        // earlier one cannot open a gap behind it.
        assert!(gaps(&[(0.0, 2.0), (2.0, 4.0)]).is_empty());
        assert!(gaps(&[(0.0, 9.0), (1.0, 2.0)]).is_empty());
        // Out-of-order placement is sorted before the sweep.
        let found = gaps(&[(4.0, 5.0), (0.0, 1.0)]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start_secs, found[0].end_secs), (1.0, 4.0));
    }

    #[test]
    fn overlaps_name_the_clip_on_top() {
        assert!(overlaps(&[(0.0, 2.0), (2.0, 4.0)]).is_empty(), "ends are exclusive");
        let found = overlaps(&[(0.0, 3.0), (2.0, 4.0)]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start_secs, found[0].end_secs), (2.0, 3.0));
        assert_eq!((found[0].under, found[0].over), (0, 1), "the later clip is on top");
        // Three clips, two pairs: 0 under 1 and 1 under 2.
        let found = overlaps(&[(0.0, 3.0), (2.0, 6.0), (5.0, 7.0)]);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].under, found[0].over), (0, 1));
        assert_eq!((found[1].under, found[1].over), (1, 2));
    }
}
