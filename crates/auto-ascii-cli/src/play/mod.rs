//! `play`, `compose play`, `dev sim` and `dev bench-seek`: resolve the target,
//! then hand it to the terminal player, or run it headlessly against a
//! simulated terminal. Returns how playback stopped so `main` can map it.

mod bench;
mod sim;

use std::path::{Path, PathBuf};

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::color_depth;
use auto_ascii::{Composition, PaletteChoice, Stopped};
use auto_ascii_term::{Caps, ColorTier};

use crate::BoxErr;
use crate::args::{BenchArgs, PlayOptions, PlaybackArgs, RepaintArg, SimArgs, TerminalArgs};
use crate::home::{Home, Target};
use crate::library;

pub const PLAY_IS_INTERACTIVE: &str =
    "play is interactive; run it without --json (or add --sim for one JSON stats line)";

pub struct Request<'a> {
    pub playback: &'a PlaybackArgs,
    pub terminal: &'a TerminalArgs,
    pub sim: Option<&'a SimArgs>,
    pub bench_seek: Option<u32>,
}

impl<'a> Request<'a> {
    pub fn of(opts: &'a PlayOptions) -> Request<'a> {
        Request {
            playback: &opts.playback,
            terminal: &opts.terminal,
            sim: opts.sim.sim.is_some().then_some(&opts.sim),
            bench_seek: opts.bench.bench_seek,
        }
    }

    pub fn sim(playback: &'a PlaybackArgs, terminal: &'a TerminalArgs, sim: &'a SimArgs) -> Request<'a> {
        Request { playback, terminal, sim: Some(sim), bench_seek: None }
    }

    pub fn bench(
        playback: &'a PlaybackArgs,
        terminal: &'a TerminalArgs,
        bench: &'a BenchArgs,
    ) -> Request<'a> {
        Request { playback, terminal, sim: None, bench_seek: bench.bench_seek }
    }

    fn headless(&self) -> bool {
        self.sim.is_some() || self.bench_seek.is_some()
    }
}

pub fn play(spec: &str, req: &Request<'_>, json: bool) -> Result<Stopped, BoxErr> {
    refuse_json(req, json)?;
    let target = match explicit(spec) {
        Some(target) => target,
        None => Home::resolve()?.resolve_playable(spec)?,
    };
    run(target, req)
}

pub fn play_composition(spec: &str, req: &Request<'_>, json: bool) -> Result<Stopped, BoxErr> {
    refuse_json(req, json)?;
    let path = if Path::new(spec).is_file() {
        PathBuf::from(spec)
    } else {
        Home::resolve()?.resolve_composition(spec)?
    };
    run(Target::Composition(path), req)
}

fn refuse_json(req: &Request<'_>, json: bool) -> Result<(), BoxErr> {
    if json && !req.headless() {
        return Err(PLAY_IS_INTERACTIVE.into());
    }
    Ok(())
}

fn explicit(spec: &str) -> Option<Target> {
    let path = Path::new(spec);
    path.is_file().then(|| {
        if Composition::is_toml_path(path) {
            Target::Composition(path.to_path_buf())
        } else {
            Target::Clip(path.to_path_buf())
        }
    })
}

fn run(target: Target, req: &Request<'_>) -> Result<Stopped, BoxErr> {
    let seek_secs = req
        .playback
        .seek
        .as_deref()
        .map(|ts| auto_ascii::timecode::parse(ts).map_err(|e| format!("--seek {ts:?}: {e}")))
        .transpose()?;
    if let Target::Clip(path) = &target {
        library::asset_info(path)?;
    }
    if req.headless() {
        headless(&target, req, seek_secs)?;
        return Ok(Stopped::Ended);
    }
    interactive(&target, req, seek_secs)
}

fn interactive(target: &Target, req: &Request<'_>, seek_secs: Option<f64>) -> Result<Stopped, BoxErr> {
    let (pb, term) = (req.playback, req.terminal);
    let builder = auto_ascii::Player::builder();
    let builder = match target {
        Target::Clip(path) => builder.asset(path),
        Target::Composition(path) => builder.composition(path),
    };
    let mut builder = builder
        .palette(pb.palette.into())
        .tier(term.tier)
        .repaint(term.repaint.into())
        .looping(pb.loop_playback)
        .no_query(term.no_query)
        .no_quirks(term.no_quirks)
        .no_cache(term.no_cache)
        .no_backdrop(term.no_backdrop)
        .mute(pb.mute)
        .no_audio(pb.no_audio);
    if let Some(cap) = term.fps_cap {
        builder = builder.fps_cap(cap);
    }
    if let Some(a) = term.cell_aspect {
        builder = builder.cell_aspect(a);
    }
    if let Some(secs) = seek_secs {
        builder = builder.seek_secs(secs);
    }
    if let Some(dur) = term.duration_secs {
        builder = builder.duration_secs(dur);
    }
    if let Some(spec) = &term.font_table {
        builder = builder.font_table(spec.as_str());
    }
    if let Some(style) = pb.style {
        builder = builder.style(style);
    }
    Ok(builder.build()?.play()?)
}

fn headless(target: &Target, req: &Request<'_>, seek_secs: Option<f64>) -> Result<(), BoxErr> {
    let (pb, term) = (req.playback, req.terminal);
    let (path, is_comp) = match target {
        Target::Clip(path) => (path, false),
        Target::Composition(path) => (path, true),
    };
    let comp = open_timeline(path, is_comp)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    let start_frame = match seek_secs {
        Some(secs) => comp.frame_at_secs(secs)?,
        None => 0,
    };
    let tier = req.sim.and_then(|s| s.sim_tier).or(term.tier).unwrap_or(ColorTier::True);
    let mut glyphs = PaletteChoice::from(pb.palette).resolve_for_caps(&Caps::default());
    if let Some(spec) = &term.font_table {
        glyphs = auto_ascii::load_font_table(spec)?.veto_tier(glyphs);
    }
    let mut deck = ClipDeck::new(
        comp.clips().iter().map(|c| c.path.clone()).collect(),
        DeckConfig {
            cell_aspect: term.cell_aspect.unwrap_or(auto_ascii_core::DEFAULT_CELL_ASPECT),
            repaint_full: term.repaint == RepaintArg::Full,
            color: color_depth(tier),
            glyph_tier: glyphs,
        },
    );
    deck.set_style(pb.style.unwrap_or_default());
    match (req.sim, req.bench_seek) {
        (_, Some(seeks)) => bench::run(&comp, deck, seeks),
        (Some(sim), None) => sim::run(sim::Run {
            comp: &comp,
            deck,
            sim,
            playback: pb,
            tier,
            start_frame,
            single_clip: !is_comp,
        }),
        (None, None) => unreachable!("headless() checked one of them"),
    }
}

fn open_timeline(path: &Path, is_comp: bool) -> Result<Composition, BoxErr> {
    let mut comp = if is_comp {
        Composition::from_toml_file(path, Composition::default_library_dir().as_deref())
            .map_err(|e| format!("reading composition {}: {e}", path.display()))?
    } else {
        Composition::single(path)
    };
    comp.resolve()?;
    Ok(comp)
}
