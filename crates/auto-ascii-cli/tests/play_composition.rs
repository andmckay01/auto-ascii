use std::fs;
use std::path::{Path, PathBuf};

use auto_ascii_eval::fixtures::{Fixture, build_fixture};

struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> TmpDir {
        let dir = std::env::temp_dir()
            .join(format!("auto-ascii-play-composition-{}-{tag}", std::process::id()));
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

fn run_player(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_auto-ascii"))
        .env("AUTO_ASCII_HOME", std::env::temp_dir().join("auto-ascii-play-composition-no-home"))
        .arg("play")
        .args(args)
        .output()
        .expect("spawn auto-ascii play");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let pat = format!("\"{key}\":");
    let start = json.find(&pat).unwrap_or_else(|| panic!("no {key} in {json}")) + pat.len();
    let rest = &json[start..];
    let end = rest.find([',', '}']).unwrap_or_else(|| panic!("unterminated {key} in {json}"));
    rest[..end].trim_matches('"')
}

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

    let in_gap = sim(&comp, "0:03", &dir.0.join("gap.dump"));
    let on_clip = sim(&comp, "0:01", &dir.0.join("clipa.dump"));
    assert_ne!(in_gap, on_clip, "3 s is inside the gap");
}
