//! Review-reel GIF encoding and HTML generation.

use auto_ascii_eval::{EdgeScore, GrayImage};

use auto_ascii_factory::ffmpeg::BoxErr;

use super::eval::{base64, html_escape};

const MIN_SIGN_OFF_ROWS: u32 = 4;
pub const REEL_ROWS: u32 = 6;
const _: () = assert!(REEL_ROWS >= MIN_SIGN_OFF_ROWS);
pub const GIF_SECS: u32 = 10;
pub const GIF_FPS: u32 = 10;

pub struct ReelRow {
    pub frame: u32,
    pub secs: f64,
    pub ssim: Option<f64>,
    pub edge: Option<EdgeScore>,
    pub flicker_to_date: Option<f64>,
    pub src_png: Vec<u8>,
    pub render_png: Vec<u8>,
}

pub struct ReelClip {
    pub name: String,
    pub fps: f64,
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub frames: u32,
    pub ssim_mean: Option<f64>,
    pub edge_f1_mean: Option<f64>,
    pub flicker: Option<f64>,
    pub gif: Vec<u8>,
    pub gif_w: u16,
    pub gif_h: u16,
    pub rows: Vec<ReelRow>,
}

const GIF_CENTISECONDS_PER_SECOND: u32 = 100;

fn gif_delay_centiseconds(fps: u32) -> u16 {
    (GIF_CENTISECONDS_PER_SECOND / fps.max(1)).max(1) as u16
}

pub fn encode_gray_gif(frames: &[GrayImage], fps: u32) -> Result<Vec<u8>, BoxErr> {
    assert!(!frames.is_empty(), "encode_gray_gif: no frames");
    let (w, h) = (frames[0].w(), frames[0].h());
    let mut palette = Vec::with_capacity(256 * 3);
    for i in 0..=255u8 {
        palette.extend([i, i, i]);
    }
    let delay = gif_delay_centiseconds(fps);
    let mut out = Vec::new();
    {
        let mut enc = gif::Encoder::new(&mut out, w, h, &palette)
            .map_err(|e| format!("gif encoder: {e}"))?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(|e| format!("gif repeat: {e}"))?;
        for img in frames {
            assert_eq!((img.w(), img.h()), (w, h), "mixed GIF frame dimensions");
            let frame = gif::Frame {
                width: w,
                height: h,
                buffer: std::borrow::Cow::Borrowed(img.as_slice()),
                delay,
                ..gif::Frame::default()
            };
            enc.write_frame(&frame).map_err(|e| format!("gif frame: {e}"))?;
        }
    }
    Ok(out)
}

fn fmt_opt(v: Option<f64>, digits: usize) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.digits$}"))
}

