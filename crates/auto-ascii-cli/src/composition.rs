//! The compositions folder's file model (PLAN-M6-M8 §3).
//!
//! A composition is a TOML file and IS the source of truth (§0.3): agents
//! write it directly, and `compose new`/`add` are conveniences that edit
//! the same bytes. So both are TEXT operations — `new` writes a header and
//! a comment block naming the clip keys, `add` APPENDS one `[[clip]]`
//! table — and nothing here ever re-serializes a file, which is what keeps
//! an agent's (or a human's) comments and ordering intact. The append is
//! one `O_APPEND` write, so concurrent adds cannot overwrite each other;
//! it is not a transaction, and a process killed mid-write can still leave
//! a partial table for its author to finish.
//!
//! Reading a composition and analysing it are both the facade's job
//! ([`Composition::from_toml_file`], then `gaps`/`overlaps`/`mark_for`,
//! which work on the FRAME GRID so a 1e-16 s sliver between abutting clips
//! cannot exist). What this module adds is the `compose show` report: the
//! facade's answers in the shape the CLI prints and serializes.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use auto_ascii::{ClipMark, Composition};
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
/// it exactly as it was.
///
/// ONE `O_APPEND` write of a few dozen bytes: two agents adding clips at
/// the same moment both land, and neither can lose the other's table the
/// way a read-modify-write would. What is already in the file is read only
/// to the extent of its LAST BYTE, which is all that decides whether the
/// table needs a newline in front of it.
pub fn append_clip(path: &Path, table: &str) -> Result<(), BoxErr> {
    let mut text = String::with_capacity(table.len() + 1);
    if !ends_with_newline(path)? {
        text.push('\n');
    }
    text.push_str(table);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Whether the file already ends in a newline — an empty file counts, so
/// nothing leads with a blank line. Seeks to the last byte rather than
/// reading a composition that may be thousands of clips long.
fn ends_with_newline(path: &Path) -> Result<bool, BoxErr> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| format!("stat {}: {e}", path.display()))?.len();
    if len == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1)).map_err(|e| format!("seek {}: {e}", path.display()))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(last[0] == b'\n')
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
    /// What this clip does to earlier ones and what later ones do to it,
    /// from [`Composition::overlaps`] and [`Composition::mark_for`]. Not
    /// part of the JSON shape — `overlaps` already carries every pair, and
    /// this is the table's last column.
    #[serde(skip)]
    pub marks: Vec<Mark>,
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
    /// A clip, with the marks it already carries.
    Clip(&'a ClipRow),
    /// A stretch nothing covers.
    Gap(&'a Span),
}

impl Row<'_> {
    /// Where this row starts on the timeline — what the table sorts by.
    pub fn start_secs(&self) -> f64 {
        match self {
            Row::Clip(clip) => clip.start_secs,
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
            .map(Row::Clip)
            .chain(self.gaps.iter().map(Row::Gap))
            .collect();
        rows.sort_by(|a, b| a.start_secs().total_cmp(&b.start_secs()));
        rows
    }
}

/// The `compose show` report for a RESOLVED composition (an unresolved one
/// has no timeline, so its report is empty rather than wrong).
pub fn report(comp: &Composition) -> Report {
    // Both questions are the facade's, answered on the frame grid: two
    // clips that abut cannot report a sliver of overlap, and a gap shorter
    // than one frame cannot exist to be printed.
    let overlaps: Vec<Overlap> = comp
        .overlaps()
        .iter()
        .map(|o| Overlap {
            start_secs: o.span.start_secs,
            end_secs: o.span.end_secs,
            under: o.under,
            over: o.over,
        })
        .collect();
    let clips = comp
        .clips()
        .iter()
        .zip(comp.timeline())
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
            marks: marks_for(comp, index, &overlaps),
        })
        .collect();
    Report {
        name: comp.name().to_string(),
        fps: comp.fps(),
        duration_secs: comp.duration_secs(),
        frame_count: comp.frame_count(),
        clips,
        gaps: comp
            .gaps()
            .iter()
            .map(|g| Span { start_secs: g.start_secs, end_secs: g.end_secs })
            .collect(),
        overlaps,
    }
}

/// The table's last column for one clip: what it covers, then how much of
/// it survives. The verdict is [`Composition::mark_for`] — whether a clip
/// is merely under something or never seen at all is a question about
/// FRAMES, and summing covered stretches is how it is answered; the first
/// overlap that covers this clip names the culprit for the reader.
fn marks_for(comp: &Composition, index: usize, overlaps: &[Overlap]) -> Vec<Mark> {
    let mut marks: Vec<Mark> =
        overlaps.iter().filter(|o| o.over == index).map(|o| Mark::Over(o.under)).collect();
    let culprit = overlaps.iter().find(|o| o.under == index).map(|o| o.over);
    match (comp.mark_for(index), culprit) {
        (Some(ClipMark::Partial), Some(over)) => marks.push(Mark::Under(over)),
        (Some(ClipMark::Hidden), Some(over)) => marks.push(Mark::Hidden(over)),
        _ => {}
    }
    marks
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

    /// Rows are the report's own rows plus its gaps, in TIMELINE order —
    /// the only thing left in this module once the facade answers what a
    /// gap and an overlap are. Built by hand: no composition, no assets.
    fn row(index: usize, start: f64, end: f64, marks: Vec<Mark>) -> ClipRow {
        ClipRow {
            index,
            asset: format!("clip-{index}"),
            path: format!("/tmp/clip-{index}.ascii"),
            in_secs: 0.0,
            out_secs: end - start,
            at_secs: Some(start),
            start_secs: start,
            end_secs: end,
            fps: 30.0,
            marks,
        }
    }

    #[test]
    fn rows_are_the_timeline_in_order() {
        let report = Report {
            name: "t".into(),
            fps: 30.0,
            duration_secs: 9.0,
            frame_count: 270,
            // File order puts the later clip first; the table must not.
            clips: vec![
                row(0, 6.0, 9.0, vec![Mark::Over(1)]),
                row(1, 0.0, 2.0, vec![Mark::Under(0)]),
            ],
            gaps: vec![Span { start_secs: 2.0, end_secs: 6.0 }],
            overlaps: Vec::new(),
        };
        let rows = report.rows();
        assert_eq!(rows.len(), 3);
        match (&rows[0], &rows[1], &rows[2]) {
            (Row::Clip(first), Row::Gap(gap), Row::Clip(last)) => {
                assert_eq!(first.index, 1);
                assert_eq!(first.marks, [Mark::Under(0)]);
                assert_eq!((gap.start_secs, gap.end_secs), (2.0, 6.0));
                assert_eq!(last.index, 0);
            }
            _ => panic!("clip, gap, clip"),
        }
    }
}
