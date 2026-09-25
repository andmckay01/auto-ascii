use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use auto_ascii::{ClipMark, Composition};
use serde::Serialize;

use crate::BoxErr;
use crate::library::absolute;

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

pub fn clip_table(asset: &str, times: &[(&str, &str)]) -> String {
    let mut out = format!("\n[[clip]]\nasset = {}\n", toml_string(asset));
    for (key, spec) in times {
        out.push_str(&format!("{key} = {}\n", toml_string(spec)));
    }
    out
}

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

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub name: String,
    pub fps: f64,
    pub duration_secs: f64,
    pub frame_count: u32,
    pub clips: Vec<ClipRow>,
    pub gaps: Vec<Span>,
    pub overlaps: Vec<Overlap>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClipRow {
    pub index: usize,
    pub asset: String,
    pub path: String,
    pub in_secs: f64,
    pub out_secs: f64,
    pub at_secs: Option<f64>,
    pub start_secs: f64,
    pub end_secs: f64,
    pub fps: f64,
    #[serde(skip)]
    pub marks: Vec<Mark>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Span {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Overlap {
    pub start_secs: f64,
    pub end_secs: f64,
    pub under: usize,
    pub over: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Over(usize),
    Under(usize),
    Hidden(usize),
}

pub enum Row<'a> {
    Clip(&'a ClipRow),
    Gap(&'a Span),
}

impl Row<'_> {
    pub fn start_secs(&self) -> f64 {
        match self {
            Row::Clip(clip) => clip.start_secs,
            Row::Gap(gap) => gap.start_secs,
        }
    }
}

impl Report {
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

pub fn report(comp: &Composition) -> Report {
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
