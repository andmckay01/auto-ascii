use std::time::Instant;

use auto_ascii::Composition;
use auto_ascii::deck::ClipDeck;
use auto_ascii_term::{Backend, SimBackend};

use crate::BoxErr;

const COLS: u16 = 300;
const ROWS: u16 = 80;

pub fn run(comp: &Composition, mut deck: ClipDeck, seeks: u32) -> Result<(), BoxErr> {
    if seeks == 0 {
        return Err("--bench-seek needs N >= 1".into());
    }
    let mut backend = SimBackend::new(COLS, ROWS);
    backend.resize(COLS, ROWS);
    deck.set_size(COLS, ROWS);
    deck.present_at(&mut backend, comp.locate_frame(0))?;
    backend.take_output();

    let frames = u64::from(comp.frame_count());
    let mut lat_ms: Vec<f64> = Vec::with_capacity(seeks as usize);
    let mut rng: u64 = 0x5EED_F00D_D15C_0B01;
    for _ in 0..seeks {
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        let frame = ((rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) % frames) as u32;
        let t = Instant::now();
        deck.reset_active();
        deck.present_at(&mut backend, comp.locate_frame(frame))?;
        lat_ms.push(t.elapsed().as_secs_f64() * 1e3);
        backend.take_output();
    }
    lat_ms.sort_by(|a, b| a.partial_cmp(b).expect("latencies are finite"));
    let pick = |q: f64| lat_ms[((lat_ms.len() - 1) as f64 * q).round() as usize];
    outln!(
        "{{\"seeks\":{seeks},\"grid\":\"{COLS}x{ROWS}\",\"frames\":{frames},\
         \"p50_ms\":{:.2},\"p95_ms\":{:.2},\"max_ms\":{:.2}}}",
        pick(0.50),
        pick(0.95),
        lat_ms[lat_ms.len() - 1],
    );
    Ok(())
}
