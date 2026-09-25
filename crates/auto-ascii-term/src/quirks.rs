//! Terminal quirk table, keyed on **queried identity** — never on `TERM`.
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
//! Every entry is pinned end-to-end by the pty identity fixtures in
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

/// One identity-keyed capability adjustment. Deliberately data, not code: a
/// quirk is a pattern on the volley replies plus a bounded caps adjustment.
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

/// The static quirk table.
///
/// - `kitty-rgbless-xtgettcap`: kitty's XTGETTCAP tables carry `Tc` but no
///   `RGB`, so the `RGB` query gets the invalid-reply form `0+r…`
///   (kitty/terminfo.py), yet kitty is unconditionally truecolor (its docs
///   and its own `COLORTERM=truecolor` export). In a COLORTERM-stripped launch
///   (sudo, `env -i`) the queried identity raises the tier to truecolor. It
///   fires only on that reply shape, so a kitty that grows a real `RGB`
///   answer leaves it inert.
/// - `xterm-no-direct-color`: plain xterm answers `RGB` with `-1`, "no direct
///   color" (xterm/misc.c `xtermGetTcap`), and approximates SGR 38;2 into its
///   256-color palette. A globally exported `COLORTERM=truecolor` would still
///   claim truecolor; the answering terminal wins and the tier is clamped to
///   256. xterm in direct-color mode replies a real channel width, so the
///   `rgb_reply` key keeps this entry away from it.
///
/// Both entries are pinned by the pty fixtures in `tests/terminal_identity.rs`.
pub const QUIRKS: &[Quirk] = &[
    Quirk {
        name: "kitty-rgbless-xtgettcap",
        xtversion_prefix: "kitty(",
        rgb_reply: Some(Some(false)),
        color_at_least: Some(ColorTier::True),
        color_at_most: None,
    },
    Quirk {
        name: "xterm-no-direct-color",
        xtversion_prefix: "XTerm(",
        rgb_reply: Some(Some(false)),
        color_at_least: None,
        color_at_most: Some(ColorTier::C256),
    },
];

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

    #[test]
    fn kitty_without_colorterm_upgrades_to_truecolor() {
        let mut c = caps(ColorTier::C256);
        let applied = apply_quirks(&mut c, &replies("kitty(0.42.2)", Some(false)));
        assert_eq!(c.color, ColorTier::True);
        assert_eq!(applied, ["kitty-rgbless-xtgettcap"]);
        assert!(c.sync_2026);
        assert_eq!(c.cell_px, Some((10, 20)));
    }

    #[test]
    fn kitty_with_real_rgb_reply_is_not_a_quirk() {
        let mut c = caps(ColorTier::True);
        assert!(apply_quirks(&mut c, &replies("kitty(9.0)", Some(true))).is_empty());
        assert_eq!(c.color, ColorTier::True);
    }

    #[test]
    fn xterm_rgb_denial_clamps_passive_truecolor_claim() {
        let mut c = caps(ColorTier::True);
        let applied = apply_quirks(&mut c, &replies("XTerm(390)", Some(false)));
        assert_eq!(c.color, ColorTier::C256);
        assert_eq!(applied, ["xterm-no-direct-color"]);

        let mut c = caps(ColorTier::C16);
        apply_quirks(&mut c, &replies("XTerm(390)", Some(false)));
        assert_eq!(c.color, ColorTier::C16);
    }

    #[test]
    fn xterm_direct_color_is_not_clamped() {
        let mut c = caps(ColorTier::True);
        assert!(apply_quirks(&mut c, &replies("XTerm(390)", Some(true))).is_empty());
        assert_eq!(c.color, ColorTier::True);
    }

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
