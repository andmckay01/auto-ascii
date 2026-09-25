use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let dirs = [manifest.join("src"), manifest.join("../auto-ascii-format/src")];

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (tag, dir) in ["auto-ascii-factory", "auto-ascii-format"].iter().zip(&dirs) {
        let start = files.len();
        collect_rs(dir, dir, &mut files);
        for (rel, _) in &mut files[start..] {
            *rel = format!("{tag}/{rel}");
        }
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
