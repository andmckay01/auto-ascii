//! Queried-terminal identity rules and capability adjustments.

use crate::caps::{Caps, ColorTier};
use crate::probe::ProbeReplies;

#[derive(Clone, Copy, Debug)]
pub struct Quirk {
    pub name: &'static str,
    pub xtversion_prefix: &'static str,
    pub rgb_reply: Option<Option<bool>>,
    pub color_at_least: Option<ColorTier>,
    pub color_at_most: Option<ColorTier>,
}

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
