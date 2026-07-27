//! Terminal quirk table, keyed on **queried identity** — never on `TERM`
//! (PLAN §3.1, M5 item C).
//!
//! `TERM` describes what the user (or a launcher, or a stale profile)
//! *claims*; the XTVERSION/DA1 replies describe the terminal that actually
//! answered the volley. Where a terminal's replies are known to under- or
//! over-state its real capabilities, a small static entry here corrects the
//! probed [`Caps`] *after* the volley — and only then, because a quirk match
//! requires the queried identity, which cache hits, `--no-query` runs and
//! silent terminals never have. `--no-quirks` (`ProbeOptions::no_quirks`)
//! skips the table entirely.
//!
//! Every entry cites the terminal's own source or documentation; the same
//! research is pinned end-to-end by the pty identity fixtures in
//! `tests/terminal_identity.rs`.
//!
//! Quirks that need no table entry, handled structurally elsewhere: VTE
//! answers DECRQM 2026 with `4` ("permanently reset" — vte/src/vteseq.cc
//! `Terminal::DECRQM_DEC`, vte/src/modes.py `CONTOUR_BATCHED_RENDERING`),
//! which [`crate::probe::ProbeReplies::sync_supported`] already reads as
//! unsupported; and VTE answers `CSI 14/18/19 t` but not our `CSI 16 t`
//! cell-size query, so its cell size correctly falls through to the
//! `TIOCGWINSZ` pixel fields VTE fills in (vte/src/pty.cc `Pty::set_size`:
//! `ws_xpixel = ws_col * cell_width_px`). Neither depends on identifying
//! VTE, so neither belongs in an identity-keyed table.

use crate::caps::{Caps, ColorTier};
use crate::probe::ProbeReplies;

/// One identity-keyed capability adjustment (PLAN §3.1 "quirk table keyed on
/// queried identity"). Deliberately data, not code: a quirk is a pattern on
/// the volley replies plus a bounded caps adjustment.
#[derive(Clone, Copy, Debug)]
pub struct Quirk {
    /// Short stable name, reported by [`apply_quirks`] for diagnostics.
    pub name: &'static str,
    /// Matches when the XTVERSION reply text starts with this (e.g.
    /// `"kitty("` against `kitty(0.42.2)`). XTVERSION is the strongest
    /// identity signal our volley collects; terminals that do not answer it
    /// (alacritty, VTE, the Linux console) can never match — by design,
    /// their identity was not *queried*.
    pub xtversion_prefix: &'static str,
    /// Additionally require the XTGETTCAP `RGB` reading to be exactly this
    /// (`None` = don't care). Lets an entry key on the *combination* of who
    /// answered and what it said about direct color.
    pub rgb_reply: Option<Option<bool>>,
    /// Raise the color tier to at least this (identity proves more than the
    /// replies could).
    pub color_at_least: Option<ColorTier>,
    /// Clamp the color tier to at most this (identity disproves what passive
    /// hints claimed).
    pub color_at_most: Option<ColorTier>,
}

/// The static quirk table (M5 item C: real, sourced entries only).
pub const QUIRKS: &[Quirk] = &[
    // kitty cannot prove truecolor through the query path: its XTGETTCAP
    // tables carry the tmux-style `Tc` boolean but no `RGB` entry, so our
    // `+q524742` query gets the invalid-reply form `0+r…` back
    // (kitty/terminfo.py: `bool_capabilities` lists `Tc`, unknown names fall
    // through to `0+r<name>`; fixture `tests/terminal_identity.rs::KITTY`).
    // kitty itself is unconditionally truecolor — "kitty supports true
    // color" is unqualified in its own docs (sw.kovidgoyal.net/kitty/conf/,
    // and kitty exports COLORTERM=truecolor for exactly this reason). In a
    // COLORTERM-stripped launch (sudo, `env -i`, session managers that
    // whitelist env) the passive path therefore leaves a truecolor terminal
    // at the conservative 256 tier; the queried identity corrects it.
    Quirk {
        name: "kitty-rgbless-xtgettcap",
        xtversion_prefix: "kitty(",
        // Fire only on the researched reply shape (`0+r` → Some(false)); if
        // a future kitty grows a real RGB answer the quirk goes inert.
        rgb_reply: Some(Some(false)),
        color_at_least: Some(ColorTier::True),
        color_at_most: None,
    },
    // xterm's RGB handling, the other way around: plain xterm ANSWERS the
    // valid XTGETTCAP form `1+r524742=` with the value "-1" — "no direct
    // color" (xterm/misc.c `xtermGetTcap`: `if (TScreenOf(xw)->direct_color
    // && xw->has_rgb) {…} else unparseputs(xw, "-1")`) — and renders any
    // SGR 38;2 it receives by approximating into its 256-color palette
    // (xterm ctlseqs, "direct color" notes; fixture
    // `tests/terminal_identity.rs::XTERM`). A globally exported
    // `COLORTERM=truecolor` (a common .bashrc lie) would still promote the
    // passive tier to True — but the terminal that actually answered said
    // it has no direct color, and the queried evidence wins: clamp to 256,
    // which is both what xterm can display and where our own quantizer
    // produces better output than xterm's nearest-match approximation.
    // xterm in direct-color mode replies a real channel width ("8"), so
    // `rgb_reply` keeps this entry away from `xterm-direct` sessions.
    Quirk {
        name: "xterm-no-direct-color",
        xtversion_prefix: "XTerm(",
        rgb_reply: Some(Some(false)),
        color_at_least: None,
        color_at_most: Some(ColorTier::C256),
    },
];

