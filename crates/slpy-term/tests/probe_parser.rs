//! Probe reply parser against canned terminal reply streams (M1 acceptance
//! 4): kitty / xterm / vte transcripts, chunked delivery, garbage tolerance.

use slpy_term::ProbeParser;

/// kitty answers everything: XTVERSION, DECRPM 2026 (reset-but-settable),
/// XTGETTCAP RGB valid, cell pixel size, DA1 sentinel.
const KITTY: &[u8] = b"\x1bP>|kitty(0.32.2)\x1b\\\
\x1b[?2026;2$y\
\x1bP1+r524742=38\x1b\\\
\x1b[6;20;10t\
\x1b[?62;c";

/// xterm: XTVERSION yes, DECRPM 0 (mode not recognized), XTGETTCAP invalid
/// (`0+r`), cell size, long DA1.
const XTERM: &[u8] = b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP0+r\x1b\\\
\x1b[6;16;8t\
\x1b[?64;1;2;6;9;15;16;17;18;21;22;28c";

/// vte (gnome-terminal): no XTVERSION/XTGETTCAP replies at all, DECRPM 2026
/// supported, no cell-size reply, DA1 answers.
const VTE: &[u8] = b"\x1b[?2026;2$y\x1b[?65;1;9c";

#[test]
fn kitty_stream_parses_fully() {
    let mut p = ProbeParser::new();
    assert!(p.feed(KITTY), "DA1 sentinel must complete the volley");
    let r = p.replies();
    assert_eq!(r.xtversion.as_deref(), Some("kitty(0.32.2)"));
    assert_eq!(r.decrqm_2026, Some(2));
    assert_eq!(r.xtgettcap_rgb, Some(true));
    assert_eq!(r.cell_px, Some((10, 20)), "CSI 6;height;width t → (w,h)");
    assert!(r.da1);
}

#[test]
fn xterm_stream_parses_fully() {
    let mut p = ProbeParser::new();
    assert!(p.feed(XTERM));
    let r = p.replies();
    assert_eq!(r.xtversion.as_deref(), Some("XTerm(390)"));
    assert_eq!(r.decrqm_2026, Some(0), "mode not recognized");
    assert_eq!(r.xtgettcap_rgb, Some(false), "0+r is an invalid-cap reply");
    assert_eq!(r.cell_px, Some((8, 16)));
    assert!(r.da1);
}

#[test]
fn vte_stream_partial_answers() {
    let mut p = ProbeParser::new();
    assert!(p.feed(VTE));
    let r = p.replies();
    assert_eq!(r.xtversion, None);
    assert_eq!(r.decrqm_2026, Some(2));
    assert_eq!(r.xtgettcap_rgb, None);
    assert_eq!(r.cell_px, None);
    assert!(r.da1);
}

/// Replies arrive in arbitrary read-chunk boundaries — byte-at-a-time must
/// parse identically to one big feed.
#[test]
fn byte_at_a_time_chunking() {
    for stream in [KITTY, XTERM, VTE] {
        let mut whole = ProbeParser::new();
        whole.feed(stream);
        let mut trickle = ProbeParser::new();
        let mut done = false;
        for &b in stream {
            done = trickle.feed(&[b]);
        }
        assert!(done);
        assert_eq!(trickle.replies(), whole.replies());
    }
}

/// Stray user keystrokes, C0 controls and unknown sequences interleaved with
/// the replies must not derail parsing.
#[test]
fn garbage_tolerant() {
    let mut noisy = Vec::new();
    noisy.extend_from_slice(b"hello\r\n\x07");
    noisy.extend_from_slice(b"\x1b[?2026;1$y");
    noisy.extend_from_slice(b"\x1b]0;window title\x07"); // OSC skipped
    noisy.extend_from_slice(b"\x1b[38;5;42m"); // unrelated CSI ignored
    noisy.extend_from_slice(b"\x1bP>|foo(1.0)\x1b\\");
    noisy.extend_from_slice(b"more junk");
    noisy.extend_from_slice(b"\x1b[?1;2c");

    let mut p = ProbeParser::new();
    assert!(p.feed(&noisy));
    let r = p.replies();
    assert_eq!(r.decrqm_2026, Some(1));
    assert_eq!(r.xtversion.as_deref(), Some("foo(1.0)"));
    assert!(r.da1);
}

/// No DA1 → not done, whatever else arrived (the deadline is the only other
/// exit; the caller falls back to conservative defaults).
#[test]
fn without_da1_never_done() {
    let mut p = ProbeParser::new();
    assert!(!p.feed(b"\x1b[?2026;2$y\x1bP>|kitty(0.32.2)\x1b\\\x1b[6;20;10t"));
    assert!(!p.done());
    assert_eq!(p.replies().decrqm_2026, Some(2));
}

/// A DA1-looking reply must actually be DA1 (`CSI ? … c`), not any CSI
/// ending in `c` with parameters that aren't a device attribute report.
#[test]
fn da1_requires_question_prefix_or_bare() {
    let mut p = ProbeParser::new();
    p.feed(b"\x1b[1;2c"); // not a DA1 reply shape
    assert!(!p.done());
    p.feed(b"\x1b[?6c");
    assert!(p.done());
}

/// Oversized sequences are consumed (terminator honored) but dropped.
#[test]
fn oversized_sequence_dropped_not_wedged() {
    let mut p = ProbeParser::new();
    let mut big = Vec::new();
    big.extend_from_slice(b"\x1bP>|");
    big.extend(std::iter::repeat_n(b'x', 5000));
    big.extend_from_slice(b"\x1b\\");
    big.extend_from_slice(b"\x1b[?62;c");
    assert!(p.feed(&big));
    assert_eq!(p.replies().xtversion, None, "overlong reply must be dropped");
    assert!(p.replies().da1, "parser must recover after the oversized DCS");
}
