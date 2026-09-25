use auto_ascii_term::ProbeParser;

const KITTY: &[u8] = b"\x1bP>|kitty(0.32.2)\x1b\\\
\x1b[?2026;2$y\
\x1bP0+r524742\x1b\\\
\x1b[6;20;10t\
\x1b[?62;52;c";

const XTERM: &[u8] = b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP1+r524742=2D31\x1b\\\
\x1b[6;16;8t\
\x1b[?64;1;2;6;9;15;16;17;18;21;22;28c";

const VTE: &[u8] = b"\x1b[?2026;4$y\x1b[?61;1;4;21;22;28c";

#[test]
fn kitty_stream_parses_fully() {
    let mut p = ProbeParser::new();
    assert!(p.feed(KITTY), "DA1 sentinel must complete the volley");
    let r = p.replies();
    assert_eq!(r.xtversion.as_deref(), Some("kitty(0.32.2)"));
    assert_eq!(r.decrqm_2026, Some(2));
    assert!(r.sync_supported(), "Ps=2 is settable ⇒ synchronized output usable");
    assert_eq!(r.xtgettcap_rgb, Some(false), "kitty has no RGB cap: 0+r");
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
    assert!(!r.sync_supported());
    assert_eq!(r.xtgettcap_rgb, Some(false), "RGB=-1 means no direct color");
    assert_eq!(r.cell_px, Some((8, 16)));
    assert!(r.da1);
}

#[test]
fn vte_stream_partial_answers() {
    let mut p = ProbeParser::new();
    assert!(p.feed(VTE));
    let r = p.replies();
    assert_eq!(r.xtversion, None);
    assert_eq!(r.decrqm_2026, Some(4));
    assert!(!r.sync_supported(), "permanently reset (4) is NOT support");
    assert_eq!(r.xtgettcap_rgb, None);
    assert_eq!(r.cell_px, None);
    assert!(r.da1);
}

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

#[test]
fn xtgettcap_rgb_is_read_by_value() {
    let cases: &[(&[u8], Option<bool>)] = &[
        (b"\x1bP1+r524742=2D31\x1b\\", Some(false)),
        (b"\x1bP1+r524742=38\x1b\\", Some(true)),
        (b"\x1bP1+r524742=382F382F38\x1b\\", Some(true)),
        (b"\x1bP1+r524742\x1b\\", Some(true)),
        (b"\x1bP0+r524742\x1b\\", Some(false)),
        (b"\x1bP1+r544E=787465726D;524742=2D31\x1b\\", Some(false)),
        (b"\x1bP1+r524742=382f382f38\x1b\\", Some(true)),
        (b"\x1bP1+r544E=787465726D\x1b\\", None),
    ];
    for (stream, want) in cases {
        let mut p = ProbeParser::new();
        p.feed(stream);
        assert_eq!(
            p.replies().xtgettcap_rgb,
            *want,
            "stream {:?}",
            String::from_utf8_lossy(stream)
        );
    }
}

#[test]
fn garbage_tolerant() {
    let mut noisy = Vec::new();
    noisy.extend_from_slice(b"hello\r\n\x07");
    noisy.extend_from_slice(b"\x1b[?2026;1$y");
    noisy.extend_from_slice(b"\x1b]0;window title\x07");
    noisy.extend_from_slice(b"\x1b[38;5;42m");
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

#[test]
fn without_da1_never_done() {
    let mut p = ProbeParser::new();
    assert!(!p.feed(b"\x1b[?2026;2$y\x1bP>|kitty(0.32.2)\x1b\\\x1b[6;20;10t"));
    assert!(!p.done());
    assert_eq!(p.replies().decrqm_2026, Some(2));
}

#[test]
fn da1_requires_question_prefix_or_bare() {
    let mut p = ProbeParser::new();
    p.feed(b"\x1b[1;2c");
    assert!(!p.done());
    p.feed(b"\x1b[?6c");
    assert!(p.done());
}

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