pub fn render_reel_html(clips: &[ReelClip], generator: &str) -> String {
    let mut h = String::with_capacity(1 << 22);
    h.push_str(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>auto-ascii review reel</title>\n<style>\n\
         body{font-family:system-ui,sans-serif;margin:2rem auto;max-width:1250px;\
         background:#14151a;color:#d8dae2;line-height:1.45}\n\
         h1{font-size:1.5rem}h2{font-size:1.2rem;border-bottom:1px solid #33363f;\
         padding-bottom:.3rem;margin-top:2.5rem}\n\
         .meta{color:#9aa0ae;font-size:.85rem}\n\
         table{border-collapse:collapse;margin:.8rem 0;font-size:.85rem;\
         font-variant-numeric:tabular-nums}\n\
         th,td{border:1px solid #33363f;padding:.35rem .6rem;vertical-align:top;\
         text-align:left}\nth{background:#1d1f26;text-align:center}\n\
         img{display:block;max-width:100%;image-rendering:pixelated}\n\
         td img{width:480px}\n\
         .gif img{width:960px;margin:.6rem 0}\n\
         .strip{font-size:.85rem;white-space:nowrap}\n\
         .strip b{color:#eaecf2}\n\
         </style>\n</head>\n<body>\n\
         <h1>auto-ascii review reel — M3 sign-off</h1>\n",
    );
    h.push_str(&format!(
        "<p class=\"meta\">{} | {} clip(s) | source | rasterized render | metrics per \
         timestamp; animated GIF = rasterized render, {GIF_SECS}s @ {GIF_FPS} fps</p>\n",
        html_escape(generator),
        clips.len()
    ));

    for clip in clips {
        h.push_str(&format!("<h2>{}</h2>\n", html_escape(&clip.name)));
        h.push_str(&format!(
            "<p class=\"meta\">{} frames @ {:.3} fps on a {}x{} grid | clip means: \
             ssim {} | edge F1 {} | flicker {} sw/cell/s</p>\n",
            clip.frames,
            clip.fps,
            clip.grid_cols,
            clip.grid_rows,
            fmt_opt(clip.ssim_mean, 4),
            fmt_opt(clip.edge_f1_mean, 4),
            fmt_opt(clip.flicker, 3),
        ));
        if !clip.gif.is_empty() {
            h.push_str(&format!(
                "<div class=\"gif\"><img alt=\"animated render of {} ({}x{} px)\" \
                 src=\"data:image/gif;base64,{}\"></div>\n",
                html_escape(&clip.name),
                clip.gif_w,
                clip.gif_h,
                base64(&clip.gif)
            ));
        }
        h.push_str(
            "<table><tr><th>frame</th><th>source (fps-normalized, base res)</th>\
             <th>rasterized render</th><th>metrics</th></tr>\n",
        );
        for row in &clip.rows {
            let edge = match &row.edge {
                None => "edge F1 <b>n/a</b>".to_string(),
                Some(s) => format!(
                    "edge F1 <b>{:.4}</b><br>precision {:.4} / recall {:.4}<br>\
                     truth {} / predicted {} cells",
                    s.f1, s.precision, s.recall, s.truth_cells, s.predicted_cells
                ),
            };
            h.push_str(&format!(
                "<tr><td>{}<br class=\"meta\">t={:.2}s</td>\
                 <td><img alt=\"source frame {}\" src=\"data:image/png;base64,{}\"></td>\
                 <td><img alt=\"render of frame {}\" src=\"data:image/png;base64,{}\"></td>\
                 <td class=\"strip\">ssim <b>{}</b><br>{}<br>flicker-to-date <b>{}</b></td></tr>\n",
                row.frame,
                row.secs,
                row.frame,
                base64(&row.src_png),
                row.frame,
                base64(&row.render_png),
                fmt_opt(row.ssim, 4),
                edge,
                fmt_opt(row.flicker_to_date, 3),
            ));
        }
        h.push_str("</table>\n");
    }
    h.push_str("</body>\n</html>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_gray(w: u16, h: u16, v: u8) -> GrayImage {
        GrayImage::from_raw(w, h, vec![v; w as usize * h as usize])
    }

    #[test]
    fn gif_delay_is_whole_centiseconds_and_never_zero() {
        assert_eq!(gif_delay_centiseconds(30), 3);
        assert_eq!(gif_delay_centiseconds(10), 10);
        assert_eq!(gif_delay_centiseconds(0), 100);
        assert_eq!(gif_delay_centiseconds(1000), 1);
    }

    #[test]
    fn gif_is_valid_and_animated() {
        let frames: Vec<GrayImage> = (0..5).map(|i| tiny_gray(8, 4, i * 40)).collect();
        let gif = encode_gray_gif(&frames, GIF_FPS).unwrap();
        assert_eq!(&gif[..6], b"GIF89a");
        assert_eq!(u16::from_le_bytes([gif[6], gif[7]]), 8);
        assert_eq!(u16::from_le_bytes([gif[8], gif[9]]), 4);
        assert!(
            gif.windows(11).any(|w| w == b"NETSCAPE2.0"),
            "missing loop extension"
        );
        let gce_count = gif.windows(2).filter(|w| w == b"\x21\xF9").count();
        assert_eq!(gce_count, 5, "one graphic control extension per frame");
    }

    #[test]
    #[should_panic(expected = "mixed GIF frame dimensions")]
    fn gif_rejects_mixed_dims() {
        let _ = encode_gray_gif(&[tiny_gray(8, 4, 0), tiny_gray(4, 8, 0)], GIF_FPS);
    }

    #[test]
    fn reel_html_is_self_contained() {
        let clip = ReelClip {
            name: "clip <&> one".into(),
            fps: 30.0,
            grid_cols: 300,
            grid_rows: 80,
            frames: 194,
            ssim_mean: Some(0.64),
            edge_f1_mean: Some(0.41),
            flicker: Some(0.8),
            gif: encode_gray_gif(&[tiny_gray(8, 4, 7)], GIF_FPS).unwrap(),
            gif_w: 8,
            gif_h: 4,
            rows: vec![
                ReelRow {
                    frame: 10,
                    secs: 0.333,
                    ssim: Some(0.6),
                    edge: Some(EdgeScore {
                        precision: 0.5,
                        recall: 0.25,
                        f1: 1.0 / 3.0,
                        truth_cells: 40,
                        predicted_cells: 20,
                    }),
                    flicker_to_date: Some(0.5),
                    src_png: b"\x89PNG fake".to_vec(),
                    render_png: b"\x89PNG fake2".to_vec(),
                },
                ReelRow {
                    frame: 90,
                    secs: 3.0,
                    ssim: None,
                    edge: None,
                    flicker_to_date: None,
                    src_png: vec![1, 2, 3],
                    render_png: vec![4, 5, 6],
                },
            ],
        };
        let html = render_reel_html(&[clip], "auto-ascii-factory test");

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("clip &lt;&amp;&gt; one"), "clip names are escaped");
        for banned in ["http://", "https://", "<link", "<script", "@import", "url("] {
            assert!(!html.contains(banned), "external ref: {banned}");
        }
        for (i, chunk) in html.split("src=\"").enumerate().skip(1) {
            assert!(chunk.starts_with("data:"), "non-data src #{i}");
        }
        assert!(html.contains("data:image/gif;base64,"));
        assert_eq!(html.matches("data:image/png;base64,").count(), 4);
        assert!(html.contains("edge F1 <b>0.3333</b>"));
        assert!(html.contains("edge F1 <b>n/a</b>"));
    }
}