/// Color-tier ordering shared with the cache's no-downgrade rule.
fn rank(t: ColorTier) -> u8 {
    match t {
        ColorTier::Mono => 0,
        ColorTier::C16 => 1,
        ColorTier::C256 => 2,
        ColorTier::True => 3,
    }
}

/// Apply every matching table entry to `caps`, returning the names of the
/// quirks that matched (diagnostics; empty for unknown/unqueried
/// identities). Called by `probe_caps` after the volley's reply upgrades and
/// before the forced `--tier` override (which still wins) — never on cache
/// hits or `--no-query` runs, and never when `ProbeOptions::no_quirks` is
/// set.
pub fn apply_quirks(caps: &mut Caps, replies: &ProbeReplies) -> Vec<&'static str> {
    let mut applied = Vec::new();
    let Some(version) = replies.xtversion.as_deref() else {
        return applied;
    };
    for q in QUIRKS {
        if !version.starts_with(q.xtversion_prefix) {
            continue;
        }
        if let Some(want) = q.rgb_reply
            && replies.xtgettcap_rgb != want
        {
            continue;
        }
        if let Some(min) = q.color_at_least
            && rank(caps.color) < rank(min)
        {
            caps.color = min;
        }
        if let Some(max) = q.color_at_most
            && rank(caps.color) > rank(max)
        {
            caps.color = max;
        }
        applied.push(q.name);
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{GlyphFlags, GlyphSupportTier};

    fn caps(color: ColorTier) -> Caps {
        Caps {
            color,
            glyphs: GlyphFlags::ASCII,
            glyph_support: GlyphSupportTier::UnicodeCore,
            sync_2026: true,
            cells: (80, 24),
            cell_px: Some((10, 20)),
            can_query: true,
        }
    }

    fn replies(xtversion: &str, rgb: Option<bool>) -> ProbeReplies {
        ProbeReplies {
            xtversion: Some(xtversion.to_owned()),
            xtgettcap_rgb: rgb,
            da1: true,
            ..ProbeReplies::default()
        }
    }

    /// The kitty entry: a COLORTERM-stripped kitty session (passive said
    /// C256, kitty answered `0+r` to the RGB query) is upgraded to True by
    /// its queried identity.
    #[test]
    fn kitty_without_colorterm_upgrades_to_truecolor() {
        let mut c = caps(ColorTier::C256);
        let applied = apply_quirks(&mut c, &replies("kitty(0.42.2)", Some(false)));
        assert_eq!(c.color, ColorTier::True);
        assert_eq!(applied, ["kitty-rgbless-xtgettcap"]);
        // Nothing else is touched.
        assert!(c.sync_2026);
        assert_eq!(c.cell_px, Some((10, 20)));
    }

    /// Future-proofing: if kitty ever answers the RGB query for real
    /// (`Some(true)`), the entry must go inert — the normal reply upgrade
    /// already handles it.
    #[test]
    fn kitty_with_real_rgb_reply_is_not_a_quirk() {
        let mut c = caps(ColorTier::True);
        assert!(apply_quirks(&mut c, &replies("kitty(9.0)", Some(true))).is_empty());
        assert_eq!(c.color, ColorTier::True);
    }

    /// The xterm entry: XTerm answered the *valid* RGB form with "-1" ("no
    /// direct color", xterm/misc.c) — a passive COLORTERM=truecolor lie is
    /// clamped back to the 256 colors xterm can actually show.
    #[test]
    fn xterm_rgb_denial_clamps_passive_truecolor_claim() {
        let mut c = caps(ColorTier::True); // as if COLORTERM=truecolor in .bashrc
        let applied = apply_quirks(&mut c, &replies("XTerm(390)", Some(false)));
        assert_eq!(c.color, ColorTier::C256);
        assert_eq!(applied, ["xterm-no-direct-color"]);

        // Already at or below the clamp: untouched (never an upgrade).
        let mut c = caps(ColorTier::C16);
        apply_quirks(&mut c, &replies("XTerm(390)", Some(false)));
        assert_eq!(c.color, ColorTier::C16);
    }

    /// xterm in direct-color mode answers a real channel width → the clamp
    /// must NOT fire (the `rgb_reply` key keeps the entry away).
    #[test]
    fn xterm_direct_color_is_not_clamped() {
        let mut c = caps(ColorTier::True);
        assert!(apply_quirks(&mut c, &replies("XTerm(390)", Some(true))).is_empty());
        assert_eq!(c.color, ColorTier::True);
    }

    /// Unknown identities and unanswered XTVERSION never match anything —
    /// the table is keyed on QUERIED identity by construction.
    #[test]
    fn unqueried_or_unknown_identity_is_untouched() {
        let mut c = caps(ColorTier::C256);
        assert!(apply_quirks(&mut c, &ProbeReplies::default()).is_empty());
        assert!(apply_quirks(&mut c, &replies("foot(1.16.2)", Some(true))).is_empty());
        assert!(
            apply_quirks(&mut c, &replies("WezTerm 20240203-110809-5046fc22", Some(true)))
                .is_empty()
        );
        assert_eq!(c.color, ColorTier::C256);
    }
}
