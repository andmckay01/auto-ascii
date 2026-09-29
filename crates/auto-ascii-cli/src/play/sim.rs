use std::io::Write as _;
use std::time::{Duration, Instant};

use auto_ascii::Composition;
use auto_ascii::audio::{SinkChoice, SoundOptions, Soundtrack};
use auto_ascii::deck::ClipDeck;
use auto_ascii_term::{Backend, ColorTier, Event, SimBackend};

use crate::BoxErr;
use crate::args::{PlaybackArgs, SimArgs};

pub struct Run<'a> {
    pub comp: &'a Composition,
    pub deck: ClipDeck,
    pub sim: &'a SimArgs,
    pub playback: &'a PlaybackArgs,
    pub tier: ColorTier,
    pub start_frame: u32,
    pub single_clip: bool,
}

pub fn run(run: Run<'_>) -> Result<(), BoxErr> {
    let Run { comp, mut deck, sim, playback, tier, start_frame, single_clip } = run;
    let spec = sim.sim.expect("sim::run requires --sim");
    let (cols, rows, nframes) = (spec.cols, spec.rows, spec.frames);

    let mut backend = SimBackend::new(cols, rows);
    let mut caps = backend.caps().clone();
    caps.color = tier;
    backend.set_caps(caps);
    backend.resize(cols, rows);
    deck.set_size(cols, rows);

    let mut dump = sim
        .sim_dump
        .as_ref()
        .map(|p| std::fs::File::create(p).map_err(|e| format!("creating {}: {e}", p.display())))
        .transpose()?;

    deck.enable_layer_mask();
    let mut layer_counts = [0u64; 5];

    let fps = comp.fps();
    let frame_count = u64::from(comp.frame_count());
    let start_secs = f64::from(start_frame) / fps;
    let (mut sound, open_notes) = if sim.sim_audio && single_clip {
        let opening = Soundtrack::open(
            &comp.clips()[0].path,
            comp.duration_secs(),
            SoundOptions { no_audio: playback.no_audio, muted: playback.mute, looping: true },
            SinkChoice::null(),
        );
        (opening.soundtrack, opening.notes)
    } else {
        (None, Vec::new())
    };
    let paced_from = Instant::now();
    if let Some(st) = sound.as_mut() {
        st.seek(start_secs, paced_from);
        st.set_running(true, paced_from);
    }
    let tick = Duration::from_secs_f64(1.0 / fps);
    let mut clock_secs = start_secs;

    let resize_at = nframes / 2;
    let mut bytes_total: u64 = 0;
    let mut rendered: u64 = 0;
    let t0 = Instant::now();
    for i in 0..nframes {
        if let Some((rc, rr)) = sim.sim_resize
            && i == resize_at
        {
            backend.push_event(Event::Resize(rc, rr));
        }
        if deck.drain_events(&mut backend).quit {
            break;
        }
        let frame_idx = if sim.sim_audio {
            let now = Instant::now();
            clock_secs = match sound.as_mut() {
                Some(st) => {
                    st.poll(now);
                    st.now_secs(now)
                }
                None => start_secs + now.duration_since(paced_from).as_secs_f64(),
            };
            (comp.frame_after(0, clock_secs + 1e-6) % frame_count) as u32
        } else {
            ((u64::from(start_frame) + i) % frame_count) as u32
        };
        let stats = deck.present_at(&mut backend, comp.locate_frame(frame_idx))?;
        bytes_total += u64::from(stats.bytes);
        if let Some(mask) = deck.layer_mask() {
            for &l in mask.as_slice() {
                if let Some(c) = layer_counts.get_mut(l as usize) {
                    *c += 1;
                }
            }
        }
        let out = backend.take_output();
        if let Some(f) = dump.as_mut() {
            f.write_all(&out)?;
        }
        rendered += 1;
        if sim.sim_audio {
            let due = paced_from + tick * (i as u32 + 1);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
    }
    let wall_s = t0.elapsed().as_secs_f64();
    let audio = sim.sim_audio.then(|| audio_json(sound.as_ref(), clock_secs, wall_s));
    let notes: Vec<String> = open_notes
        .into_iter()
        .chain(sound.take().map(Soundtrack::finish).unwrap_or_default())
        .collect();

    let fps = if wall_s > 0.0 { rendered as f64 / wall_s } else { 0.0 };
    let avg = if rendered > 0 { bytes_total as f64 / rendered as f64 } else { 0.0 };
    let ms = |ns: u64| ns as f64 / 1e6;
    let (gc, gr) = backend.caps().cells;
    let stage = deck.stage();
    outln!(
        "{{\"fps\":{fps:.2},\"frames\":{rendered},\"bytes_total\":{bytes_total},\
         \"avg_bytes_per_frame\":{avg:.1},\"tier\":\"{}\",\"stage_ms\":{{\"decode\":{:.1},\
         \"resample\":{:.1},\"compose\":{:.1},\"present\":{:.1}}},\
         \"layers\":{{\"base\":{},\"edge\":{},\"highlight\":{},\"shadow\":{},\
         \"structure\":{}}},\"grid_after\":\"{gc}x{gr}\"{}}}",
        tier_tag(tier),
        ms(stage.decode),
        ms(stage.resample),
        ms(stage.compose),
        ms(stage.present),
        layer_counts[0],
        layer_counts[1],
        layer_counts[2],
        layer_counts[3],
        layer_counts[4],
        audio.map(|a| format!(",\"audio\":{a}")).unwrap_or_default(),
    );
    for note in &notes {
        crate::output::emit_err(&format!("auto-ascii: sound: {note}\n"));
    }
    Ok(())
}

fn tier_tag(t: ColorTier) -> &'static str {
    match t {
        ColorTier::True => "truecolor",
        ColorTier::C256 => "256",
        ColorTier::C16 => "16",
        ColorTier::Mono => "mono",
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn audio_json(sound: Option<&Soundtrack>, clock_secs: f64, wall_s: f64) -> String {
    let Some(st) = sound else {
        return format!(
            "{{\"sound\":\"none\",\"clock\":\"wall\",\"clock_secs\":{clock_secs:.3},\"wall_secs\":{wall_s:.3}}}"
        );
    };
    let s = st.stats();
    format!(
        "{{\"sound\":\"{}\",\"clock\":\"{}\",\"source\":{},\"device\":{},\"rate\":{},\
         \"channels\":{},\"decoded_secs\":{:.3},\"decoded\":{},\"callbacks\":{},\"underruns\":{},\
         \"clock_secs\":{clock_secs:.3},\"wall_secs\":{wall_s:.3}}}",
        st.sound().label(),
        s.clock,
        json_str(&s.source.display().to_string()),
        json_str(&s.device),
        s.rate,
        s.channels,
        s.decoded_secs,
        s.decoded,
        s.callbacks,
        s.underruns,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_json_strings_are_escaped() {
        assert_eq!(json_str(r#"a "b" \ c"#), r#""a \"b\" \\ c""#);
        assert_eq!(json_str("tab\there"), "\"tab\\u0009here\"");
    }
}
