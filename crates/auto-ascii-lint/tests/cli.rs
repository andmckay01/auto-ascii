//! Isolated CLI fixtures for scope, exit status and report mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let root = workspace().join("target/comment-check-tests").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let repo = Self(root);
        repo.write("scripts/comment-allowlist.toml", "entries = []\n");
        repo
    }

    fn write(&self, path: &str, source: &str) {
        if let Some(parent) = self.0.join(path).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(self.0.join(path), source).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_check-comments"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

fn output(result: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
}

#[test]
fn tracked_untracked_and_ignored_files_obey_scope_and_exit_status() {
    let repo = Repo::new();
    repo.write(".gitignore", "ignored.rs\n");
    repo.write("ignored.rs", "// ignored\n");
    repo.write("one.rs", "//! Owner.\nfn one() {} // trailing\n");
    repo.write("two.rs", "//! Owner.\n");
    assert!(
        Command::new("git")
            .current_dir(&repo.0)
            .args(["add", "two.rs"])
            .status()
            .unwrap()
            .success()
    );
    let result = repo.run(&[]);
    assert_eq!(result.status.code(), Some(1), "{}", output(&result));
    assert!(output(&result).contains("one.rs:2:"));
    assert!(!output(&result).contains("ignored.rs:"));
    assert_eq!(repo.run(&["--paths", "two.rs"]).status.code(), Some(0));
    assert_eq!(
        repo.run(&["--paths", "one.rs", "two.rs"]).status.code(),
        Some(1)
    );
    let result = repo.run(&["--count", "--report-only"]);
    assert_eq!(result.status.code(), Some(0), "{}", output(&result));
    assert!(output(&result).contains("| **Total** | 4 | 2 | 3 | 3 |"));
    assert!(output(&result).contains("1 violation(s)"));
}

#[test]
fn exact_exemptions_fail_when_changed_removed_or_already_compliant() {
    let repo = Repo::new();
    repo.write("one.rs", "// old\n");
    repo.write(
        "scripts/comment-allowlist.toml",
        "[[entries]]\npath='one.rs'\ncomment='// old'\nreason='fixture contract'\n",
    );
    assert_eq!(repo.run(&[]).status.code(), Some(0));
    repo.write("one.rs", "// changed\n");
    let result = repo.run(&["--report-only"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(output(&result).contains("stale allowlist entry"));
    assert_eq!(repo.run(&["--paths", "scripts"]).status.code(), Some(0));
    repo.write("one.rs", "fn one() {}\n");
    assert_eq!(repo.run(&[]).status.code(), Some(1));
    std::fs::remove_file(repo.0.join("one.rs")).unwrap();
    assert_eq!(repo.run(&[]).status.code(), Some(1));
    repo.write("one.rs", "//! Owner.\n");
    repo.write(
        "scripts/comment-allowlist.toml",
        "[[entries]]\npath='one.rs'\ncomment='//! Owner.'\nreason='unneeded exemption'\n",
    );
    assert_eq!(repo.run(&[]).status.code(), Some(1));
}

#[test]
fn invalid_schema_and_syntax_fail_even_in_report_mode() {
    let repo = Repo::new();
    repo.write(
        "scripts/comment-allowlist.toml",
        "entries=[]\nunknown=true\n",
    );
    assert_eq!(repo.run(&["--report-only"]).status.code(), Some(2));
    repo.write("scripts/comment-allowlist.toml", "entries=[]\n");
    repo.write("bad.py", "x = '''unterminated\n");
    let result = repo.run(&["--report-only"]);
    assert_eq!(result.status.code(), Some(2));
    assert!(output(&result).contains("bad.py:1: comment extraction failed"));
    assert_eq!(repo.run(&["--paths"]).status.code(), Some(2));
    assert_eq!(repo.run(&["--unknown"]).status.code(), Some(2));
    assert_eq!(repo.run(&["--paths", "../outside"]).status.code(), Some(2));
}

#[test]
fn isolated_tooling_snapshot_has_no_violations() {
    let repo = Repo::new();
    for (path, source) in [
        ("Cargo.toml", include_str!("../Cargo.toml")),
        ("src/lib.rs", include_str!("../src/lib.rs")),
        ("src/main.rs", include_str!("../src/main.rs")),
        ("src/make.rs", include_str!("../src/make.rs")),
        ("src/scope.rs", include_str!("../src/scope.rs")),
        ("src/extract.rs", include_str!("../src/extract.rs")),
        ("src/rust.rs", include_str!("../src/rust.rs")),
        ("src/python.py", include_str!("../src/python.py")),
        ("tests/policy.rs", include_str!("policy.rs")),
        ("tests/cli.rs", include_str!("cli.rs")),
    ] {
        repo.write(path, source);
    }
    let unrelated = Repo::new();
    unrelated.write("review-invalid.sh", "echo 'unterminated\n");
    for args in [&["--count", "--report-only"][..], &[]] {
        let result = repo.run(args);
        assert!(result.status.success(), "{}", output(&result));
        assert!(output(&result).contains("0 violation(s)"));
    }
}

#[cfg(unix)]
#[test]
fn source_symlinks_are_refused_only_within_the_selected_scope() {
    let repo = Repo::new();
    repo.write("one.rs", "//! Owner.\n");
    std::os::unix::fs::symlink("one.rs", repo.0.join("link.rs")).unwrap();
    let result = repo.run(&[]);
    assert_eq!(result.status.code(), Some(2));
    assert!(output(&result).contains("source symlink refused"));
    assert_eq!(repo.run(&["--paths", "one.rs"]).status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn tracked_source_parent_symlinks_are_refused_before_reading() {
    let repo = Repo::new();
    let outside = Repo::new();
    repo.write("nested/deeper/probe.rs", "//! Owner.\n");
    outside.write("deeper/probe.rs", "/* unterminated");
    assert!(
        Command::new("git")
            .current_dir(&repo.0)
            .args(["add", "nested/deeper/probe.rs"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::rename(repo.0.join("nested"), repo.0.join("saved-nested")).unwrap();
    std::os::unix::fs::symlink(&outside.0, repo.0.join("nested")).unwrap();
    for args in [
        &["--paths", "nested"][..],
        &["--report-only", "--paths", "nested/deeper/probe.rs"],
    ] {
        let result = repo.run(args);
        assert_eq!(result.status.code(), Some(2), "{}", output(&result));
        assert!(output(&result).contains("source symlink refused"));
        assert!(!output(&result).contains("extraction failed"));
    }
    assert_eq!(
        repo.run(&["--paths", "saved-nested"]).status.code(),
        Some(0)
    );
}

#[cfg(unix)]
#[test]
fn allowlist_leaf_and_parent_symlinks_are_refused_even_outside_scan_scope() {
    for parent in [false, true] {
        let repo = Repo::new();
        let outside = Repo::new();
        repo.write("one.rs", "//! Owner.\n");
        outside.write("scripts/comment-allowlist.toml", "invalid TOML [[[\n");
        let path = if parent {
            "scripts"
        } else {
            "scripts/comment-allowlist.toml"
        };
        std::fs::rename(repo.0.join(path), repo.0.join("saved-allowlist")).unwrap();
        std::os::unix::fs::symlink(outside.0.join(path), repo.0.join(path)).unwrap();
        let result = repo.run(&["--report-only", "--paths", "one.rs"]);
        assert_eq!(result.status.code(), Some(2), "{}", output(&result));
        assert!(output(&result).contains("symlink refused"));
        assert!(output(&result).contains("scripts/comment-allowlist.toml"));
    }
}
