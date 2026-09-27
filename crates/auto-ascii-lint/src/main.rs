//! Worktree comment policy command and Markdown count report.

use anyhow::{Context, Result, bail};
use auto_ascii_lint::{Allowlist, language, path_metadata, scan, violation, worktree_files};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

#[derive(Default)]
struct Counts {
    files: usize,
    commented: usize,
    clusters: usize,
    lines: usize,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        self.files += other.files;
        self.commented += other.commented;
        self.clusters += other.clusters;
        self.lines += other.lines;
    }
    fn print(&self, area: &str) {
        println!(
            "| {area} | {} | {} | {} | {} |",
            self.files, self.commented, self.clusters, self.lines
        );
    }
}

fn run() -> Result<bool> {
    let mut count = false;
    let mut report = false;
    let mut prefixes = Vec::new();
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--count" => count = true,
            "--report-only" => report = true,
            "--paths" => {
                let before = prefixes.len();
                while args.peek().is_some_and(|a| !a.starts_with("--")) {
                    let prefix = args.next().unwrap();
                    let prefix = prefix.trim_start_matches("./").trim_end_matches('/');
                    if prefix.is_empty() || prefix.split('/').any(|p| p == ".." || p.is_empty()) {
                        bail!("--paths requires repository-relative file or directory prefixes");
                    }
                    prefixes.push(prefix.to_owned());
                }
                if prefixes.len() == before {
                    bail!("--paths needs at least one prefix");
                }
            }
            "--help" | "-h" => {
                println!(
                    "check-comments [--paths <file-or-directory>...] [--count] [--report-only]"
                );
                println!(
                    "Violations exit 1; extraction/configuration failures exit 2 even in report mode."
                );
                return Ok(true);
            }
            _ => bail!("unknown argument: {arg}"),
        }
    }
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        bail!("run check-comments inside a Git worktree");
    }
    let root = PathBuf::from(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
    path_metadata(&root, "scripts/comment-allowlist.toml")?
        .context("scripts/comment-allowlist.toml is missing")?;
    let allowlist = Allowlist::parse(&std::fs::read_to_string(
        root.join("scripts/comment-allowlist.toml"),
    )?)?;
    let mut used = BTreeSet::new();
    let mut violations = Vec::new();
    let mut areas: BTreeMap<String, Counts> = BTreeMap::new();
    for path in worktree_files(&root, &prefixes)? {
        let lang = language(&path).unwrap();
        let source = std::fs::read_to_string(root.join(&path))
            .with_context(|| format!("{path}:1: source read failed"))?;
        let comments =
            scan(&source, lang).with_context(|| format!("{path}:1: comment extraction failed"))?;
        let area = if path.starts_with("crates/") {
            path.split('/').nth(1).unwrap()
        } else {
            "(root/tools)"
        };
        let counts = areas.entry(area.to_owned()).or_default();
        counts.files += 1;
        counts.commented += usize::from(!comments.is_empty());
        counts.clusters += comments.len();
        counts.lines += comments
            .iter()
            .map(|c| c.end_line - c.line + 1)
            .sum::<usize>();
        for c in comments {
            if let Some(reason) = violation(&source, &c, lang)
                && !allowlist.exempt(&path, &source[c.start..c.end], &mut used)
            {
                violations.push(format!("{path}:{}: {reason}", c.line));
            }
        }
    }
    let stale = allowlist.stale(&used, &prefixes);
    if count {
        println!("| Area | Scanned files | Commented files | Clusters | Comment lines |");
        println!("|---|---:|---:|---:|---:|---:|");
        let mut total = Counts::default();
        for (area, counts) in &areas {
            counts.print(area);
            total.add(counts);
        }
        total.print("**Total**");
        println!();
    } else {
        for message in &violations {
            println!("{message}");
        }
    }
    for message in &stale {
        println!("{message}");
    }
    println!(
        "check-comments: {} violation(s), {} stale allowlist entries{}",
        violations.len(),
        stale.len(),
        if report { " (report-only)" } else { "" }
    );
    Ok(stale.is_empty() && (report || violations.is_empty()))
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("check-comments:1: {e:#}");
            ExitCode::from(2)
        }
    }
}
