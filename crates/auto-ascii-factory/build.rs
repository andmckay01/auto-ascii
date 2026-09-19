//! Compile-time pipeline source fingerprint (M2 review fix).
//!
//! `auto-ascii-factory eval` caches built corpus assets keyed by
//! `(input sha, build-params sha, PIPELINE_FINGERPRINT)`. The third
//! component is emitted here: an FNV-1a 64 hash over every `.rs` file in
//! this crate's `src/` and auto-ascii-format's `src/` (the two crates whose code
//! determines asset bytes — extract/shots/lut/build stages and the ASCI
//! writer). Any code change therefore invalidates the eval cache; without
//! this, an M3 pipeline change would silently reuse assets built by M2 code
//! and eval would measure the old pipeline. Over-invalidation (an edit to
//! the eval driver itself) just costs a rebuild — the safe direction.
//!
//! Deterministic: files are hashed in sorted relative-path order with their
//! path and length mixed in; no timestamps, no host state.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let dirs = [manifest.join("src"), manifest.join("../auto-ascii-format/src")];

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (tag, dir) in ["auto-ascii-factory", "auto-ascii-format"].iter().zip(&dirs) {
        let start = files.len();
        collect_rs(dir, dir, &mut files);
        for (rel, _) in &mut files[start..] {
            *rel = format!("{tag}/{rel}"); // disambiguate the two src roots
        }
        // Directory-level rerun: new/removed files retrigger the hash.
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    files.sort();

    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fnv = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (rel, path) in &files {
        let data = std::fs::read(path)
            .unwrap_or_else(|e| panic!("build.rs: read {}: {e}", path.display()));
        fnv(rel.as_bytes());
        fnv(&(data.len() as u64).to_le_bytes());
        fnv(&data);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=ASCII_PIPELINE_FINGERPRINT={h:016x}");
}

/// Recursively collect `.rs` files under `dir`, keyed by path relative to
/// `root` (checkout-location independent).
fn collect_rs(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("build.rs: read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(root)
                .expect("path is under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
}
